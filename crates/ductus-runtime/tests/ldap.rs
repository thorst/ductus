//! Sign-in against a real OpenLDAP directory (`scripts/dev/slapd.sh`: a throwaway slapd per test,
//! as this user, with its own CA). Skipped with a note when slapd isn't installed, unless
//! `DUCTUS_TEST_SLAPD=required` (CI), which makes that a failure.
//!
//! Real Active Directory can't be tested here: its transitive-membership rule is unit-tested as a
//! filter (`ductus_core::ldap`) and verified with the first pilot.

use ductus_core::ldap::{Access, DirectoryRole, LdapAuth};
use ductus_core::resources::{CertificateEntry, LdapServer, Resources};
use ductus_runtime::doctor::{self, Mode, Probe, Target};
use ductus_runtime::ldap::{self, Directory, Person, Verdict};
use ductus_runtime::tls::Certificates;
use ductus_store::{DirectoryAccount, Role, Store};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
        if cfg!(windows) {
            eprintln!("(LDAP tests skipped on Windows: scripts/dev/slapd.sh is bash; Linux CI runs them)");
            return None;
        }
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

    /// Its CA as resources.yaml would name it: `slapd-ca: { ca_file: DIR/ca.pem }`.
    fn certificates(&self) -> Arc<Certificates> {
        let entry = CertificateEntry { ca_file: Some(self.dir.path().join("ca.pem")), ..Default::default() };
        Arc::new(Certificates::load(&[("slapd-ca".to_string(), entry)].into()).unwrap())
    }

    /// The directory as resources.yaml would have it: `security` is `ldaps`, `starttls` or `plain`;
    /// `ca`: trust the directory's CA (`ca: slapd-ca`).
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
            ca: ca.then(|| "slapd-ca".into()),
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
        Directory::new(Some(&auth), &resources, &self.certificates()).unwrap().unwrap()
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

/// Accounts to check, known only by name (as before 0.6.7.1).
fn by_name(names: &[&str]) -> Vec<DirectoryAccount> {
    names.iter().map(|n| DirectoryAccount { id: 0, username: n.to_string(), directory_id: None }).collect()
}

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
    let names = by_name(&["test-user-anna", "test-user-olaf", "test-user-ada"]);
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
    let broken = Directory::new(Some(&d.auth), &resources, &slapd.certificates()).unwrap().unwrap();
    let err = format!("{:#}", broken.sign_in("test-user-anna", "test-user-anna-password").await.unwrap_err());
    assert!(err.contains("service account"), "{err}");

    // A certificate this server doesn't trust (no `ca`): refused.
    let resources =
        Resources { ldap: [("corp".to_string(), slapd.server("ldaps", false))].into(), ..Default::default() };
    let untrusted = Directory::new(Some(&d.auth), &resources, &slapd.certificates()).unwrap().unwrap();
    assert!(untrusted.sign_in("test-user-anna", "test-user-anna-password").await.is_err());

    // `ca` names a certificates entry: one that isn't there stops the directory being set up.
    let mut unnamed = slapd.server("ldaps", true);
    unnamed.ca = Some("no-such-ca".into());
    let resources = Resources { ldap: [("corp".to_string(), unnamed)].into(), ..Default::default() };
    let err = format!("{:#}", Directory::new(Some(&d.auth), &resources, &slapd.certificates()).err().unwrap());
    assert!(err.contains("ldap `corp`: ca: no CA named `no-such-ca`"), "{err}");

    slapd.stop();
    let started = std::time::Instant::now();
    let err = format!("{:#}", d.sign_in("test-user-anna", "test-user-anna-password").await.unwrap_err());
    assert!(err.contains("connecting to"), "{err}");
    assert!(d.check_all(&by_name(&["test-user-anna"])).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(5), "refused at once, not waited for");
}

/// A refresh reads every directory account in the database: two tests refreshing at once would
/// check each other's people against the wrong slapd (and refuse them). Taken by the tests that do.
static ONE_DIRECTORY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The test database (`DUCTUS_TEST_DATABASE_URL`), or None with a note.
async fn store() -> Option<Store> {
    let Ok(url) = std::env::var("DUCTUS_TEST_DATABASE_URL") else {
        eprintln!("(directory account tests skipped: set DUCTUS_TEST_DATABASE_URL)");
        return None;
    };
    Some(Store::connect(&url, 4).await.unwrap())
}

fn allowed(verdict: Verdict) -> Person {
    match verdict {
        Verdict::Allowed(person) => person,
        other => panic!("refused: {other:?}"),
    }
}

/// Add a person back under a sign-in name (a new entry: the directory gives it a new id), in
/// `ductus-operators`.
async fn add_person(admin: &mut ldap3::Ldap, uid: &str, name: &str, mail: &str, password: &str) {
    let dn = format!("uid={uid},{PEOPLE}");
    let sn = name.rsplit(' ').next().unwrap();
    let attrs = vec![
        ("objectClass", HashSet::from(["inetOrgPerson"])),
        ("uid", HashSet::from([uid])),
        ("cn", HashSet::from([name])),
        ("sn", HashSet::from([sn])),
        ("mail", HashSet::from([mail])),
        ("userPassword", HashSet::from([password])),
    ];
    admin.add(&dn, attrs).await.unwrap().success().unwrap();
    // With `refint` the delete took them out of the group; without it the old DN is still listed.
    let member = HashSet::from([dn.as_str()]);
    let r = admin
        .modify("cn=ductus-operators,ou=groups,dc=example,dc=org", vec![ldap3::Mod::Add("member", member)])
        .await
        .unwrap();
    assert!(r.rc == 0 || r.rc == 20, "{r}"); // 20: already a member
}

/// The 0.6.7 design's item 5: a sign-in name given to someone new must never bring back the
/// earlier person's account, nor their tokens (whoever holds those strings would get in, with the
/// new person's role). Olaf leaves; a new Olaf is hired under the same name (a new directory entry).
#[tokio::test]
async fn a_reused_sign_in_name_never_brings_back_the_earlier_persons_account() {
    const OLAF: &str = "test-user-olaf";
    let _one = ONE_DIRECTORY.lock().await;
    let Some(slapd) = Slapd::start() else { return };
    let Some(store) = store().await else { return };
    let names = [OLAF.to_string()];
    let forget = || store.forget_test_users(&names);
    forget().await.unwrap(); // left by a run that failed
    let d = slapd.directory("plain", false);
    let audit = || ductus_store::Audit::new(ductus_store::By::cli("test"), "test");

    let first = allowed(d.sign_in(OLAF, "test-user-olaf-password").await.unwrap());
    let old = ldap::signed_in(&store, OLAF, &first, None).await.unwrap();
    let token_name = format!("{OLAF}-token-{}", std::process::id());
    let (_, token) = store.create_token(&token_name, Role::Operator, &[], Some(old.id), &audit()).await.unwrap();
    assert!(store.authenticate(&token).await.unwrap().is_some());

    // Gone from the directory, and the name given to someone else before any refresh ran.
    let mut admin = slapd.admin().await;
    admin.delete(&format!("uid={OLAF},{PEOPLE}")).await.unwrap().success().unwrap();
    add_person(&mut admin, OLAF, "Olaf Newman", "olaf.newman@example.org", "new-olaf-password").await;

    ldap::refresh(&store, &d).await.unwrap();
    assert!(store.authenticate(&token).await.unwrap().is_none(), "a refresh refuses the earlier Olaf's token");

    let second = allowed(d.sign_in(OLAF, "new-olaf-password").await.unwrap());
    let new = ldap::signed_in(&store, OLAF, &second, None).await.unwrap();
    assert_ne!(new.id, old.id, "the new Olaf gets an account of their own");
    assert!(store.authenticate(&token).await.unwrap().is_none(), "the new Olaf's sign-in doesn't bring it back");
    assert_eq!((new.username.as_str(), new.display_name.as_str()), (OLAF, "Olaf Newman"));
    let earlier = store.user(old.id).await.unwrap().unwrap();
    assert_eq!(earlier.username, format!("{OLAF}#{}", old.id), "moved out of the way");
    assert!(earlier.directory_refused_at.is_some());
    forget().await.unwrap();
}

/// A person renamed in the directory (a new sign-in name, display name and email, as at a
/// marriage) keeps their one account: the refresh finds them by entryUUID, the account takes the
/// new name and keeps the old ones, their token goes on working, and signing in with the new name
/// is the same account. `refint` keeps them in their group through the modrdn.
#[tokio::test]
async fn a_renamed_person_keeps_their_account() {
    const ANNA: &str = "test-user-anna";
    const ANNA_J: &str = "test-user-annaj";
    let _one = ONE_DIRECTORY.lock().await;
    let Some(slapd) = Slapd::start() else { return };
    let Some(store) = store().await else { return };
    let names = [ANNA.to_string(), ANNA_J.to_string()];
    let forget = || store.forget_test_users(&names);
    forget().await.unwrap();
    let d = slapd.directory("plain", false);
    let audit = || ductus_store::Audit::new(ductus_store::By::cli("test"), "test");

    let anna = allowed(d.sign_in(ANNA, "test-user-anna-password").await.unwrap());
    let id = anna.id.clone().expect("slapd gives every entry an entryUUID");
    assert_eq!((id.attribute.as_str(), anna.username.as_deref()), ("entryUUID", Some(ANNA)));
    let account = ldap::signed_in(&store, ANNA, &anna, None).await.unwrap();
    assert_eq!(account.directory_id, Some(id.to_string()));
    let token_name = format!("{ANNA}-token-{}", std::process::id());
    let (_, token) = store.create_token(&token_name, Role::Viewer, &[], Some(account.id), &audit()).await.unwrap();

    let mut admin = slapd.admin().await;
    admin
        .modifydn(&format!("uid={ANNA},{PEOPLE}"), &format!("uid={ANNA_J}"), true, None)
        .await
        .unwrap()
        .success()
        .unwrap();
    let mods = vec![
        ldap3::Mod::Replace("cn", HashSet::from(["Anna Jones"])),
        ldap3::Mod::Replace("sn", HashSet::from(["Jones"])),
        ldap3::Mod::Replace("mail", HashSet::from(["anna.jones@example.org"])),
    ];
    admin.modify(&format!("uid={ANNA_J},{PEOPLE}"), mods).await.unwrap().success().unwrap();
    assert_eq!(d.sign_in(ANNA, "test-user-anna-password").await.unwrap(), Verdict::Unknown, "the old name is gone");

    ldap::refresh(&store, &d).await.unwrap();
    let now = store.user(account.id).await.unwrap().unwrap();
    assert_eq!(
        (now.username.as_str(), now.display_name.as_str(), now.email.as_str()),
        (ANNA_J, "Anna Jones", "anna.jones@example.org")
    );
    assert!(now.directory_refused_at.is_none(), "still in ductus-viewers (refint followed the rename)");
    assert!(store.authenticate(&token).await.unwrap().is_some(), "her token goes on working");
    let former: Vec<String> = store.former_names(account.id).await.unwrap().into_iter().map(|n| n.value).collect();
    assert_eq!(former, [ANNA, "Anna Smith", "anna@example.org"]);

    let again = allowed(d.sign_in(ANNA_J, "test-user-anna-password").await.unwrap());
    assert_eq!(ldap::signed_in(&store, ANNA_J, &again, None).await.unwrap().id, account.id, "one account");
    forget().await.unwrap();
}

/// What a refresh round costs (0.6.7.1): 0, 20 and 200 directory accounts with a token, checked
/// by name (as before) and by entryUUID, and the whole round with the store's saves. Run by hand:
/// `cargo test --release -p ductus-runtime --test ldap -- --ignored --nocapture refresh_round`.
#[tokio::test]
#[ignore]
async fn measure_a_refresh_round() {
    let _one = ONE_DIRECTORY.lock().await;
    let Some(slapd) = Slapd::start() else { return };
    let Some(store) = store().await else { return };
    let pattern = ["test-user-bench-%".to_string()];
    store.forget_test_users(&pattern).await.unwrap();
    let d = slapd.directory("plain", false);
    let audit = || ductus_store::Audit::new(ductus_store::By::cli("test"), "test");
    let mut admin = slapd.admin().await;
    let names: Vec<String> = (0..200).map(|i| format!("test-user-bench-{i:03}")).collect();
    for n in &names {
        add_person(&mut admin, n, &format!("Bench {n}"), &format!("{n}@example.org"), "bench-password").await;
    }
    let median = |mut t: Vec<f64>| {
        t.sort_by(f64::total_cmp);
        t[t.len() / 2]
    };
    let mut made = 0;
    for n in [0usize, 20, 200] {
        for name in &names[made..n] {
            let person = allowed(d.check_all(&by_name(&[name])).await.unwrap().remove(0).unwrap());
            let user = ldap::signed_in(&store, name, &person, None).await.unwrap();
            store.create_token(&format!("{name}-token"), Role::Viewer, &[], Some(user.id), &audit()).await.unwrap();
        }
        made = n;
        let accounts = store.directory_users_to_check().await.unwrap();
        assert_eq!(accounts.len(), n);
        let names_only: Vec<DirectoryAccount> =
            accounts.iter().map(|a| DirectoryAccount { directory_id: None, ..a.clone() }).collect();
        let (mut by_name_t, mut by_id_t, mut round_t) = (vec![], vec![], vec![]);
        for _ in 0..5 {
            let t = std::time::Instant::now();
            d.check_all(&names_only).await.unwrap();
            by_name_t.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = std::time::Instant::now();
            d.check_all(&accounts).await.unwrap();
            by_id_t.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = std::time::Instant::now();
            let r = ldap::refresh(&store, &d).await.unwrap();
            round_t.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!((r.checked, r.failed), (n, 0));
        }
        println!(
            "{n:>3} accounts: check by name {:.1} ms, by id {:.1} ms; the whole round {:.1} ms (median of 5)",
            median(by_name_t),
            median(by_id_t),
            median(round_t)
        );
    }
    store.forget_test_users(&pattern).await.unwrap();
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
    let checks = doctor::check_with(probes, Duration::from_secs(5), Mode::Full, &slapd.certificates()).await;
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
    let c = doctor::check_with(vec![probe(wrong)], Duration::from_secs(3), Mode::Certificates, &slapd.certificates())
        .await
        .remove(0);
    assert_eq!(c.outcome, doctor::Outcome::Fail, "{:?}", c.steps);
}
