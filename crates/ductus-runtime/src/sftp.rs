//! SFTP connections (pure Rust: russh + russh-sftp), shared by SFTP sources and destinations.
//!
//! One connection per source or destination, opened on first use and kept open. After any
//! error the caller calls `reset`, and the next use reconnects.

use anyhow::{Context, Result, anyhow, bail};
use ductus_core::resources::SftpServer;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Connection {
    /// The server's name in resources.yaml, for messages.
    name: String,
    server: SftpServer,
    session: Option<Session>,
}

struct Session {
    ssh: russh::client::Handle<HostKeyCheck>,
    sftp: SftpSession,
}

/// Accepts the server only if its key matches `host_key`; remembers the key it saw so a refusal
/// can say which fingerprint to check.
struct HostKeyCheck {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl russh::client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fingerprint = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256),
            PublicKeyOrCertificate::Certificate(cert) => cert.public_key().fingerprint(HashAlg::Sha256),
        }
        .to_string();
        let accepted = match self.expected.as_deref().map(str::trim) {
            Some("any") => true,
            Some(expected) => expected == fingerprint,
            None => false,
        };
        *self.seen.lock().expect("host key lock") = Some(fingerprint);
        Ok(accepted)
    }
}

impl Connection {
    pub fn new(name: &str, server: SftpServer) -> Self {
        Self { name: name.to_string(), server, session: None }
    }

    /// Drop the connection (after an error); the next call reconnects.
    pub fn reset(&mut self) {
        self.session = None;
    }

    pub async fn sftp(&mut self) -> Result<&SftpSession> {
        if self.session.as_ref().is_some_and(|s| s.ssh.is_closed()) {
            self.session = None;
        }
        if self.session.is_none() {
            self.session = Some(self.connect().await?);
        }
        Ok(&self.session.as_ref().expect("connected").sftp)
    }

    async fn connect(&self) -> Result<Session> {
        let (name, s) = (&self.name, &self.server);
        let seen = Arc::new(Mutex::new(None));
        let check = HostKeyCheck { expected: s.host_key.clone(), seen: seen.clone() };
        let config =
            Arc::new(russh::client::Config { keepalive_interval: Some(Duration::from_secs(30)), ..Default::default() });
        let connecting = russh::client::connect(config, (s.host.as_str(), s.port), check);
        let mut ssh = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Err(_) => bail!("sftp `{name}`: timed out connecting to {}:{}", s.host, s.port),
            Ok(Ok(ssh)) => ssh,
            Ok(Err(e)) => {
                let seen = seen.lock().expect("host key lock").clone();
                return Err(match (seen, s.host_key.as_deref()) {
                    (Some(fp), None) => anyhow!(
                        "sftp `{name}`: {}:{} has host key {fp}. If that's the right server, add `host_key: {fp}` to it in resources.yaml",
                        s.host,
                        s.port
                    ),
                    (Some(fp), Some(expected)) if expected.trim() != "any" && expected.trim() != fp => anyhow!(
                        "sftp `{name}`: host key mismatch for {}:{}: expected {expected}, got {fp}. If the server's key was changed on purpose, update host_key",
                        s.host,
                        s.port
                    ),
                    _ => anyhow!(e).context(format!("sftp `{name}`: connecting to {}:{}", s.host, s.port)),
                });
            }
        };

        let mut accepted = false;
        if let Some(pem) = &s.private_key {
            let key = russh::keys::decode_secret_key(pem, s.passphrase.as_deref())
                .with_context(|| format!("sftp `{name}`: reading private_key"))?;
            let hash = ssh.best_supported_rsa_hash().await?.flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            accepted = ssh.authenticate_publickey(&s.user, key).await?.success();
        }
        if !accepted && let Some(password) = &s.password {
            accepted = ssh.authenticate_password(&s.user, password).await?.success();
        }
        if !accepted {
            bail!("sftp `{name}`: {}:{} refused the login for user `{}`", s.host, s.port, s.user);
        }
        let channel = ssh.channel_open_session().await.context("opening SSH channel")?;
        channel.request_subsystem(true, "sftp").await.context("starting SFTP")?;
        let sftp = SftpSession::new(channel.into_stream()).await.context("starting SFTP")?;
        tracing::info!(server = %name, host = %s.host, "SFTP connected");
        Ok(Session { ssh, sftp })
    }

    /// Write a file whole: under a temporary dot-name first, then renamed, so a reader never
    /// sees half a file (and SFTP sources, ours included, skip dot-names).
    pub async fn put(&mut self, dir: &str, name: &str, data: &[u8]) -> Result<()> {
        let sftp = self.sftp().await?;
        if !sftp.try_exists(dir).await? {
            sftp.create_dir(dir).await.with_context(|| format!("creating {dir}"))?;
        }
        let tmp = join(dir, &format!(".{name}.tmp"));
        let target = join(dir, name);
        let mut file = sftp.create(&tmp).await.with_context(|| format!("creating {tmp}"))?;
        file.write_all(data).await.with_context(|| format!("writing {tmp}"))?;
        file.shutdown().await.with_context(|| format!("closing {tmp}"))?;
        // SFTP rename won't replace a file. One with this name is an earlier attempt at the same
        // message (re-sent after a crash), so replacing it is right.
        if sftp.try_exists(&target).await? {
            sftp.remove_file(&target).await?;
        }
        sftp.rename(&tmp, &target).await.with_context(|| format!("renaming {tmp} to {target}"))?;
        Ok(())
    }
}

/// `dir/name` on the server (SFTP paths always use `/`).
pub fn join(dir: &str, name: &str) -> String {
    match dir.trim_end_matches('/') {
        "" if dir.starts_with('/') => format!("/{name}"),
        "" | "." => name.to_string(),
        d => format!("{d}/{name}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn joins_paths() {
        assert_eq!(super::join("out", "a.csv"), "out/a.csv");
        assert_eq!(super::join("/data/out/", "a.csv"), "/data/out/a.csv");
        assert_eq!(super::join("/", "a.csv"), "/a.csv");
        assert_eq!(super::join(".", "a.csv"), "a.csv");
    }
}
