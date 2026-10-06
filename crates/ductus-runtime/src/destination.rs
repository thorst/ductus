//! Delivery to a destination.

use ductus_core::config::Destination;
use ductus_hl7::Message;
use std::path::PathBuf;
use std::time::Duration;

pub enum SendError {
    /// Worth retrying: connection refused, timeout, AE.
    Transient(String),
    /// Retrying won't help: AR. The message is quarantined.
    Permanent(String),
}

pub enum Sender {
    Mllp(ductus_mllp::Client),
    File(PathBuf),
}

impl Sender {
    pub fn new(destination: &Destination, base_dir: &std::path::Path) -> Self {
        match destination {
            Destination::Mllp { host, port, timeout_ms } => {
                Sender::Mllp(ductus_mllp::Client::new(format!("{host}:{port}"), Duration::from_millis(*timeout_ms)))
            }
            Destination::File { dir } => Sender::File(base_dir.join(dir)),
        }
    }

    pub async fn send(&mut self, id: i64, payload: &[u8]) -> Result<(), SendError> {
        match self {
            Sender::Mllp(client) => {
                let ack = client.send(payload).await.map_err(|e| SendError::Transient(e.to_string()))?;
                let ack = String::from_utf8_lossy(&ack);
                let ack = Message::parse(&ack).map_err(|e| SendError::Transient(format!("unreadable ACK: {e}")))?;
                match ack.value("MSA-1") {
                    "AA" | "CA" => Ok(()),
                    "AR" | "CR" => Err(SendError::Permanent(format!("rejected: {}", ack.value("MSA-3")))),
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
