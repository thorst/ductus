//! TLS to Postgres (sqlx over rustls with aws-lc): libpq's URL parameters, what a connection is
//! protected with as the server reports it, and why a connection was refused, in plain words.

use anyhow::Result;

/// The TLS version of the connection (`TLSv1.3`) as the server reports it (`pg_stat_ssl`); None
/// for a plain one. Readable by any role for its own session.
pub async fn version<'e>(conn: impl sqlx::PgExecutor<'e>) -> Result<Option<String>> {
    let v: Option<Option<String>> =
        sqlx::query_scalar("SELECT version FROM pg_stat_ssl WHERE pid = pg_backend_pid() AND ssl")
            .fetch_optional(conn)
            .await?;
    Ok(v.flatten())
}

/// What would make a refused TLS connection work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Trust the server's CA (`sslrootcert`, `ca:`, or the system's certificates).
    Ca,
    /// Connect with a name its certificate lists.
    Name,
    /// The server's certificate is out of date (or this server's clock is).
    Renew,
    /// Present a client certificate the server accepts.
    ClientCertificate,
    /// Turn TLS on at the server (the mode requires it).
    ServerTls,
    /// Nothing to suggest beyond the words.
    Other,
}

/// Why connecting failed when it was TLS's doing, in words (`the database server's certificate
/// isn't from a CA this server trusts`), with what fixes it. None for any other failure.
pub fn refusal(e: &sqlx::Error, host: &str) -> Option<(String, Fix)> {
    use rustls::CertificateError as C;
    use rustls::Error as E;
    let tls = match e {
        sqlx::Error::Io(io) => io.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>())?,
        // sqlx's own words when the server answers the SSLRequest with no.
        sqlx::Error::Tls(t) if t.to_string().contains("does not support TLS") => {
            return Some((
                "the server doesn't offer TLS, and the connection asks for it (sslmode require or verify-*)".into(),
                Fix::ServerTls,
            ));
        }
        sqlx::Error::Tls(t) => return Some((format!("TLS: {t}"), Fix::Other)),
        _ => return None,
    };
    Some(match tls {
        E::InvalidCertificate(C::UnknownIssuer) => {
            ("the database server's certificate isn't from a CA this server trusts".into(), Fix::Ca)
        }
        E::InvalidCertificate(C::NotValidForName | C::NotValidForNameContext { .. }) => {
            (format!("the database server's certificate isn't for `{host}`"), Fix::Name)
        }
        E::InvalidCertificate(C::Expired | C::ExpiredContext { .. }) => {
            ("the database server's certificate has expired".into(), Fix::Renew)
        }
        E::InvalidCertificate(C::NotValidYet | C::NotValidYetContext { .. }) => {
            ("the database server's certificate isn't valid yet (is this server's clock right?)".into(), Fix::Renew)
        }
        E::InvalidCertificate(other) => {
            (format!("the database server's certificate was refused ({other:?})"), Fix::Other)
        }
        E::AlertReceived(alert) => {
            use rustls::AlertDescription as A;
            match alert {
                A::CertificateRequired | A::UnknownCA | A::BadCertificate | A::CertificateUnknown | A::AccessDenied => {
                    (
                        format!("the database server refused our client certificate, or its absence ({alert:?})"),
                        Fix::ClientCertificate,
                    )
                }
                other => (format!("the database server ended the handshake ({other:?})"), Fix::Other),
            }
        }
        E::InvalidMessage(_) => ("the server's answer isn't TLS: is it Postgres on this port?".into(), Fix::Other),
        other => (format!("TLS: {other}"), Fix::Other),
    })
}

/// What to do about a refusal of the engine's own database (`DUCTUS_DATABASE_URL`).
fn url_advice(fix: Fix) -> &'static str {
    match fix {
        Fix::Ca => {
            "add its CA as sslrootcert=/path/to/ca.pem to DUCTUS_DATABASE_URL, or install the CA in this server's trusted certificates"
        }
        Fix::Name => {
            "connect with a name the certificate lists (ductus doctor shows them), or sslmode=verify-ca to check only its CA"
        }
        Fix::Renew => "the database server's owner must renew it",
        Fix::ClientCertificate => {
            "give sslcert= and sslkey= in DUCTUS_DATABASE_URL, a certificate from the CA the server trusts for clients"
        }
        Fix::ServerTls => "turn on ssl in the server's postgresql.conf, or ask its DBA",
        Fix::Other => "ductus doctor checks the connection step by step",
    }
}

/// An error connecting to the engine's own database, in words, with what to check. `waited` is
/// how long the pool kept trying before it gave up (a refused or unanswered connection is tried
/// again until then), so the words can say the failure wasn't a moment's.
pub(crate) fn connect_error(e: sqlx::Error, url: &str, waited: Option<std::time::Duration>) -> anyhow::Error {
    let parsed = url::Url::parse(url).ok();
    let host = parsed.as_ref().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    if let Some((words, fix)) = refusal(&e, &host) {
        return anyhow::anyhow!("connecting to PostgreSQL over TLS: {words}: {}", url_advice(fix));
    }
    let port = parsed.as_ref().and_then(|u| u.port()).unwrap_or(5432);
    let user = parsed.as_ref().map(|u| u.username()).filter(|u| !u.is_empty()).unwrap_or("(none)").to_string();
    let database = parsed.as_ref().map(|u| u.path().trim_start_matches('/')).unwrap_or("").to_string();
    let at = format!("connecting to PostgreSQL at {host}:{port}");
    let tried = waited.map(|w| format!(" (tried for {} s)", w.as_secs())).unwrap_or_default();
    let words = match &e {
        sqlx::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => format!(
            "{at}: refused{tried}: nothing accepts connections there. Postgres is down, listens on another port, \
             or a firewall rejects it: check the host and port in the database URL (DUCTUS_DATABASE_URL), \
             and that Postgres is running"
        ),
        sqlx::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => format!(
            "{at}: no answer{tried}: a firewall drops the connection, or the host is down; \
             `ductus doctor` checks the way step by step"
        ),
        sqlx::Error::Io(io) if io.to_string().contains("lookup address") => format!(
            "connecting to PostgreSQL: no host is named `{host}` (DNS has no address for it): \
             check the host in the database URL (DUCTUS_DATABASE_URL)"
        ),
        sqlx::Error::Database(db) => match db.code().as_deref() {
            Some("28P01") => format!(
                "{at}: the password for `{user}` was refused: check the password in the database URL \
                 (DUCTUS_DATABASE_URL; characters like @ or / in it are written %40, %2F)"
            ),
            Some("3D000") => format!(
                "{at}: there's no database `{database}` on that server: create it (the PostgreSQL install guide \
                 says how) or correct its name in the database URL"
            ),
            Some("28000") => format!(
                "{at}: the server refused the login as `{user}`: {}. Its DBA allows it (pg_hba.conf, or \
                 creating the role)",
                db.message()
            ),
            _ => format!("{at}: {}", db.message()),
        },
        sqlx::Error::PoolTimedOut => format!("{at}: no connection{tried}; `ductus doctor` checks the way step by step"),
        other => format!("{at}: {other}"),
    };
    anyhow::anyhow!(words)
}

/// A pool on the engine's database, connected, or why not in words. The pool tries a refused or
/// unanswered connection again until its acquire timeout and then reports only that it timed
/// out; one direct connection after that says why (refused, no answer within 10 s, a login
/// refused), which is what someone starting the engine needs to know.
pub(crate) async fn connect_pool(url: &str, options: sqlx::postgres::PgPoolOptions) -> Result<sqlx::PgPool> {
    use sqlx::Connection;
    let waited = options.get_acquire_timeout();
    let target = connect_url(url);
    match options.connect(&target).await {
        Ok(pool) => Ok(pool),
        Err(sqlx::Error::PoolTimedOut) => {
            let direct = tokio::time::timeout(std::time::Duration::from_secs(10), sqlx::PgConnection::connect(&target));
            let e = match direct.await {
                Ok(Ok(_)) => sqlx::Error::PoolTimedOut, // up again just now: say only that it timed out
                Ok(Err(e)) => e,
                Err(_) => sqlx::Error::Io(std::io::ErrorKind::TimedOut.into()),
            };
            Err(connect_error(e, url, Some(waited)))
        }
        Err(e) => Err(connect_error(e, url, None)),
    }
}

/// The URL as sqlx connects with it. libpq 16's `sslrootcert=system` (the system's trusted
/// certificates, and `verify-full` unless the URL names a mode) isn't a file to sqlx: it is dropped
/// (the system's are trusted anyway) and `sslmode=verify-full` added when the URL names no mode.
/// Any other URL is returned as given.
pub fn connect_url(url: &str) -> String {
    let Ok(mut u) = url::Url::parse(url.trim()) else { return url.to_string() };
    let pairs: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let system = |(k, v): &(String, String)| k == "sslrootcert" && v == "system";
    if !pairs.iter().any(system) {
        return url.to_string();
    }
    let has_mode = pairs.iter().any(|(k, _)| k == "sslmode" || k == "ssl-mode");
    let kept: Vec<_> = pairs.iter().filter(|p| !system(p)).collect();
    u.query_pairs_mut().clear().extend_pairs(kept);
    if !has_mode {
        u.query_pairs_mut().append_pair("sslmode", "verify-full");
    }
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sslrootcert_system_means_the_system_s_certificates_and_verify_full() {
        assert_eq!(
            connect_url("postgres://u:p%40ss@db1/d?sslrootcert=system"),
            "postgres://u:p%40ss@db1/d?sslmode=verify-full"
        );
        assert_eq!(
            connect_url("postgres://u@db1/d?sslrootcert=system&sslmode=verify-ca"),
            "postgres://u@db1/d?sslmode=verify-ca"
        );
        let given = "postgres://u@db1/d?sslmode=require";
        assert_eq!(connect_url(given), given, "anything else as given");
    }

    #[test]
    fn says_why_tls_refused_a_connection() {
        let io = |e: rustls::Error| sqlx::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        let unknown = io(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer));
        assert_eq!(
            refusal(&unknown, "db1"),
            Some(("the database server's certificate isn't from a CA this server trusts".into(), Fix::Ca))
        );
        let name = io(rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName));
        assert_eq!(refusal(&name, "db1").unwrap().1, Fix::Name);
        let words = connect_error(unknown, "postgres://u@db1/d?sslmode=verify-full", None).to_string();
        assert!(words.contains("add its CA as sslrootcert="), "{words}");
        assert!(refusal(&sqlx::Error::PoolTimedOut, "db1").is_none(), "not TLS's doing");
    }

    #[test]
    fn says_why_the_engine_s_database_refused_a_connection_and_what_to_check() {
        let url = "postgres://ductus:pw@db1.hospital.org:5433/ductus";
        let refused = sqlx::Error::Io(std::io::ErrorKind::ConnectionRefused.into());
        let words = connect_error(refused, url, Some(std::time::Duration::from_secs(30))).to_string();
        assert!(
            words.starts_with("connecting to PostgreSQL at db1.hospital.org:5433: refused (tried for 30 s)"),
            "{words}"
        );
        assert!(words.contains("check the host and port in the database URL"), "{words}");
        let silent = sqlx::Error::Io(std::io::ErrorKind::TimedOut.into());
        assert!(connect_error(silent, url, None).to_string().contains("no answer: a firewall drops"));
        let nameless =
            sqlx::Error::Io(std::io::Error::other("failed to lookup address information: Name or service not known"));
        assert!(connect_error(nameless, url, None).to_string().contains("no host is named `db1.hospital.org`"));
        let no_port = connect_error(sqlx::Error::PoolTimedOut, "postgres://u@db1/d", None).to_string();
        assert!(no_port.starts_with("connecting to PostgreSQL at db1:5432: no connection;"), "{no_port}");
    }

    /// The pool alone would say "pool timed out while waiting for an open connection".
    #[tokio::test]
    async fn a_pool_that_timed_out_says_why() {
        let options = sqlx::postgres::PgPoolOptions::new().acquire_timeout(std::time::Duration::from_secs(1));
        let e = connect_pool("postgres://u:p@127.0.0.1:1/d", options).await.unwrap_err().to_string();
        assert!(e.starts_with("connecting to PostgreSQL at 127.0.0.1:1: refused (tried for 1 s)"), "{e}");
    }
}
