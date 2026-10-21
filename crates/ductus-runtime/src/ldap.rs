//! Sign-in with a directory (LDAP / Active Directory; `auth.ldap` in ductus.yaml, the `ldap:`
//! entry in resources.yaml it names). A sign-in finds the person with the service account, binds
//! as them with the password they typed, reads their groups and so their role; `refresh` reads
//! the groups and account state of everyone with a session or a token again, so someone removed
//! from the groups, disabled or deleted in the directory loses access within that long.
//!
//! Connections are TLS (ldaps:// or StartTLS) verified like every other outgoing connection (the
//! system's trusted certificates through the platform verifier, `tls::Trust`), plus the CA the
//! entry's `ca` names (a `certificates:` entry); plain `ldap://` only with `insecure: true`. One connection per sign-in, and one per refresh round:
//! directories handle far more than people sign in.

use crate::control::Engine;
use crate::tls::{Certificates, Trust};
use anyhow::{Context, Result, bail};
use ductus_core::ldap::{self, Access, LdapAuth};
use ductus_core::resources::{LdapSecurity, LdapServer, Resources};
use ductus_core::tls::ConnectTls;
use ductus_store::{Audit, By, DirectoryPerson, Role};
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// LDAP's invalidCredentials: a wrong password (Active Directory also answers it for a disabled,
/// locked or expired account; its reason is in the message's `data 52e/533/775/532`).
const INVALID_CREDENTIALS: u32 = 49;
/// noSuchObject: a base DN (a configured group) that isn't there.
const NO_SUCH_OBJECT: u32 = 32;

/// What the directory says about someone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// They may sign in, as this.
    Allowed(Person),
    /// The password is wrong (only at sign-in).
    WrongPassword,
    /// Nobody by that name (or the filter found more than one).
    Unknown,
    /// Disabled in the directory (Active Directory's `userAccountControl`).
    Disabled,
    /// Found, but in none of the configured role groups.
    NoGroup,
}

impl Verdict {
    /// Why access is refused, in words (for the audit log).
    pub fn reason(&self) -> &'static str {
        match self {
            Verdict::Allowed(_) => "allowed",
            Verdict::WrongPassword => "wrong password",
            Verdict::Unknown => "not in the directory",
            Verdict::Disabled => "disabled in the directory",
            Verdict::NoGroup => "in no Ductus group",
        }
    }
}

/// A person the directory lets in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub dn: String,
    pub display_name: String,
    pub email: String,
    pub access: Access,
}

impl Person {
    /// As the store keeps it.
    pub fn for_store(&self) -> DirectoryPerson {
        DirectoryPerson {
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            role: Role::try_from(self.access.role as i16).expect("directory roles are the store's"),
            permissions: self.access.permissions.clone(),
        }
    }
}

/// A configured directory: how to reach it and how to read it.
#[derive(Clone)]
pub struct Directory {
    pub name: String,
    pub auth: LdapAuth,
    pub server: LdapServer,
    trust: Trust,
}

/// An open connection, bound as the service account (or anonymous).
struct Conn {
    ldap: Ldap,
    timeout: Duration,
}

impl Directory {
    /// The directory `auth.ldap` names, if it's configured. Fails when the resource is missing or
    /// its `ca` names no loaded CA.
    pub fn new(auth: Option<&LdapAuth>, resources: &Resources, certificates: &Certificates) -> Result<Option<Self>> {
        let Some(auth) = auth else { return Ok(None) };
        if let Some(missing) = resources.missing(ductus_core::resources::Kind::Ldap, &auth.resource) {
            bail!("auth.ldap.resource: {missing}");
        }
        let server = resources.ldap[&auth.resource].clone();
        let trust = trust(&server, certificates).with_context(|| format!("ldap `{}`", auth.resource))?;
        Ok(Some(Self { name: auth.resource.clone(), auth: auth.clone(), server, trust }))
    }

    /// Check a sign-in: find the person, bind as them with `password`, read their groups.
    /// Errors mean the directory couldn't be asked (down, refused the service account, timed out).
    pub async fn sign_in(&self, username: &str, password: &str) -> Result<Verdict> {
        // An empty password is an "unauthenticated bind", which many directories accept: it
        // would sign anyone in.
        if password.is_empty() {
            return Ok(Verdict::WrongPassword);
        }
        let mut conn = self.connect().await?;
        let verdict = async {
            let Some(entry) = conn.find(self, username).await? else { return Ok(Verdict::Unknown) };
            if self.disabled(&entry) {
                return Ok(Verdict::Disabled);
            }
            match conn.step(conn.ldap.clone().simple_bind(&entry.dn, password)).await? {
                r if r.rc == 0 => {}
                r if r.rc == INVALID_CREDENTIALS => return Ok(Verdict::WrongPassword),
                r => bail!("binding as the user was refused: {r}"),
            }
            conn.bind_service(&self.server).await?; // read the groups as the service account, not as them
            self.verdict(&mut conn, entry).await
        }
        .await;
        conn.close().await;
        verdict
    }

    /// What the directory says about each of `usernames` now (refresh): one connection for all.
    /// The whole call fails when the directory can't be reached; one name's failure is its own.
    pub async fn check_all(&self, usernames: &[String]) -> Result<Vec<Result<Verdict>>> {
        let mut conn = self.connect().await?;
        let mut out = Vec::with_capacity(usernames.len());
        for name in usernames {
            let verdict = async {
                let Some(entry) = conn.find(self, name).await? else { return Ok(Verdict::Unknown) };
                if self.disabled(&entry) {
                    return Ok(Verdict::Disabled);
                }
                self.verdict(&mut conn, entry).await
            }
            .await;
            out.push(verdict);
        }
        conn.close().await;
        Ok(out)
    }

    fn disabled(&self, entry: &SearchEntry) -> bool {
        ldap::ad_disabled(first(entry, "userAccountControl"))
    }

    async fn verdict(&self, conn: &mut Conn, entry: SearchEntry) -> Result<Verdict> {
        let groups = conn.groups(self, &entry.dn).await?;
        let Some(access) = self.auth.groups.access(&groups) else { return Ok(Verdict::NoGroup) };
        Ok(Verdict::Allowed(Person {
            display_name: first(&entry, &self.auth.attributes.display_name).unwrap_or("").to_string(),
            email: first(&entry, &self.auth.attributes.email).unwrap_or("").to_string(),
            dn: entry.dn,
            access,
        }))
    }

    /// Connect (TLS as configured) and bind as the service account.
    async fn connect(&self) -> Result<Conn> {
        let mut conn = Conn::open(&self.server, &self.trust).await?;
        conn.bind_service(&self.server).await?;
        Ok(conn)
    }
}

/// What a directory's connection trusts: the platform's roots, plus its `ca`.
pub fn trust(server: &LdapServer, certificates: &Certificates) -> Result<Trust> {
    certificates.trust(&ConnectTls::new(&server.ca, &None)).context("ca")
}

/// Connect to `server` and bind as its service account, then leave (`ductus doctor`'s login step).
pub async fn check_service_account(server: &LdapServer, trust: &Trust) -> Result<()> {
    let mut conn = Conn::open(server, trust).await?;
    let bound = conn.bind_service(server).await;
    conn.close().await;
    bound
}

impl Conn {
    /// Connect (TLS as configured: ldaps://, StartTLS, or plain with `insecure`).
    async fn open(server: &LdapServer, trust: &Trust) -> Result<Conn> {
        let timeout = server.timeout();
        let mut settings = LdapConnSettings::new().set_conn_timeout(timeout);
        match server.security() {
            LdapSecurity::Tls => settings = settings.set_config(Arc::new(trust.config()?)),
            LdapSecurity::StartTls => settings = settings.set_config(Arc::new(trust.config()?)).set_starttls(true),
            LdapSecurity::Plain => {}
        }
        let (driver, ldap) = tokio::time::timeout(timeout, LdapConnAsync::with_settings(settings, &server.url))
            .await
            .map_err(|_| anyhow::anyhow!("no answer from {} in {timeout:?}", server.url))?
            .with_context(|| format!("connecting to {}", server.url))?;
        ldap3::drive!(driver);
        Ok(Conn { ldap, timeout })
    }

    /// An operation, within the directory's timeout.
    async fn step<T>(&self, op: impl std::future::Future<Output = ldap3::result::Result<T>>) -> Result<T> {
        match tokio::time::timeout(self.timeout, op).await {
            Ok(result) => Ok(result?),
            Err(_) => bail!("the directory didn't answer in {:?}", self.timeout),
        }
    }

    async fn bind_service(&mut self, server: &LdapServer) -> Result<()> {
        let (dn, password) = match (&server.bind_dn, &server.bind_password) {
            (Some(dn), Some(pw)) => (dn.as_str(), pw.as_str()),
            _ => ("", ""), // anonymous
        };
        let r = self.step(self.ldap.clone().simple_bind(dn, password)).await?;
        if r.rc != 0 {
            bail!("the service account `{dn}` was refused (check bind_dn and bind_password): {r}");
        }
        Ok(())
    }

    async fn search(&self, base: &str, scope: Scope, filter: &str, attrs: Vec<&str>) -> Result<Vec<SearchEntry>> {
        let result = self.step(self.ldap.clone().search(base, scope, filter, attrs)).await?;
        if result.1.rc == NO_SUCH_OBJECT {
            return Ok(Vec::new());
        }
        let (entries, _) = result.success().with_context(|| format!("searching {base} for {filter}"))?;
        Ok(entries.into_iter().map(SearchEntry::construct).collect())
    }

    /// The person `username` names (None if nobody, or more than one: a filter that matches two
    /// people must not pick one).
    async fn find(&self, d: &Directory, username: &str) -> Result<Option<SearchEntry>> {
        let a = &d.auth.attributes;
        let attrs = vec![a.display_name.as_str(), a.email.as_str(), "userAccountControl"];
        let mut found = self.search(&d.auth.user_base, Scope::Subtree, &d.auth.filter_for(username), attrs).await?;
        if found.len() > 1 {
            tracing::warn!(
                user = username,
                matches = found.len(),
                "auth.ldap.user_filter matches more than one entry for this name: refused (make the filter unique)"
            );
            return Ok(None);
        }
        Ok(found.pop())
    }

    /// Which configured groups `dn` belongs to (normalized DNs).
    async fn groups(&mut self, d: &Directory, dn: &str) -> Result<BTreeSet<String>> {
        let configured: BTreeSet<String> = d.auth.groups.all().map(|g| ldap::normalize_dn(g)).collect();
        let mut member = BTreeSet::new();
        if d.auth.nested {
            let root = self.root().await?;
            if root.active_directory {
                // Active Directory answers "directly or through nested groups" itself.
                for group in d.auth.groups.all() {
                    if !self.search(group, Scope::Base, &ldap::member_filter(dn, true), vec!["1.1"]).await?.is_empty() {
                        member.insert(ldap::normalize_dn(group));
                    }
                }
                return Ok(member);
            }
            // Others: walk up from the person's groups to the groups those are in, and so on.
            let base = d.auth.group_base.clone().or(root.naming_context).unwrap_or_default();
            let mut seen = BTreeSet::new();
            let mut frontier = vec![dn.to_string()];
            for _ in 0..ldap::NESTING_LIMIT {
                let mut next = Vec::new();
                for x in &frontier {
                    for g in self.search(&base, Scope::Subtree, &ldap::member_filter(x, false), vec!["1.1"]).await? {
                        if seen.insert(ldap::normalize_dn(&g.dn)) {
                            next.push(g.dn);
                        }
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            return Ok(seen.intersection(&configured).cloned().collect());
        }
        for group in d.auth.groups.all() {
            if !self.search(group, Scope::Base, &ldap::member_filter(dn, false), vec!["1.1"]).await?.is_empty() {
                member.insert(ldap::normalize_dn(group));
            }
        }
        Ok(member)
    }

    /// The directory's root entry: Active Directory or not, and its naming context.
    async fn root(&self) -> Result<Root> {
        let attrs = vec!["supportedCapabilities", "namingContexts", "defaultNamingContext"];
        let entry = self.search("", Scope::Base, "(objectClass=*)", attrs).await?.pop();
        let Some(entry) = entry else { return Ok(Root::default()) };
        let caps = entry.attrs.get("supportedCapabilities").cloned().unwrap_or_default();
        let naming = first(&entry, "defaultNamingContext").or(first(&entry, "namingContexts")).map(str::to_string);
        Ok(Root { active_directory: caps.iter().any(|c| c == ldap::AD_CAPABILITY), naming_context: naming })
    }

    async fn close(mut self) {
        let _ = tokio::time::timeout(self.timeout, self.ldap.unbind()).await;
    }
}

#[derive(Default)]
struct Root {
    active_directory: bool,
    naming_context: Option<String>,
}

fn first<'a>(entry: &'a SearchEntry, attribute: &str) -> Option<&'a str> {
    // Attribute names are case-insensitive; directories answer in their own spelling.
    entry.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(attribute)).and_then(|(_, v)| v.first()).map(String::as_str)
}

/// Read again what the directory says about everyone with a session or token (`refresh`):
/// changes are saved, refusals end access. When the directory can't be reached nothing changes
/// (sessions run on to their own end: an outage mustn't sign everyone out).
pub async fn refresh(engine: &Engine, directory: &Directory) -> Result<Refreshed> {
    let people = engine.store.directory_users_to_check().await?;
    let mut done = Refreshed::default();
    if people.is_empty() {
        return Ok(done);
    }
    let names: Vec<String> = people.iter().map(|(_, n)| n.clone()).collect();
    let verdicts = directory.check_all(&names).await?;
    let by = By::engine();
    for ((id, name), verdict) in people.iter().zip(verdicts) {
        match verdict {
            Ok(Verdict::Allowed(person)) => {
                let audit = Audit::new(by, "user.directory_change");
                let (_, update) = engine.store.save_directory_user(name, &person.for_store(), &audit).await?;
                done.checked += 1;
                if update != ductus_store::DirectoryUpdate::Unchanged {
                    done.changed += 1;
                }
            }
            Ok(refused) => {
                let audit = Audit::new(by, "user.directory_refused");
                if engine.store.refuse_directory_user(*id, refused.reason(), &audit).await? {
                    tracing::warn!(user = %name, reason = refused.reason(), "the directory refused a signed-in user: their sessions ended");
                    done.refused += 1;
                }
                done.checked += 1;
            }
            Err(e) => {
                tracing::warn!(user = %name, error = %format!("{e:#}"), "couldn't check a user in the directory; trying at the next refresh");
                done.failed += 1;
            }
        }
    }
    Ok(done)
}

/// What a refresh round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refreshed {
    pub checked: usize,
    pub changed: usize,
    pub refused: usize,
    pub failed: usize,
}

/// Start the refresh rounds (with the engine; nothing if `auth.ldap` isn't configured).
pub fn start(engine: &Arc<Engine>) {
    let Some(directory) = engine.directory.clone() else { return };
    let every = directory.auth.refresh;
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let Some(engine) = weak.upgrade() else { return };
            match refresh(&engine, &directory).await {
                Ok(r) if r.checked + r.failed > 0 => tracing::info!(
                    checked = r.checked,
                    changed = r.changed,
                    refused = r.refused,
                    failed = r.failed,
                    "directory users checked"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    "couldn't reach the directory to check signed-in users; their sessions continue, trying again at the next refresh"
                ),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_names_a_loaded_ca() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap().cert.pem();
        let pem = format!("{cert}{cert}").replace('\n', "\n      ");
        let resources: Resources = serde_yaml_ng::from_str(&format!(
            "certificates:\n  corp-ca:\n    ca: |\n      {pem}\nldap:\n  d: {{ url: 'ldaps://127.0.0.1:1', ca: corp-ca }}\n  e: {{ url: 'ldaps://127.0.0.1:1', ca: nope }}\n"
        ))
        .unwrap();
        let certificates = Certificates::load(&resources.certificates).unwrap();
        let t = trust(&resources.ldap["d"], &certificates).unwrap();
        assert_eq!(t.extra_roots().len(), 2);
        assert!(t.config().is_ok());
        let err = format!("{:#}", trust(&resources.ldap["e"], &certificates).unwrap_err());
        assert!(err.contains("ca: no CA named `nope`"), "{err}");
    }

    #[test]
    fn an_empty_password_never_reaches_the_directory() {
        // Nothing listens on port 1: if the check tried to connect, it would fail instead.
        let resources: Resources =
            serde_yaml_ng::from_str("ldap:\n  d: { url: 'ldap://127.0.0.1:1', insecure: true }\n").unwrap();
        let auth: LdapAuth =
            serde_yaml_ng::from_str("resource: d\nuser_base: dc=x\ngroups: { viewer: 'cn=v,dc=x' }\n").unwrap();
        let none = Certificates::default();
        let d = Directory::new(Some(&auth), &resources, &none).unwrap().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(rt.block_on(d.sign_in("anna", "")).unwrap(), Verdict::WrongPassword);
        assert!(rt.block_on(d.sign_in("anna", "x")).is_err(), "a real password does connect (and fails here)");

        let missing: LdapAuth =
            serde_yaml_ng::from_str("resource: ad\nuser_base: dc=x\ngroups: { viewer: 'cn=v,dc=x' }\n").unwrap();
        let err = format!("{:#}", Directory::new(Some(&missing), &resources, &none).err().unwrap());
        assert!(err.contains("no directory named `ad`"), "{err}");
        assert!(Directory::new(None, &resources, &none).unwrap().is_none());
    }
}
