//! Measures what a sender meets at a TLS listener, for `scripts/bench-tls.sh`:
//!
//! ```text
//! tls-bench connect 127.0.0.1:6762 500 [--ca FILE] [--cert FILE --key FILE] [--resume]
//!     500 connections one after another, each to the end of its handshake (TCP only without
//!     --ca); full handshakes unless --resume: p50/p95/mean in ms. Post-quantum key exchange
//!     (X25519MLKEM768) unless --classic (X25519 only, as a peer without it)
//! tls-bench hold 127.0.0.1:6762 1000 [--ca FILE] [--cert FILE --key FILE]
//!     open 1,000 connections and keep them idle until SIGTERM
//! tls-bench http https://localhost:6766/ping 2000 [--ca FILE] [--new]
//!     2,000 GETs one after another, on one kept-alive connection or (--new) a new connection and
//!     full handshake each: p50/p95/mean in ms
//! ```
//!
//! An example, not part of the `ductus` binary: `cargo build --release -p ductus-runtime --example tls-bench`.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

struct Args {
    mode: String,
    target: String,
    count: usize,
    ca: Option<String>,
    pair: Option<(String, String)>,
    resume: bool,
    new: bool,
    /// Offer only X25519, as a peer without post-quantum key exchange does.
    classic: bool,
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let usage = "usage: tls-bench connect|hold|http TARGET COUNT [--ca FILE] [--cert FILE --key FILE] [--resume] [--new] [--classic]";
    let (mode, target, count) = (it.next().expect(usage), it.next().expect(usage), it.next().expect(usage));
    let count = count.parse().expect(usage);
    let mut a = Args { mode, target, count, ca: None, pair: None, resume: false, new: false, classic: false };
    let (mut cert, mut key) = (None, None);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--ca" => a.ca = it.next(),
            "--cert" => cert = it.next(),
            "--key" => key = it.next(),
            "--resume" => a.resume = true,
            "--new" => a.new = true,
            "--classic" => a.classic = true,
            _ => panic!("{usage}"),
        }
    }
    a.pair = cert.zip(key);
    a
}

/// Client settings trusting `ca`, presenting the pair; resumption only when asked (a sender that
/// opens a connection now and then rarely resumes).
fn client(a: &Args) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    let ca = std::fs::read(a.ca.as_ref().expect("--ca")).expect("reading --ca");
    for cert in CertificateDer::pem_slice_iter(&ca) {
        roots.add(cert.expect("a certificate")).expect("a CA");
    }
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    if a.classic {
        provider.kx_groups = vec![rustls::crypto::aws_lc_rs::kx_group::X25519];
    }
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots);
    let mut config = match &a.pair {
        None => builder.with_no_client_auth(),
        Some((cert, key)) => {
            let chain = CertificateDer::pem_file_iter(cert).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            builder.with_client_auth_cert(chain, PrivateKeyDer::from_pem_file(key).unwrap()).unwrap()
        }
    };
    if !a.resume {
        config.resumption = rustls::client::Resumption::disabled();
    }
    config
}

fn report(what: &str, mut ms: Vec<f64>) {
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() as f64 * q) as usize).min(ms.len() - 1)];
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    println!("{what}: n={} p50={:.3} p95={:.3} mean={:.3} ms", ms.len(), at(0.5), at(0.95), mean);
}

#[tokio::main]
async fn main() {
    let a = args();
    let host = a.target.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
    match a.mode.as_str() {
        "connect" => {
            let tls = a.ca.is_some().then(|| tokio_rustls::TlsConnector::from(Arc::new(client(&a))));
            let mut ms = Vec::with_capacity(a.count);
            for _ in 0..a.count {
                let started = Instant::now();
                let tcp = TcpStream::connect(&a.target).await.expect("connecting");
                tcp.set_nodelay(true).unwrap();
                if let Some(tls) = &tls {
                    let name = ServerName::try_from(host.clone()).unwrap();
                    let stream = tls.connect(name, tcp).await.expect("handshake");
                    ms.push(started.elapsed().as_secs_f64() * 1000.0);
                    drop(stream);
                } else {
                    ms.push(started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            report(if a.ca.is_some() { "handshake" } else { "tcp connect" }, ms);
        }
        "hold" => {
            let tls = a.ca.is_some().then(|| tokio_rustls::TlsConnector::from(Arc::new(client(&a))));
            let mut held: Vec<Box<dyn Send>> = Vec::with_capacity(a.count);
            for _ in 0..a.count {
                let tcp = TcpStream::connect(&a.target).await.expect("connecting");
                match &tls {
                    Some(tls) => {
                        let name = ServerName::try_from(host.clone()).unwrap();
                        held.push(Box::new(tls.connect(name, tcp).await.expect("handshake")));
                    }
                    None => held.push(Box::new(tcp)),
                }
            }
            println!("holding {} connections", held.len());
            wait_for_term().await;
        }
        "http" => {
            let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
            if a.new {
                builder = builder.pool_max_idle_per_host(0);
            }
            if a.target.starts_with("https:") {
                let mut config = client(&a);
                config.alpn_protocols = vec![b"http/1.1".to_vec()];
                builder = builder.tls_backend_preconfigured(config);
            }
            let http = builder.build().unwrap();
            let mut ms = Vec::with_capacity(a.count);
            for _ in 0..a.count {
                let started = Instant::now();
                let answer = http.get(&a.target).send().await.expect("a request");
                assert!(answer.status().is_success(), "{}", answer.status());
                answer.bytes().await.unwrap();
                ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            report(if a.new { "new connection each" } else { "kept alive" }, ms);
        }
        other => panic!("no mode {other}"),
    }
}

async fn wait_for_term() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        term.recv().await;
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.unwrap();
}
