//! Test hygiene (the `testing` feature: tests only, never in the binary). Accounts are disabled,
//! never deleted, in the product; tests delete the ones they make, or a long-lived development
//! database fills with them (386 by 2026-10-20, and the Users page renders every one).
//!
//! The same statements, for scripts: `forget_test_users` in `scripts/dev/db.sh`.

use anyhow::{Context, Result, bail};
use sqlx::PgPool;

use crate::Store;

/// What refers to `users` (by foreign key), cleared in this order, then the accounts. Actions keep
/// their row and the actor's name, as history; audit rows stay (`audit_log.user_id` has no key).
const FORGET: [&str; 6] = [
    "DELETE FROM api_sessions WHERE user_id IN (SELECT id FROM users WHERE username LIKE ANY($1))
        OR token_id IN (SELECT t.id FROM api_tokens t JOIN users u ON u.id = t.user_id WHERE u.username LIKE ANY($1))",
    "UPDATE message_actions SET user_id = NULL WHERE user_id IN (SELECT id FROM users WHERE username LIKE ANY($1))",
    "UPDATE message_actions SET token_id = NULL
        WHERE token_id IN (SELECT t.id FROM api_tokens t JOIN users u ON u.id = t.user_id WHERE u.username LIKE ANY($1))",
    "DELETE FROM api_tokens WHERE user_id IN (SELECT id FROM users WHERE username LIKE ANY($1))",
    "DELETE FROM user_settings WHERE user_id IN (SELECT id FROM users WHERE username LIKE ANY($1))",
    "DELETE FROM users WHERE username LIKE ANY($1)",
];

/// Delete test accounts and what refers to them, in one transaction. `patterns` are `LIKE`
/// patterns (a plain name matches itself), each starting with `test-user-`: nothing else is ever
/// deleted. Returns how many accounts went.
pub async fn forget_test_users(pool: &PgPool, patterns: &[String]) -> Result<u64> {
    if let Some(p) = patterns.iter().find(|p| !p.starts_with("test-user-")) {
        bail!("{p:?} isn't a test account's name: only test-user-* accounts are deleted");
    }
    let mut tx = pool.begin().await.context("forgetting test accounts")?;
    let mut gone = 0;
    for sql in FORGET {
        gone = sqlx::query(sql)
            .bind(patterns)
            .execute(&mut *tx)
            .await
            .context("forgetting test accounts")?
            .rows_affected();
    }
    tx.commit().await.context("forgetting test accounts")?;
    Ok(gone)
}

impl Store {
    /// [`forget_test_users`] on this store's database.
    pub async fn forget_test_users(&self, patterns: &[String]) -> Result<u64> {
        forget_test_users(&self.pool, patterns).await
    }
}

/// The accounts one test makes. Name each with [`TestUsers::name`]; an async test ends with
/// [`TestUsers::forget`]. Dropping the guard deletes them too: that's all a plain `#[test]` needs,
/// and what catches an async test that failed before `forget` (as best it can: see `Drop`).
pub struct TestUsers {
    url: String,
    names: std::sync::Mutex<Vec<String>>,
}

impl TestUsers {
    pub fn new(url: &str) -> Self {
        TestUsers { url: url.to_string(), names: Default::default() }
    }

    /// A new account name, `<prefix>-<process>-<n>` (unique per run; `prefix` starts with
    /// `test-user`), deleted with the guard.
    pub fn name(&self, prefix: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!("{prefix}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        self.names.lock().unwrap().push(name.clone());
        name
    }

    /// Delete the accounts, with what refers to them. Call it at the end of the test, while its
    /// runtime runs: a refused change's ROLLBACK is sent from a task on that runtime, and until it
    /// is, the change's row locks stay (the last-admin guard's are on every admin).
    pub async fn forget(self) -> Result<u64> {
        let names = self.take();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&self.url).await?;
        forget_test_users(&pool, &names).await
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.names.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// A throwaway Postgres with TLS (`scripts/dev/pgtls.sh`: its own CA and certificates, on
/// 127.0.0.1 and 127.0.0.2), stopped and deleted when dropped. `start` is None, with a note, when
/// the Postgres server binaries aren't installed, unless `DUCTUS_TEST_PGTLS=required` (CI), which
/// makes that a failure.
pub struct PgTls {
    pub dir: std::path::PathBuf,
    pub port: u16,
}

impl PgTls {
    fn script() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dev/pgtls.sh")
    }

    pub fn start() -> Option<Self> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("ductus-pgtls.{}.{nanos}", std::process::id()));
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let started = std::process::Command::new("bash")
            .arg(Self::script())
            .args(["start", dir.to_str()?, &port.to_string()])
            .status()
            .expect("running scripts/dev/pgtls.sh");
        let required = std::env::var("DUCTUS_TEST_PGTLS").as_deref() == Ok("required");
        match started.code() {
            Some(0) => Some(Self { dir, port }),
            Some(2) if !required => {
                eprintln!("(Postgres TLS tests skipped: apt-get install postgresql)");
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
            _ => panic!("the TLS Postgres didn't start (scripts/dev/pgtls.sh)"),
        }
    }

    /// A file it made (`ca.pem`, `other-ca.pem`, `client.pem`, `client.key`, ...).
    pub fn file(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }

    /// `postgres://ductus:ductus@<host>:<port>/ductus?<query>`.
    pub fn url(&self, host: &str, query: &str) -> String {
        format!("postgres://ductus:ductus@{host}:{}/ductus?{query}", self.port)
    }

    /// verify-full against its own CA.
    pub fn verified_url(&self) -> String {
        self.url("127.0.0.1", &format!("sslmode=verify-full&sslrootcert={}", self.file("ca.pem")))
    }
}

impl Drop for PgTls {
    fn drop(&mut self) {
        let _ = std::process::Command::new("bash").arg(Self::script()).arg("stop").arg(&self.dir).status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A test that failed before `forget`. Drop can't await and can't block on the test's runtime (it
/// runs inside it), so a thread of its own deletes them. That blocks the test's runtime meanwhile:
/// a ROLLBACK it hasn't sent yet keeps its locks, so the delete gives up after `lock_timeout` and
/// names what it left rather than hang the test run.
impl Drop for TestUsers {
    fn drop(&mut self) {
        let names = self.take();
        if names.is_empty() {
            return;
        }
        let url = self.url.clone();
        let left = names.join(", ");
        let done = std::thread::spawn(move || -> Result<u64> {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(async {
                let options = url.parse::<sqlx::postgres::PgConnectOptions>()?.options([("lock_timeout", "5s")]);
                let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_with(options).await?;
                forget_test_users(&pool, &names).await
            })
        })
        .join();
        match done {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("couldn't delete this test's accounts ({left}): {e:#}"),
            Err(_) => eprintln!("couldn't delete this test's accounts ({left}): the cleanup panicked"),
        }
    }
}
