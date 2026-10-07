//! Delivery to a destination.

use ductus_core::InterfaceConfig;
use ductus_core::config::{AckMode, Destination, DestinationConfig, OnReject};
use ductus_core::format::Format;
use ductus_core::resources::{Api, SftpServer};
use ductus_hl7::Message;
use std::path::PathBuf;
use std::time::Duration;

pub enum SendError {
    /// Worth retrying: connection refused, timeout, AE, HTTP 5xx/408/429.
    Transient(String),
    /// Retrying won't help: AR (unless configured to retry), other HTTP 4xx. Quarantined.
    Permanent(String),
}

pub enum Sender {
    Mllp { client: ductus_mllp::Client, ack: AckMode, on_reject: OnReject },
    File { dir: PathBuf, extension: &'static str },
    Http { client: reqwest::Client, method: reqwest::Method, url: String, headers: reqwest::header::HeaderMap },
    Sftp { connection: Box<crate::sftp::Connection>, dir: String, extension: &'static str },
}

/// Content-Type for a format's serialized output.
pub fn content_type(format: &Format) -> &'static str {
    match format {
        Format::Json => "application/json",
        Format::Xml => "application/xml",
        Format::Csv(_) => "text/csv",
        Format::Hl7 => "x-application/hl7-v2+er7",
        Format::Text | Format::Fixed(_) => "text/plain; charset=utf-8",
    }
}

/// File extension for a format's serialized output.
fn extension(format: &Format) -> &'static str {
    match format {
        Format::Hl7 => "hl7",
        Format::Json => "json",
        Format::Xml => "xml",
        Format::Csv(_) => "csv",
        Format::Text | Format::Fixed(_) => "txt",
    }
}

impl Sender {
    /// A sender for one of `config`'s destinations. `server`: an SFTP destination's server;
    /// `api`: an HTTP destination's API; both from resources.yaml.
    pub fn new(
        config: &InterfaceConfig,
        destination: &DestinationConfig,
        server: Option<SftpServer>,
        api: Option<Api>,
    ) -> Self {
        let output = config.output_for(destination);
        match &destination.connector {
            Destination::Sftp { server: name, dir } => Sender::Sftp {
                connection: Box::new(crate::sftp::Connection::new(name, server.expect("checked at startup"))),
                dir: dir.clone(),
                extension: extension(output),
            },
            Destination::Mllp { host, port, timeout_ms, ack, on_reject } => Sender::Mllp {
                client: ductus_mllp::Client::new(format!("{host}:{port}"), Duration::from_millis(*timeout_ms)),
                ack: *ack,
                on_reject: *on_reject,
            },
            Destination::File { dir } => Sender::File { dir: config.dir.join(dir), extension: extension(output) },
            Destination::Http { url, api: _, path, method, headers, timeout_ms } => {
                let mut map = reqwest::header::HeaderMap::new();
                map.insert(
                    reqwest::header::CONTENT_TYPE,
                    reqwest::header::HeaderValue::from_static(content_type(output)),
                );
                // The API's headers first, so the destination's own win.
                let api_headers = api.iter().flat_map(|a| a.headers.iter());
                let url = match (&api, url) {
                    (Some(a), _) => a.url_for(path.as_deref().unwrap_or("")),
                    (None, Some(u)) => u.clone(),
                    (None, None) => unreachable!("validated: an HTTP destination has `url` or `api`"),
                };
                let timeout = timeout_ms
                    .map(Duration::from_millis)
                    .or(api.as_ref().and_then(|a| a.timeout))
                    .unwrap_or(Duration::from_secs(10));
                for (k, v) in api_headers.chain(headers) {
                    match (k.parse::<reqwest::header::HeaderName>(), v.parse::<reqwest::header::HeaderValue>()) {
                        (Ok(k), Ok(v)) => {
                            map.insert(k, v);
                        }
                        _ => tracing::error!(interface = %config.name, header = %k, "invalid HTTP header ignored"),
                    }
                }
                Sender::Http {
                    client: reqwest::Client::builder().timeout(timeout).build().expect("building HTTP client"),
                    method: method.to_ascii_uppercase().parse().unwrap_or(reqwest::Method::POST),
                    url,
                    headers: map,
                }
            }
        }
    }

    pub async fn send(&mut self, id: i64, payload: &[u8]) -> Result<(), SendError> {
        match self {
            Sender::Mllp { client, ack: AckMode::None, .. } => {
                client.send_no_ack(payload).await.map_err(|e| SendError::Transient(e.to_string()))
            }
            Sender::Mllp { client, ack: AckMode::Wait, on_reject } => {
                let ack = client.send(payload).await.map_err(|e| SendError::Transient(e.to_string()))?;
                let ack = String::from_utf8_lossy(&ack);
                let ack = Message::parse(&ack).map_err(|e| SendError::Transient(format!("unreadable ACK: {e}")))?;
                match ack.value("MSA-1") {
                    "AA" | "CA" => Ok(()),
                    "AR" | "CR" => {
                        let error = format!("rejected (AR): {}", ack.value("MSA-3"));
                        Err(match on_reject {
                            OnReject::Quarantine => SendError::Permanent(error),
                            OnReject::Retry => SendError::Transient(error),
                        })
                    }
                    code => Err(SendError::Transient(format!("{code}: {}", ack.value("MSA-3")))),
                }
            }
            Sender::File { dir, extension } => {
                let write = async {
                    tokio::fs::create_dir_all(&*dir).await?;
                    // Write then rename, so readers never see a partial file.
                    let tmp = dir.join(format!(".{id}.tmp"));
                    tokio::fs::write(&tmp, payload).await?;
                    tokio::fs::rename(&tmp, dir.join(format!("{id}.{extension}"))).await
                };
                write.await.map_err(|e| SendError::Transient(e.to_string()))
            }
            Sender::Sftp { connection, dir, extension } => {
                connection.put(dir, &format!("{id}.{extension}"), payload).await.map_err(|e| {
                    connection.reset();
                    SendError::Transient(format!("{e:#}"))
                })
            }
            Sender::Http { client, method, url, headers } => {
                let response = client
                    .request(method.clone(), url.as_str())
                    .headers(headers.clone())
                    .body(payload.to_vec())
                    .send()
                    .await
                    .map_err(|e| SendError::Transient(e.to_string()))?;
                let status = response.status();
                if status.is_success() {
                    return Ok(());
                }
                let body: String = response.text().await.unwrap_or_default().chars().take(500).collect();
                let error = format!("HTTP {status}: {body}");
                if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
                    Err(SendError::Transient(error))
                } else {
                    Err(SendError::Permanent(error))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers /charges with the headers it got; /slow takes 2 s.
    async fn server() -> String {
        use axum::routing::post;
        let app = axum::Router::new()
            .route(
                "/v2/charges",
                post(|h: axum::http::HeaderMap| async move {
                    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
                    if get("x-key") == "k1" && get("x-mine") == "dest" && get("x-both") == "dest" {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::BAD_REQUEST
                    }
                }),
            )
            .route("/v2/slow", post(|| async { tokio::time::sleep(Duration::from_secs(2)).await }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn http_destinations_send_through_an_api() {
        let base = server().await;
        let resources: ductus_core::Resources = serde_yaml_ng::from_str(&format!(
            "apis:\n  billing: {{ url: '{base}/v2', headers: {{ X-Key: k1, X-Both: api }}, timeout: 300ms }}\n"
        ))
        .unwrap();
        let config = |path: &str| -> InterfaceConfig {
            serde_yaml_ng::from_str(&format!(
                "name: x\nsource: {{ mllp: {{ port: 1 }} }}\ndestination: {{ http: {{ api: billing, path: {path}, headers: {{ X-Mine: dest, X-Both: dest }} }} }}\n"
            ))
            .unwrap()
        };
        let api = resources.apis.get("billing").cloned();
        let send = |config: InterfaceConfig| {
            let d = config.destination.clone().unwrap();
            let mut sender = Sender::new(&config, &d, None, api.clone());
            async move { sender.send(1, b"MSH|").await }
        };
        // The API's URL and headers, the destination's headers winning where both set one.
        assert!(send(config("/charges")).await.is_ok(), "the server checks the headers");
        // The API's timeout applies when the destination has none.
        let start = std::time::Instant::now();
        assert!(matches!(send(config("/slow")).await, Err(SendError::Transient(_))), "a timeout retries");
        assert!(start.elapsed() < Duration::from_secs(1), "gave up after {:?}, not 300ms", start.elapsed());
    }
}
