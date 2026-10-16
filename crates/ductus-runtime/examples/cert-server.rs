//! A TLS server for demos and browser tests: each listener presents a self-signed certificate
//! for `localhost` that ends a given number of whole days from today (negative: already ended).
//!
//! ```text
//! cert-server 7443=40 7444=/tmp/days    # a number, or a file holding one (read at every connection)
//! ```
//!
//! A file lets a demo "renew" or "age" a certificate without restarting the server. It is an
//! example (it uses rcgen, a dev-dependency), not part of the `ductus` binary:
//! `cargo build --release -p ductus-runtime --example cert-server`.

use chrono::{Datelike, Utc};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let specs: Vec<(u16, String)> = std::env::args()
        .skip(1)
        .map(|a| {
            let (port, days) = a.split_once('=').expect("usage: cert-server PORT=DAYS|PORT=FILE ...");
            (port.parse().expect("a port number"), days.to_string())
        })
        .collect();
    assert!(!specs.is_empty(), "usage: cert-server PORT=DAYS|PORT=FILE ...");
    let mut tasks = Vec::new();
    for (port, days) in specs {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("binding");
        println!("listening on 127.0.0.1:{port} ({days})");
        tasks.push(tokio::spawn(async move {
            let mut current: Option<(i64, Arc<rustls::ServerConfig>)> = None;
            while let Ok((tcp, _)) = listener.accept().await {
                let days = read_days(&days);
                let config = match &current {
                    Some((d, config)) if *d == days => config.clone(),
                    _ => {
                        let config = config_for(days);
                        println!("127.0.0.1:{port} now presents a certificate ending in {days} day(s)");
                        current = Some((days, config.clone()));
                        config
                    }
                };
                tokio::spawn(async move { drop(tokio_rustls::TlsAcceptor::from(config).accept(tcp).await) });
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

fn read_days(spec: &str) -> i64 {
    spec.parse().unwrap_or_else(|_| {
        std::fs::read_to_string(spec).ok().and_then(|t| t.trim().parse().ok()).expect("days, or a file holding them")
    })
}

fn config_for(days: i64) -> Arc<rustls::ServerConfig> {
    let date = |d: i64| {
        let day = (Utc::now() + chrono::Duration::days(d)).date_naive();
        rcgen::date_time_ymd(day.year(), day.month() as u8, day.day() as u8)
    };
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, format!("localhost ({days} days)"));
    params.not_before = date(-400.min(days - 1));
    params.not_after = date(days);
    let cert = params.self_signed(&key).unwrap();
    let key = rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    Arc::new(config)
}
