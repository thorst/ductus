//! Doctor and the engine's certificate checks against Postgres over TLS: a throwaway server with its
//! own CA (`scripts/dev/pgtls.sh`; skipped without the Postgres server binaries unless
//! `DUCTUS_TEST_PGTLS=required`, as in CI).

use ductus_core::resources::{CertificateEntry, Database, Resources};
use ductus_runtime::doctor::{self, Check, Mode, Outcome, Probe, Target};
use ductus_runtime::tls::Certificates;
use ductus_store::testing::PgTls;
use std::sync::Arc;
use std::time::Duration;

fn step<'a>(c: &'a Check, name: &str) -> Option<&'a doctor::Step> {
    c.steps.iter().find(|s| s.name == name)
}

fn detail(c: &Check, name: &str) -> String {
    step(c, name).map(|s| s.detail.clone()).unwrap_or_else(|| panic!("no {name} step in {:#?}", c.steps))
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reads_a_postgres_certificate_and_says_whether_it_is_verified() {
    let Some(pg) = PgTls::start() else { return };
    let ca = |file: &str| CertificateEntry { ca_file: Some(pg.dir.join(file)), ..Default::default() };
    let certificates = Arc::new(
        Certificates::load(&[("pg-ca".to_string(), ca("ca.pem")), ("other-ca".to_string(), ca("other-ca.pem"))].into())
            .unwrap(),
    );
    let p = pg.port;
    let db = |host: &str, more: &str| {
        format!("{{ postgres: {{ host: {host}, port: {p}, database: ductus, user: ductus, password: ductus{more} }} }}")
    };
    let resources: Resources = serde_yaml_ng::from_str(&format!(
        "databases:\n  verified: {}\n  wrong_ca: {}\n  wrong_name: {}\n  by_ca: {}\n  prefer: {}\n  plain: {}\n",
        db("127.0.0.1", ", ca: pg-ca"),
        db("127.0.0.1", ", ca: other-ca"),
        db("127.0.0.2", ", ca: pg-ca"),
        db("127.0.0.2", ", tls: verify-ca, ca: pg-ca"),
        db("127.0.0.1", ""),
        db("127.0.0.1", ", tls: disable"),
    ))
    .unwrap();
    let probe = |name: &str| Probe {
        what: format!("database {name}"),
        target: Target::Database(name.into(), resources.databases[name].clone()),
        used_by: vec![],
        resource: Some(name.into()),
    };
    let names = ["verified", "wrong_ca", "wrong_name", "by_ca", "prefer", "plain"];
    let mut probes: Vec<Probe> = names.iter().map(|n| probe(n)).collect();
    let store = |url: String| Probe {
        what: "engine database".into(),
        target: Target::Store(url),
        used_by: vec![],
        resource: None,
    };
    probes.push(store(pg.verified_url()));
    probes.push(store(pg.url("127.0.0.1", "")));
    let checks = doctor::check(probes.clone(), Duration::from_secs(5), &certificates).await;
    let [verified, wrong_ca, wrong_name, by_ca, prefer, plain, store_verified, store_prefer] = &checks[..] else {
        panic!("{checks:#?}")
    };

    // Verified: the handshake as the engine makes it, the certificate kept, the login says so.
    assert_eq!(verified.outcome, Outcome::Ok, "{verified:#?}");
    assert!(detail(verified, "TLS").contains("trusted; certificate for localhost"), "{verified:#?}");
    assert!(detail(verified, "login").contains("TLS 1.3, verified (tls: verify-full)"), "{verified:#?}");
    let seen = verified.certificate.as_ref().expect("watched: the engine verifies it");
    assert!(seen.trusted && seen.subject.as_deref() == Some("localhost"), "{seen:#?}");
    assert!((364..=365).contains(&(seen.not_after.unwrap() - chrono::Utc::now()).num_days()));

    assert_eq!(wrong_ca.outcome, Outcome::Fail);
    assert!(detail(wrong_ca, "TLS").contains("not trusted"), "{wrong_ca:#?}");
    assert!(step(wrong_ca, "login").is_none(), "no login when the handshake the engine makes is refused");
    assert!(wrong_ca.certificate.as_ref().is_some_and(|c| c.error.is_some() && c.subject.is_some()), "read anyway");
    assert!(detail(wrong_name, "TLS").contains("not 127.0.0.2"), "{wrong_name:#?}");
    assert_eq!(by_ca.outcome, Outcome::Ok, "verify-ca checks the CA, not the name: {by_ca:#?}");

    // Not verified: said, with what verify-full would make of it; not watched. On this machine
    // that's no warning (nothing crosses a network).
    let tls = detail(prefer, "TLS");
    assert!(tls.contains("not verified (tls: prefer), and tls: verify-full would refuse it: not trusted"), "{tls}");
    assert!(detail(prefer, "login").contains("not verified (tls: prefer)"), "{prefer:#?}");
    assert_eq!((prefer.outcome, prefer.certificate.is_none()), (Outcome::Ok, true), "{prefer:#?}");
    assert!(step(plain, "TLS").is_none() && detail(plain, "login").contains("plain, no TLS (tls: disable)"));

    // The engine's own database: its URL's sslrootcert.
    assert_eq!(store_verified.outcome, Outcome::Ok, "{store_verified:#?}");
    assert!(detail(store_verified, "login").contains("TLS 1.3, verified (sslmode=verify-full)"));
    assert!(step(store_verified, "commits").is_some());
    let seen = store_verified.certificate.as_ref().unwrap();
    assert_eq!((seen.resource.as_str(), seen.host.as_str(), seen.port), ("engine database", "127.0.0.1", p));
    assert!(detail(store_prefer, "TLS").contains("not verified (sslmode=prefer)"), "{store_prefer:#?}");

    // The engine's own checks (every 6 h): only what it verifies, and nothing logs in.
    let watched = doctor::certificates_only(probes);
    let checks = doctor::check_with(watched, Duration::from_secs(5), Mode::Certificates, &certificates).await;
    let what: Vec<&str> = checks.iter().map(|c| c.what.as_str()).collect();
    assert_eq!(
        what,
        ["database verified", "database wrong_ca", "database wrong_name", "database by_ca", "engine database"]
    );
    assert!(checks.iter().all(|c| step(c, "login").is_none() && step(c, "commits").is_none()), "{checks:#?}");

    let targets = ductus_runtime::certificates::targets(&[], &resources, &pg.verified_url(), None);
    let watched: Vec<&str> = targets.iter().map(|t| t.resource.as_str()).collect();
    assert_eq!(watched, ["engine database", "by_ca", "verified", "wrong_ca", "wrong_name"], "{targets:#?}");
    assert!(matches!(&resources.databases["prefer"], Database::Postgres(c) if !c.mode().verifies()));
}
