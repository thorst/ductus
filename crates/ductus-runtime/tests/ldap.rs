//! Sign-in against a real OpenLDAP directory (`scripts/dev/slapd.sh`: a throwaway slapd per test,
//! as this user, with its own CA). Skipped with a note when slapd isn't installed, unless
//! `DUCTUS_TEST_SLAPD=required` (CI), which makes that a failure.
//!
//! Real Active Directory can't be tested here: its transitive-membership rule is unit-tested as a
//! filter (`ductus_core::ldap`) and verified with the first pilot.

use ductus_core::ldap::{Access, DirectoryRole, LdapAuth};
use ductus_core::resources::{LdapServer, Resources};
use ductus_runtime::doctor::{self, Mode, Probe, Target};
use ductus_runtime::ldap::{Directory, Verdict};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A running slapd, stopped when dropped.
struct Slapd {
    dir: tempfile::TempDir,
    port: u16,
    tls_port: u16,
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dev/slapd.sh")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Slapd {
    fn start() -> Option<Self> {
        if !Path::new("/usr/sbin/slapd").exists() {
            assert!(
                std::env::var("DUCTUS_TEST_SLAPD").as_deref() != Ok("required"),
                "slapd isn't installed and DUCTUS_TEST_SLAPD=required"
            );
            eprintln!("(LDAP tests skipped: apt-get install slapd)");
            return None;
        }
        let dir = tempfile::Builder::new().prefix("ductus-ldap.").tempdir().unwrap();
        let (port, tls_port) = (free_port(), free_port());
        let started = std::process::Command::new("bash")
            .arg(script())
            .args(["start", dir.path().to_str().unwrap(), &port.to_string(), &tls_port.to_string()])
            .status()
            .unwrap();
        assert!(started.success(), "slapd didn't start");
        Some(Self { dir, port, tls_port })
    }

    fn stop(&self) {
        std::process::Command::new("bash")
            .arg(script())
            .args(["stop", self.dir.path().to_str().unwrap()])
            .status()
            .unwrap();
    }

    fn ca(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ca.pem")).unwrap()
    }

    /// The directory as resources.yaml would have it: `security` is `ldaps`, `starttls` or `plain`.
    fn server(&self, security: &str, ca: bool) -> LdapServer {
        let (url, starttls, insecure) = match security {
            "ldaps" => (format!("ldaps://127.0.0.1:{}", self.tls_port), false, false),
            "starttls" => (format!("ldap://127.0.0.1:{}", self.port), true, false),
            _ => (format!("ldap://127.0.0.1:{}", self.port), false, true),
        };
        LdapServer {
            url,
            starttls,
            insecure,
            bind_dn: Some("cn=ductus-svc,ou=service,dc=example,dc=org".into()),
            bind_password: Some("svc-password".into()),
            ca: ca.then(|| self.ca()),
            timeout: Some(Duration::from_secs(5)),
            application: None,
            vendor: None,
            owner: Some("Directory Services".into()),
            custom: Default::default(),
        }
    }

    fn directory(&self, security: &str, nested: bool) -> Directory {
        let resources =
            Resources { ldap: [("corp".to_string(), self.server(security, true))].into(), ..Default::default() };
        let auth: LdapAuth = serde_yaml_ng::from_str(&format!(
            "resource: corp
user_base: ou=people,dc=example,dc=org
user_filter: (uid={{username}})
attributes: {{ display_name: cn, email: mail }}
nested: {nested}
groups:
  admin: cn=ductus-admins,ou=groups,dc=example,dc=org
  operator: cn=ductus-operators,ou=groups,dc=example,dc=org
  viewer: cn=ductus-viewers,ou=groups,dc=example,dc=org
  permissions:
    payloads: cn=ductus-phi,ou=groups,dc=example,dc=org
    audit: cn=ductus-auditors,ou=groups,dc=example,dc=org
"
        ))
        .unwrap();
        auth.check().unwrap();
        Directory::new(Some(&auth), &resources).unwrap().unwrap()
    }

    /// Change the directory as its admin would.
    async fn admin(&self) -> ldap3::Ldap {
        let (conn, mut ldap) = ldap3::LdapConnAsync::new(&format!("ldap://127.0.0.1:{}", self.port)).await.unwrap();
        ldap3::drive!(conn);
        ldap.simple_bind("cn=admin,dc=example,dc=org", "admin-password").await.unwrap().success().unwrap();
        ldap
    }
}

impl Drop for Slapd {
    fn drop(&mut self) {
        self.stop();
    }
}

fn role(verdict: &Verdict) -> Option<(DirectoryRole, Vec<String>)> {
    match verdict {
        Verdict::Allowed(p) => Some((p.access.role, p.access.permissions.clone())),
        _ => None,
    }
}

const PEOPLE: &str = "ou=people,dc=example,dc=org";

#[tokio::test]
async fn people_sign_in_with_their_directory_password_and_groups() {
    let Some(slapd) = Slapd::start() else { return };
    let d = slapd.directory("ldaps", false);

    let ada = d.sign_in("test-user-ada", "test-user-ada-password").await.unwrap();
    assert_eq!(role(&ada), Some((DirectoryRole::Admin, vec!["payloads".into(), "audit".into()])));
    let anna = d.sign_in("test-user-anna", "test-user-anna-password").await.unwrap();
    let Verdict::Allowed(person) = &anna else { panic!("{anna:?}") };
    assert_eq!(
        (person.dn.as_str(), person.display_name.as_str(), person.email.as_str()),
        ("uid=test-user-anna,ou=people,dc=example,dc=org", "Anna Smith", "anna@example.org")
    );
    assert_eq!(person.access, Access { role: DirectoryRole::Viewer, permissions: vec![] });
    assert_eq!(
        role(&d.sign_in("test-user-olaf", "test-user-olaf-password").await.unwrap()).unwrap().0,
        DirectoryRole::Operator
    );

    assert_eq!(d.sign_in("test-user-anna", "wrong").await.unwrap(), Verdict::WrongPassword);
    assert_eq!(d.sign_in("test-user-anna", "").await.unwrap(), Verdict::WrongPassword, "never an unauthenticated bind");
    assert_eq!(d.sign_in("test-user-nobody", "test-user-nobody-password").await.unwrap(), Verdict::NoGroup);
    assert_eq!(d.sign_in("nobody-by-that-name", "x").await.unwrap(), Verdict::Unknown);
    // `*` would match every person if it reached the filter unescaped.
    assert_eq!(d.sign_in("*", "test-user-anna-password").await.unwrap(), Verdict::Unknown);
    assert_eq!(d.sign_in("test-user-a*", "test-user-anna-password").await.unwrap(), Verdict::Unknown);
    // Without `nested`, a member of a group inside a role group isn't in it.
    assert_eq!(d.sign_in("test-user-nested", "test-user-nested-password").await.unwrap(), Verdict::NoGroup);
}

#[tokio::test]
async fn nested_groups_count_when_asked_and_starttls_works() {
    let Some(slapd) = Slapd::start() else { return };
    let d = slapd.directory("starttls", true);
    // OpenLDAP has no transitive rule: the groups are walked (lab-team is in ductus-operators).
    let nested = d.sign_in("test-user-nested", "test-user-nested-password").await.unwrap();
    assert_eq!(role(&nested), Some((DirectoryRole::Operator, vec![])));
    assert_eq!(
        role(&d.sign_in("test-user-ada", "test-user-ada-password").await.unwrap()).unwrap().0,
        DirectoryRole::Admin
    );
    assert_eq!(d.sign_in("test-user-nobody", "test-user-nobody-password").await.unwrap(), Verdict::NoGroup);

    // A loop of groups doesn't hang the walk.
    let mut admin = slapd.admin().await;
    let loop_member = HashSet::from(["cn=ductus-operators,ou=groups,dc=example,dc=org"]);
    admin
        .modify("cn=lab-team,ou=groups,dc=example,dc=org", vec![ldap3::Mod::Add("member", loop_member)])
        .await
        .unwrap()
        .success()
        .unwrap();
    assert_eq!(
        role(&d.sign_in("test-user-nested", "test-user-nested-password").await.unwrap()).unwrap().0,
        DirectoryRole::Operator
    );
}

#[tokio::test]
async fn a_refresh_sees_groups_removed_and_people_deleted() {
    let Some(slapd) = Slapd::start() else { return };
    let d = slapd.directory("plain", false);
    let names = ["test-user-anna".to_string(), "test-user-olaf".to_string(), "test-user-ada".to_string()];
    let before: Vec<Verdict> = d.check_all(&names).await.unwrap().into_iter().map(Result::unwrap).collect();
    assert!(before.iter().all(|v| matches!(v, Verdict::Allowed(_))), "{before:?}");

    let mut admin = slapd.admin().await;
    let anna = HashSet::from([format!("uid=test-user-anna,{PEOPLE}")]);
    admin
        .modify("cn=ductus-viewers,ou=groups,dc=example,dc=org", vec![ldap3::Mod::Delete("member".to_string(), anna)])
        .await
        .unwrap()
        .success()
        .unwrap();
    admin.delete(&format!("uid=test-user-olaf,{PEOPLE}")).await.unwrap().success().unwrap();
    let ada = HashSet::from([format!("uid=test-user-ada,{PEOPLE}")]);
    admin
        .modify("cn=ductus-phi,ou=groups,dc=example,dc=org", vec![ldap3::Mod::Delete("member".to_string(), ada)])
        .await
        .unwrap()
        .success()
        .unwrap();

    let after: Vec<Verdict> = d.check_all(&names).await.unwrap().into_iter().map(Result::unwrap).collect();
    assert_eq!(after[0], Verdict::NoGroup, "removed from the group");
    assert_eq!(after[1], Verdict::Unknown, "deleted from the directory");
    assert_eq!(role(&after[2]), Some((DirectoryRole::Admin, vec!["audit".into()])), "a permission taken away");
    assert_eq!(after[0].reason(), "in no Ductus group");
}

#[tokio::test]
async fn a_directory_that_is_down_or_refuses_the_service_account_is_an_error() {
    let Some(slapd) = Slapd::start() else { return };
    let d = slapd.directory("ldaps", false);
    assert!(matches!(d.sign_in("test-user-anna", "test-user-anna-password").await.unwrap(), Verdict::Allowed(_)));

    // The wrong service password: an error (the directory can't be asked), never "wrong password".
    let mut wrong = slapd.server("ldaps", true);
    wrong.bind_password = Some("not-it".into());
    let resources = Resources { ldap: [("corp".to_string(), wrong)].into(), ..Default::default() };
    let broken = Directory::new(Some(&d.auth), &resources).unwrap().unwrap();
    let err = format!("{:#}", broken.sign_in("test-user-anna", "test-user-anna-password").await.unwrap_err());
    assert!(err.contains("service account"), "{err}");

    // A certificate this server doesn't trust (no `ca`): refused.
    let resources =
        Resources { ldap: [("corp".to_string(), slapd.server("ldaps", false))].into(), ..Default::default() };
    let untrusted = Directory::new(Some(&d.auth), &resources).unwrap().unwrap();
    assert!(untrusted.sign_in("test-user-anna", "test-user-anna-password").await.is_err());

    slapd.stop();
    let started = std::time::Instant::now();
    let err = format!("{:#}", d.sign_in("test-user-anna", "test-user-anna-password").await.unwrap_err());
    assert!(err.contains("connecting to"), "{err}");
    assert!(d.check_all(&["test-user-anna".into()]).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(5), "refused at once, not waited for");
}

#[tokio::test]
async fn doctor_reads_the_directory_certificate() {
    let Some(slapd) = Slapd::start() else { return };
    let probe = |server: LdapServer| Probe {
        what: "directory corp".into(),
        target: Target::Ldap("corp".into(), server),
        used_by: vec![],
        resource: Some("corp".into()),
    };
    assert!(probe(slapd.server("ldaps", true)).target.has_certificate());
    assert!(!probe(slapd.server("plain", true)).target.has_certificate(), "plain LDAP has none");
    let probes = vec![
        probe(slapd.server("ldaps", true)),
        probe(slapd.server("starttls", true)),
        probe(slapd.server("ldaps", false)),
        probe(slapd.server("plain", false)),
    ];
    assert_eq!(doctor::certificates_only(probes.clone()).len(), 3);
    let checks = doctor::check_with(probes, Duration::from_secs(5), Mode::Full).await;
    for c in &checks[..3] {
        let cert = c.certificate.as_ref().expect("a certificate");
        assert_eq!(
            (cert.resource.as_str(), cert.host.as_str(), cert.subject.as_deref()),
            ("corp", "127.0.0.1", Some("localhost"))
        );
        let days = (cert.not_after.unwrap() - chrono::Utc::now()).num_days();
        assert!((28..=30).contains(&days), "{days}");
    }
    assert!(checks[0].certificate.as_ref().unwrap().trusted && checks[1].certificate.as_ref().unwrap().trusted);
    assert!(
        checks[1].steps.iter().any(|s| s.name == "LDAP" && s.detail.contains("StartTLS accepted")),
        "{:?}",
        checks[1].steps
    );
    assert!(
        checks[0].steps.iter().any(|s| s.name == "login" && s.detail.contains("cn=ductus-svc")),
        "{:?}",
        checks[0].steps
    );
    let untrusted = checks[2].certificate.as_ref().unwrap();
    assert!(!untrusted.trusted && untrusted.fingerprint.is_some(), "read though not trusted");
    assert!(checks[2].steps.iter().all(|s| s.name != "login"), "no login over a connection that failed TLS");
    assert!(checks[3].certificate.is_none());
    assert!(
        checks[3].steps.iter().any(|s| s.name == "TLS" && s.detail.contains("plain ldap://")),
        "{:?}",
        checks[3].steps
    );
    assert!(checks[3].steps.iter().any(|s| s.name == "login" && s.outcome == doctor::Outcome::Ok));
    assert_eq!(checks[0].endpoint, format!("127.0.0.1:{} LDAPS", slapd.tls_port));

    // A port that doesn't do StartTLS (slapd's ldaps port speaks TLS at once).
    let mut wrong = slapd.server("starttls", true);
    wrong.url = format!("ldap://127.0.0.1:{}", slapd.tls_port);
    let c = doctor::check_with(vec![probe(wrong)], Duration::from_secs(3), Mode::Certificates).await.remove(0);
    assert_eq!(c.outcome, doctor::Outcome::Fail, "{:?}", c.steps);
}
