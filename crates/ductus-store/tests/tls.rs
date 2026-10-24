//! The store over TLS, against a throwaway Postgres with its own CA (`scripts/dev/pgtls.sh`;
//! skipped without the Postgres server binaries unless `DUCTUS_TEST_PGTLS=required`, as in CI).

use ductus_store::testing::PgTls;
use ductus_store::{NewEngine, Store};

async fn refused(url: &str) -> String {
    match Store::connect_read_only(url).await {
        Ok(_) => panic!("connected: {url}"),
        Err(e) => format!("{e:#}"),
    }
}

#[tokio::test]
async fn the_store_connects_over_verified_tls_and_refuses_the_wrong_ca_or_name() {
    let Some(pg) = PgTls::start() else { return };

    // verify-full against the server's CA: migrations, partitions and the engine's own lock
    // connection all over TLS.
    let store = Store::connect(&pg.verified_url(), 2).await.unwrap();
    assert_eq!(store.tls_version().await.unwrap().as_deref(), Some("TLSv1.3"));
    let engine = NewEngine { instance: "tls-test", host: "here", pid: 1, version: "test", environment: "dev" };
    store.lock_engine(&engine, None).await.unwrap().release().await;

    let ca = |file: &str| format!("sslmode=verify-full&sslrootcert={}", pg.file(file));
    let err = refused(&pg.url("127.0.0.1", &ca("other-ca.pem"))).await;
    assert!(err.contains("the database server's certificate isn't from a CA this server trusts"), "{err}");
    assert!(err.contains("add its CA as sslrootcert="), "and what to do: {err}");
    // The server listens on 127.0.0.2 too, which its certificate doesn't name.
    let err = refused(&pg.url("127.0.0.2", &ca("ca.pem"))).await;
    assert!(err.contains("the database server's certificate isn't for `127.0.0.2`"), "{err}");
    let verify_ca = format!("sslmode=verify-ca&sslrootcert={}", pg.file("ca.pem"));
    let by_ca = Store::connect_read_only(&pg.url("127.0.0.2", &verify_ca)).await.unwrap();
    assert!(by_ca.tls_version().await.unwrap().is_some(), "verify-ca checks the CA, not the name");

    // libpq's default (prefer): TLS when the server offers it, nothing checked.
    let prefer = Store::connect_read_only(&pg.url("127.0.0.1", "")).await.unwrap();
    assert!(prefer.tls_version().await.unwrap().is_some());
    let plain = Store::connect_read_only(&pg.url("127.0.0.1", "sslmode=disable")).await.unwrap();
    assert_eq!(plain.tls_version().await.unwrap(), None);

    // Client-certificate login (pg_hba `cert`): the certificate is the password.
    let cert = format!(
        "postgres://ductus_cert@127.0.0.1:{}/ductus?{}&sslcert={}&sslkey={}",
        pg.port,
        ca("ca.pem"),
        pg.file("client.pem"),
        pg.file("client.key")
    );
    Store::connect_read_only(&cert).await.unwrap();
    let without = cert.split("&sslcert").next().unwrap();
    let err = refused(without).await;
    assert!(err.contains("connection requires a valid client certificate"), "{err}");
}
