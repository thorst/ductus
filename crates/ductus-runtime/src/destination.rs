//! Delivery to a destination.

use ductus_core::InterfaceConfig;
use ductus_core::config::{AckMode, Destination, DestinationConfig, OnReject};
use ductus_core::format::Format;
use ductus_core::resources::{Api, SftpServer};
use ductus_hl7::Message;
use std::path::PathBuf;
use std::sync::Arc;
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
    /// `api`: an HTTP destination's API; both from resources.yaml. `tls`: the client settings for
    /// an MLLP destination over TLS, or an HTTPS one naming a CA or client certificate (from
    /// `Certificates::client`). `None` for a route to another interface: nothing is sent, the
    /// worker stores the messages there (`Intake::accept_routed`).
    pub fn new(
        config: &InterfaceConfig,
        destination: &DestinationConfig,
        server: Option<SftpServer>,
        api: Option<Api>,
        tls: Option<Arc<rustls::ClientConfig>>,
    ) -> Option<Self> {
        let output = config.output_for(destination);
        Some(match &destination.connector {
            Destination::Interface(_) => return None,
            Destination::Sftp { server: name, dir } => Sender::Sftp {
                connection: Box::new(crate::sftp::Connection::new(name, server.expect("checked at startup"))),
                dir: dir.clone(),
                extension: extension(output),
            },
            Destination::Mllp { host, port, timeout_ms, ack, on_reject, tls: _ } => {
                let timeout = Duration::from_millis(*timeout_ms);
                let client = match tls {
                    Some(tls) => crate::tls::mllp_client(host, *port, timeout, tls),
                    None => ductus_mllp::Client::new(format!("{host}:{port}"), timeout),
                };
                Sender::Mllp { client, ack: *ack, on_reject: *on_reject }
            }
            Destination::File { dir } => Sender::File { dir: config.dir.join(dir), extension: extension(output) },
            Destination::Http { url, api: _, path, method, headers, timeout_ms, .. } => {
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
                    client: crate::tls::http_client(tls.as_ref(), reqwest::Client::builder().timeout(timeout))
                        .expect("building HTTP client"),
                    method: method.to_ascii_uppercase().parse().unwrap_or(reqwest::Method::POST),
                    url,
                    headers: map,
                }
            }
        })
    }

    pub async fn send(&mut self, id: i64, payload: &[u8]) -> Result<(), SendError> {
        match self {
            Sender::Mllp { client, ack: AckMode::None, .. } => client.send_no_ack(payload).await.map_err(mllp_error),
            Sender::Mllp { client, ack: AckMode::Wait, on_reject } => {
                let ack = client.send(payload).await.map_err(mllp_error)?;
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

/// A failed MLLP send: worth retrying, said plainly when TLS refused it.
fn mllp_error(e: std::io::Error) -> SendError {
    SendError::Transient(crate::tls::outbound_failure(&e).unwrap_or_else(|| e.to_string()))
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
            let mut sender = Sender::new(&config, &d, None, api.clone(), None).unwrap();
            async move { sender.send(1, b"MSH|").await }
        };
        // The API's URL and headers, the destination's headers winning where both set one.
        assert!(send(config("/charges")).await.is_ok(), "the server checks the headers");
        // The API's timeout applies when the destination has none.
        let start = std::time::Instant::now();
        assert!(matches!(send(config("/slow")).await, Err(SendError::Transient(_))), "a timeout retries");
        assert!(start.elapsed() < Duration::from_secs(1), "gave up after {:?}, not 300ms", start.elapsed());
    }

    use crate::tls::testing::{ca, connecting, receiver_and_client, receiving};
    use crate::tls::{Certificates, testing::Ca};
    use ductus_core::tls::{ConnectTls, ListenTls};

    /// What a send came to, in words (`SendError` has none of its own).
    fn outcome(r: Result<(), SendError>) -> Result<(), String> {
        r.map_err(|e| match e {
            SendError::Transient(e) => format!("transient: {e}"),
            SendError::Permanent(e) => format!("permanent: {e}"),
        })
    }

    /// An MLLP receiver over TLS (asking senders what `tls` asks) that answers AA; its port.
    async fn tls_receiver(certificates: &Certificates, tls: &ListenTls) -> u16 {
        let acceptor = tokio_rustls::TlsAcceptor::from(certificates.server(tls, &[]).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let open = move |tcp, peer| {
            let acceptor = acceptor.clone();
            async move { crate::tls::handshake(&acceptor, "test", tcp, peer).await }
        };
        let ack =
            |_m: bytes::Bytes| async { Some(bytes::Bytes::from_static(b"MSH|^~\\&|R||||||ACK|1|P|2.5\rMSA|AA|1\r")) };
        tokio::spawn(ductus_mllp::serve_with(listener, open, ack, ductus_mllp::CancellationToken::new()));
        port
    }

    /// Send one message to an MLLP destination on `port` with `tls`, waiting for the ACK or not.
    async fn send_mllp(
        certificates: &Certificates,
        port: u16,
        tls: Option<ConnectTls>,
        ack: &str,
    ) -> Result<(), String> {
        let config: InterfaceConfig = serde_yaml_ng::from_str(&format!(
            "name: x\nsource: {{ mllp: {{ port: 1 }} }}\ndestination: {{ mllp: {{ host: localhost, port: {port}, timeout_ms: 2000, ack: {ack} }} }}\n"
        ))
        .unwrap();
        let d = config.destination.clone().unwrap();
        let tls = tls.map(|t| certificates.client(&t).unwrap());
        let mut sender = Sender::new(&config, &d, None, None, tls).unwrap();
        outcome(sender.send(1, b"MSH|^~\\&|A||||||ADT^A01|1|P|2.5\r").await)
    }

    #[tokio::test]
    async fn mllp_over_tls_with_a_private_ca_and_a_client_certificate() {
        let (server_ca, client_ca): (Ca, Ca) = (ca("receiver's CA"), ca("clients' CA"));
        let certificates = receiver_and_client(&server_ca, &client_ca);
        let port = tls_receiver(&certificates, &receiving(true)).await;
        let ours = connecting(Some("server-ca"), Some("client"));
        assert_eq!(send_mllp(&certificates, port, Some(ours.clone()), "wait").await, Ok(()));
        assert_eq!(send_mllp(&certificates, port, Some(ours.clone()), "none").await, Ok(()), "without an ACK too");

        // Refusals are temporary (the message waits) and say why.
        let refused = |tls: ConnectTls| {
            let certificates = &certificates;
            async move { send_mllp(certificates, port, Some(tls), "wait").await.unwrap_err() }
        };
        let err = refused(connecting(None, Some("client"))).await;
        assert!(err.starts_with("transient: TLS: the receiver's certificate isn't from a CA we trust"), "{err}");
        let err = refused(connecting(Some("client-ca"), Some("client"))).await;
        assert!(err.contains("isn't from a CA we trust"), "the wrong CA: {err}");
        let err = refused(connecting(Some("server-ca"), None)).await;
        assert!(err.starts_with("transient: TLS: the receiver wants a client certificate"), "none presented: {err}");
        let err = refused(connecting(Some("server-ca"), Some("stranger"))).await;
        assert!(err.starts_with("transient: TLS: the receiver refused our client certificate"), "{err}");

        // A receiver that asks for no client certificate takes one anyway.
        let port = tls_receiver(&certificates, &receiving(false)).await;
        assert_eq!(send_mllp(&certificates, port, Some(ours), "wait").await, Ok(()));
        assert_eq!(send_mllp(&certificates, port, Some(connecting(Some("server-ca"), None)), "wait").await, Ok(()));
    }

    /// A lab receiver whose certificate was made as a CA's (`openssl req -x509` does by default):
    /// accepted when that exact certificate is named in `ca:`, on every OS (nuances.md "A CA's
    /// certificate as a server's"); one a trusted CA issued is refused, in words that say so.
    #[tokio::test]
    async fn a_receiver_presenting_a_ca_certificate_is_accepted_only_when_trusted_itself() {
        let as_ca = |params: &mut rcgen::CertificateParams| {
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        };
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        as_ca(&mut params);
        let lab = params.self_signed(&key).unwrap();
        let issuer = ca("receiver's CA");
        let issued_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        as_ca(&mut params);
        let issued = params.signed_by(&issued_key, &issuer.issuer).unwrap();
        let ca_entry = |pem: String| ductus_core::resources::CertificateEntry { ca: Some(pem), ..Default::default() };
        let entries = std::collections::BTreeMap::from([
            ("lab".to_string(), crate::tls::testing::entry(&lab.pem(), &key.serialize_pem())),
            ("lab-itself".to_string(), ca_entry(lab.pem())),
            ("issued".to_string(), crate::tls::testing::entry(&issued.pem(), &issued_key.serialize_pem())),
            ("issuer".to_string(), ca_entry(issuer.cert.pem())),
        ]);
        let certificates = Certificates::load(&entries).unwrap();
        let receiver = |name: &str| ListenTls { certificate: name.into(), clients: None };

        let port = tls_receiver(&certificates, &receiver("lab")).await;
        let sent = send_mllp(&certificates, port, Some(connecting(Some("lab-itself"), None)), "wait").await;
        assert_eq!(sent, Ok(()), "the named certificate itself is trusted");

        let port = tls_receiver(&certificates, &receiver("issued")).await;
        let err = send_mllp(&certificates, port, Some(connecting(Some("issuer"), None)), "wait").await.unwrap_err();
        assert!(
            err.starts_with("transient: TLS: the receiver presents a CA's certificate (CA:TRUE) as its own"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn https_destinations_present_a_client_certificate() {
        let (server_ca, client_ca) = (ca("receiver's CA"), ca("clients' CA"));
        let certificates = receiver_and_client(&server_ca, &client_ca);
        let app = axum::Router::new().route("/orders", axum::routing::post(|| async { "taken" }));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let tls = certificates.server(&receiving(true), &[b"http/1.1"]).unwrap();
        let listener = crate::tls::Listener::new(tcp, Some(tls), "test", None).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let send = |names: &str| {
            let config: InterfaceConfig = serde_yaml_ng::from_str(&format!(
                "name: x\nsource: {{ mllp: {{ port: 1 }} }}\ndestination: {{ http: {{ url: 'https://localhost:{port}/orders'{names} }} }}\n"
            ))
            .unwrap();
            let d = config.destination.clone().unwrap();
            let Destination::Http { ca, certificate, .. } = &d.connector else { panic!("an HTTP destination") };
            let tls = certificates.named(&ConnectTls::new(ca, certificate)).unwrap();
            let mut sender = Sender::new(&config, &d, None, None, tls).unwrap();
            async move { outcome(sender.send(1, b"{}").await) }
        };
        assert_eq!(send(", ca: server-ca, certificate: client").await, Ok(()));
        let err = send(", ca: server-ca").await.unwrap_err();
        assert!(err.starts_with("transient:"), "no client certificate: {err}");
        let err = send("").await.unwrap_err();
        assert!(err.starts_with("transient:"), "the platform doesn't trust a private CA: {err}");
    }
}
