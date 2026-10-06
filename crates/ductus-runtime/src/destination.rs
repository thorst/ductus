//! Delivery to a destination.

use ductus_core::InterfaceConfig;
use ductus_core::config::{AckMode, Destination, OnReject};
use ductus_core::format::Format;
use ductus_core::resources::SftpServer;
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
    Mllp {
        client: ductus_mllp::Client,
        ack: AckMode,
        on_reject: OnReject,
    },
    File {
        dir: PathBuf,
        extension: &'static str,
    },
    Http {
        client: reqwest::Client,
        method: reqwest::Method,
        url: String,
        headers: reqwest::header::HeaderMap,
    },
    Sftp {
        connection: crate::sftp::Connection,
        dir: String,
        extension: &'static str,
    },
    /// Respond-mode HTTP interfaces deliver nothing.
    None,
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
    /// `server`: an SFTP destination's server, from resources.yaml.
    pub fn new(config: &InterfaceConfig, server: Option<SftpServer>) -> Self {
        match &config.destination {
            Some(Destination::Sftp { server: name, dir }) => Sender::Sftp {
                connection: crate::sftp::Connection::new(name, server.expect("checked at startup")),
                dir: dir.clone(),
                extension: extension(config.output.as_ref().unwrap_or(&config.format)),
            },
            None => Sender::None,
            Some(Destination::Mllp { host, port, timeout_ms, ack, on_reject }) => Sender::Mllp {
                client: ductus_mllp::Client::new(format!("{host}:{port}"), Duration::from_millis(*timeout_ms)),
                ack: *ack,
                on_reject: *on_reject,
            },
            Some(Destination::File { dir }) => Sender::File {
                dir: config.dir.join(dir),
                extension: extension(config.output.as_ref().unwrap_or(&config.format)),
            },
            Some(Destination::Http { url, method, headers, timeout_ms }) => {
                let output = config.output.as_ref().unwrap_or(&config.format);
                let mut map = reqwest::header::HeaderMap::new();
                map.insert(
                    reqwest::header::CONTENT_TYPE,
                    reqwest::header::HeaderValue::from_static(content_type(output)),
                );
                for (k, v) in headers {
                    match (k.parse::<reqwest::header::HeaderName>(), v.parse::<reqwest::header::HeaderValue>()) {
                        (Ok(k), Ok(v)) => {
                            map.insert(k, v);
                        }
                        _ => tracing::error!(interface = %config.name, header = %k, "invalid HTTP header ignored"),
                    }
                }
                Sender::Http {
                    client: reqwest::Client::builder()
                        .timeout(Duration::from_millis(*timeout_ms))
                        .build()
                        .expect("building HTTP client"),
                    method: method.to_ascii_uppercase().parse().unwrap_or(reqwest::Method::POST),
                    url: url.clone(),
                    headers: map,
                }
            }
        }
    }

    pub async fn send(&mut self, id: i64, payload: &[u8]) -> Result<(), SendError> {
        match self {
            Sender::None => Ok(()),
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
