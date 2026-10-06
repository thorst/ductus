//! Delivery to a destination.

use ductus_core::config::{AckMode, Destination, OnReject};
use ductus_hl7::Message;
use std::path::PathBuf;
use std::time::Duration;

pub enum SendError {
    /// Worth retrying: connection refused, timeout, AE.
    Transient(String),
    /// Retrying won't help: AR (unless configured to retry). The message is quarantined.
    Permanent(String),
}

pub enum Sender {
    Mllp { client: ductus_mllp::Client, ack: AckMode, on_reject: OnReject },
    File(PathBuf),
}

impl Sender {
    pub fn new(destination: &Destination, base_dir: &std::path::Path) -> Self {
        match destination {
            Destination::Mllp { host, port, timeout_ms, ack, on_reject } => Sender::Mllp {
                client: ductus_mllp::Client::new(format!("{host}:{port}"), Duration::from_millis(*timeout_ms)),
                ack: *ack,
                on_reject: *on_reject,
            },
            Destination::File { dir } => Sender::File(base_dir.join(dir)),
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
            Sender::File(dir) => {
                let write = async {
                    tokio::fs::create_dir_all(&*dir).await?;
                    // Write then rename, so readers never see a partial file.
                    let tmp = dir.join(format!(".{id}.hl7.tmp"));
                    tokio::fs::write(&tmp, payload).await?;
                    tokio::fs::rename(&tmp, dir.join(format!("{id}.hl7"))).await
                };
                write.await.map_err(|e| SendError::Transient(e.to_string()))
            }
        }
    }
}
