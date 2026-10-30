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
use ductus_core::alerts::{OwnerContact, is_address};
use ductus_core::ldap::{self, Access, DirectoryId, IdSource, IdValue, LdapAuth};
use ductus_core::owners::{self, Reach};
use ductus_core::resources::{LdapSecurity, LdapServer, Resources};
use ductus_core::tls::ConnectTls;
use ductus_store::{Audit, By, DirectoryAccount, DirectoryPerson, Role, Store};
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    /// Their sign-in name as the directory spells it (`username_attribute`), if read.
    pub username: Option<String>,
    /// Their id there (`attributes.id`), if the directory gives one.
    pub id: Option<DirectoryId>,
}

impl Person {
    /// As the store keeps it.
    pub fn for_store(&self) -> DirectoryPerson {
        DirectoryPerson {
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            role: Role::try_from(self.access.role as i16).expect("directory roles are the store's"),
            permissions: self.access.permissions.clone(),
            username: self.username.clone(),
            directory_id: self.id.as_ref().map(DirectoryId::to_string),
        }
    }
}

/// An entry the directory found, and the id it has there (for a refusal: is it the same person?).
pub struct SignIn {
    pub verdict: Verdict,
    pub id: Option<DirectoryId>,
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
        Ok(self.sign_in_found(username, password).await?.verdict)
    }

    /// `sign_in`, with the id of the entry found (none when nobody was).
    pub async fn sign_in_found(&self, username: &str, password: &str) -> Result<SignIn> {
        // An empty password is an "unauthenticated bind", which many directories accept: it
        // would sign anyone in.
        if password.is_empty() {
            return Ok(SignIn { verdict: Verdict::WrongPassword, id: None });
        }
        let mut conn = self.connect().await?;
        let found = async {
            let Some(entry) = conn.find(self, &self.auth.filter_for(username), username).await? else {
                return Ok(SignIn { verdict: Verdict::Unknown, id: None });
            };
            let id = self.id_of(&entry);
            if self.disabled(&entry) {
                return Ok(SignIn { verdict: Verdict::Disabled, id });
            }
            match conn.step(conn.ldap.clone().simple_bind(&entry.dn, password)).await? {
                r if r.rc == 0 => {}
                r if r.rc == INVALID_CREDENTIALS => return Ok(SignIn { verdict: Verdict::WrongPassword, id }),
                r => bail!("binding as the user was refused: {r}"),
            }
            conn.bind_service(&self.server).await?; // read the groups as the service account, not as them
            Ok(SignIn { verdict: self.verdict(&mut conn, entry).await?, id })
        }
        .await;
        conn.close().await;
        found
    }

    /// What the directory says about each account now (refresh): one connection for all. An
    /// account whose directory id is known is looked up by it (a renamed person is still found,
    /// and a new person given their old name isn't them); others by name. The whole call fails
    /// when the directory can't be reached; one account's failure is its own.
    pub async fn check_all(&self, accounts: &[DirectoryAccount]) -> Result<Vec<Result<Verdict>>> {
        let mut conn = self.connect().await?;
        let out = self.check_on(&mut conn, accounts).await;
        conn.close().await;
        Ok(out)
    }

    async fn check_on(&self, conn: &mut Conn, accounts: &[DirectoryAccount]) -> Vec<Result<Verdict>> {
        let mut out = Vec::with_capacity(accounts.len());
        for account in accounts {
            let verdict = async {
                let found = match account.directory_id.as_deref().and_then(|id| self.searchable(id)) {
                    Some(id) => conn.find(self, &id.filter(), &account.username).await?,
                    None => conn.find(self, &self.auth.filter_for(&account.username), &account.username).await?,
                };
                let Some(entry) = found else { return Ok(Verdict::Unknown) };
                if self.disabled(&entry) {
                    return Ok(Verdict::Disabled);
                }
                self.verdict(conn, entry).await
            }
            .await;
            out.push(verdict);
        }
        out
    }

    /// Who has this email address in the directory (`attributes.email`, the value escaped), and
    /// what the directory says about them: one entry, or how many there are (none, or several:
    /// nobody is picked).
    async fn by_email(&self, conn: &mut Conn, email: &str) -> Result<Result<Verdict, usize>> {
        let filter = format!("({}={})", self.auth.attributes.email, ldap::escape(email.trim()));
        let mut found = conn.search(&self.auth.user_base, Scope::Subtree, &filter, self.person_attributes()).await?;
        if found.len() != 1 {
            return Ok(Err(found.len()));
        }
        let entry = found.pop().expect("one");
        if self.disabled(&entry) {
            return Ok(Ok(Verdict::Disabled));
        }
        Ok(Ok(self.verdict(conn, entry).await?))
    }

    fn disabled(&self, entry: &SearchEntry) -> bool {
        ldap::ad_disabled(first(entry, "userAccountControl"))
    }

    /// The attributes a search for a person reads.
    fn person_attributes(&self) -> Vec<&str> {
        let a = &self.auth.attributes;
        let mut attrs = vec![a.display_name.as_str(), a.email.as_str(), "userAccountControl"];
        attrs.extend(self.auth.username_attribute());
        match a.id_source() {
            IdSource::Auto => attrs.extend([ldap::OBJECT_GUID, ldap::ENTRY_UUID]),
            IdSource::Named(id) => attrs.push(id),
            IdSource::None => {}
        }
        attrs
    }

    /// The entry's id, as `attributes.id` says to read it.
    fn id_of(&self, entry: &SearchEntry) -> Option<DirectoryId> {
        let id = match self.auth.attributes.id_source() {
            IdSource::None => return None,
            IdSource::Named(attribute) => value_of(entry, attribute),
            IdSource::Auto => value_of(entry, ldap::OBJECT_GUID).or_else(|| value_of(entry, ldap::ENTRY_UUID)),
        };
        if id.is_none() && !NO_ID_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                directory = %self.name,
                id = %self.auth.attributes.id,
                "the directory gives no id for its people (auth.ldap.attributes.id): a renamed sign-in name and one given to someone new can't be told apart; name the attribute that has one"
            );
        }
        id
    }

    /// A stored id this directory's settings can look up (one read from another attribute before
    /// `attributes.id` changed can't: the account is found by name, and takes the new id).
    fn searchable(&self, stored: &str) -> Option<DirectoryId> {
        let id = DirectoryId::parse(stored)?;
        let usable = match self.auth.attributes.id_source() {
            IdSource::None => false,
            IdSource::Named(a) => id.attribute.eq_ignore_ascii_case(a),
            IdSource::Auto => {
                [ldap::OBJECT_GUID, ldap::ENTRY_UUID].iter().any(|a| id.attribute.eq_ignore_ascii_case(a))
            }
        };
        usable.then_some(id)
    }

    async fn verdict(&self, conn: &mut Conn, entry: SearchEntry) -> Result<Verdict> {
        let groups = conn.groups(self, &entry.dn).await?;
        let Some(access) = self.auth.groups.access(&groups) else { return Ok(Verdict::NoGroup) };
        let a = &self.auth.attributes;
        Ok(Verdict::Allowed(Person {
            display_name: first(&entry, &a.display_name).unwrap_or("").to_string(),
            email: first(&entry, &a.email).unwrap_or("").to_string(),
            username: self.auth.username_attribute().and_then(|u| first(&entry, u)).map(str::to_string),
            id: self.id_of(&entry),
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

    /// The person `filter` finds (looking for `username`): None if nobody, or more than one (a
    /// filter that matches two people must not pick one).
    async fn find(&self, d: &Directory, filter: &str, username: &str) -> Result<Option<SearchEntry>> {
        let mut found = self.search(&d.auth.user_base, Scope::Subtree, filter, d.person_attributes()).await?;
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

/// Said once per process: a directory that gives no id.
static NO_ID_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An id attribute's value. `objectGUID` is 16 bytes whatever they look like (ldap3 files values
/// that happen to be UTF-8 as text); another attribute is text unless it came back as bytes.
fn value_of(entry: &SearchEntry, attribute: &str) -> Option<DirectoryId> {
    let named = |k: &String| k.eq_ignore_ascii_case(attribute);
    let bytes = entry.bin_attrs.iter().find(|(k, _)| named(k)).and_then(|(_, v)| v.first()).cloned();
    let value = match (bytes, first(entry, attribute)) {
        (Some(b), _) => IdValue::Bytes(b),
        (None, Some(t)) if attribute.eq_ignore_ascii_case(ldap::OBJECT_GUID) => IdValue::Bytes(t.as_bytes().to_vec()),
        (None, Some(t)) if !t.trim().is_empty() => IdValue::Text(t.to_string()),
        _ => return None,
    };
    let attribute = if attribute.eq_ignore_ascii_case(ldap::OBJECT_GUID) {
        ldap::OBJECT_GUID
    } else if attribute.eq_ignore_ascii_case(ldap::ENTRY_UUID) {
        ldap::ENTRY_UUID
    } else {
        attribute
    };
    Some(DirectoryId { attribute: attribute.to_string(), value })
}

/// The account of someone the directory just let in (`username`: what they typed), made or
/// brought up to date from what it says (role, permissions, name, email).
pub async fn signed_in(
    store: &Store,
    username: &str,
    person: &Person,
    ip: Option<std::net::IpAddr>,
) -> Result<ductus_store::User> {
    let by = By { kind: "system", id: None, user_id: None, name: "directory", via: "session", ip };
    let audit = Audit::new(by, "user.directory_change");
    Ok(store.save_directory_user(username, &person.for_store(), &audit).await?.0)
}

/// Read again what the directory says about everyone with a session or token (`refresh`), by
/// their directory id where it's known: changes are saved (a renamed sign-in name renames the
/// account), refusals end access. When the directory can't be reached nothing changes (sessions
/// run on to their own end: an outage mustn't sign everyone out). A round without owners: see
/// `Round` for the engine's.
pub async fn refresh(store: &Store, directory: &Directory) -> Result<Refreshed> {
    Round::default().run(store, directory).await
}

/// One engine's refresh rounds: the people with a session or token, and the owners its project
/// names (interfaces, destinations, resources.yaml). Owners' directory accounts are checked like
/// the signed-in ones (someone who left is refused, so they show as `disabled`; a rename is seen
/// within `refresh`); owner emails that reach nobody, and owners that name a disabled account, are
/// looked up by the email attribute, and the one person with that email in a Ductus role group
/// gets their account before their first sign-in.
#[derive(Default)]
pub struct Round {
    /// The owner texts, each once.
    owners: Vec<String>,
    teams: BTreeMap<String, OwnerContact>,
    /// Owner texts already warned about in this run (lowercase): once each.
    said: HashSet<String>,
}

impl Round {
    pub fn new(owners: impl IntoIterator<Item = String>, teams: BTreeMap<String, OwnerContact>) -> Self {
        let mut owners: Vec<String> =
            owners.into_iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
        owners.sort_by_key(|o| o.to_lowercase());
        owners.dedup_by(|x, y| owners::same(x, y));
        Self { owners, teams, said: HashSet::new() }
    }

    /// The engine's owners and teams.
    pub fn of(engine: &Engine) -> Self {
        let configs = engine.interfaces.iter().map(|i| i.config.as_ref());
        let owned = owners::owned(configs, &engine.resources).into_iter().map(|o| o.owner);
        Self::new(owned, engine.teams.clone())
    }

    /// One round, on one connection. Fails only when the directory can't be reached.
    pub async fn run(&mut self, store: &Store, directory: &Directory) -> Result<Refreshed> {
        let mut accounts = if self.owners.is_empty() { Default::default() } else { store.accounts().await? };
        let mut checking = store.directory_users_to_check().await?;
        for owner in self.owner_accounts(&accounts) {
            if !checking.iter().any(|a| a.id == owner.id) {
                checking.push(owner);
            }
        }
        let mut done = Refreshed::default();
        if checking.is_empty() && self.lookups(&accounts).is_empty() {
            return Ok(done); // nothing to ask: no connection
        }
        let mut conn = directory.connect().await?;
        let verdicts = directory.check_on(&mut conn, &checking).await;
        save_verdicts(store, &checking, verdicts, &mut done).await;
        if !self.owners.is_empty() {
            if done.changed + done.refused > 0 {
                accounts = store.accounts().await?; // a refusal just now may make an owner one to look up
            }
            self.make_owners(store, directory, &mut conn, &accounts, &mut done).await;
        }
        conn.close().await;
        Ok(done)
    }

    fn reaches(&self, accounts: &ductus_store::Accounts) -> Vec<(String, Reach)> {
        let people = crate::alerts::people(&accounts.users, &accounts.former);
        self.owners.iter().map(|o| (o.clone(), owners::reach(o, &people, &self.teams))).collect()
    }

    /// The directory accounts owner texts reach (disabled ones too: refused by the directory, they
    /// come back when it says yes again), except accounts disabled here.
    fn owner_accounts(&self, accounts: &ductus_store::Accounts) -> Vec<DirectoryAccount> {
        let mut out: Vec<DirectoryAccount> = Vec::new();
        for (_, reach) in self.reaches(accounts) {
            let (Reach::User(u) | Reach::Renamed { user: u, .. } | Reach::Reused { user: u, .. } | Reach::Disabled(u)) =
                reach
            else {
                continue;
            };
            let Some(user) = accounts.users.iter().find(|a| a.username == u) else { continue };
            if user.source == "ldap" && user.disabled_at.is_none() && !out.iter().any(|a| a.id == user.id) {
                let (id, username, directory_id) = (user.id, user.username.clone(), user.directory_id.clone());
                out.push(DirectoryAccount { id, username, directory_id });
            }
        }
        out
    }

    /// The owner texts to look up, with the email to look up: an email address that reaches
    /// nobody (it's mailed as it is), and a text that names a disabled account (its own email
    /// when it is one, else the account's).
    fn lookups(&self, accounts: &ductus_store::Accounts) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (owner, reach) in self.reaches(accounts) {
            let email = match &reach {
                Reach::Address => owner.clone(),
                Reach::Disabled(_) if is_address(&owner) => owner.clone(),
                Reach::Disabled(u) => match accounts.users.iter().find(|a| &a.username == u) {
                    Some(a) if is_address(&a.email) => a.email.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            if !out.iter().any(|(_, e)| owners::same(e, &email)) {
                out.push((owner, email));
            }
        }
        out
    }

    async fn make_owners(
        &mut self,
        store: &Store,
        directory: &Directory,
        conn: &mut Conn,
        accounts: &ductus_store::Accounts,
        done: &mut Refreshed,
    ) {
        for (owner, email) in self.lookups(accounts) {
            let mailed = is_address(&owner);
            let found = match directory.by_email(conn, &email).await {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(%owner, error = %format!("{e:#}"), "couldn't look an owner up in the directory; trying at the next refresh");
                    done.failed += 1;
                    continue;
                }
            };
            let person = match found {
                Ok(Verdict::Allowed(person)) => person,
                Ok(Verdict::NoGroup) if mailed => {
                    self.say(&owner, "this owner is in the directory but in no Ductus group, so no account is made: add them to one (alerts go to the address itself until then)");
                    continue;
                }
                Ok(Verdict::NoGroup) => {
                    self.say(&owner, &format!("this owner is in the directory but in no Ductus group: add them to one, or write their email ({email})"));
                    continue;
                }
                Err(n) if n > 1 => {
                    self.say(&owner, &format!("{n} entries in the directory have the email {email}: no account is made for this owner (write the one meant by their email, and give the others their own)"));
                    continue;
                }
                // Nobody there with that email (alerts go to it as an address), or disabled there.
                _ => continue,
            };
            // The same person as an account disabled here: an admin's decision stands.
            let id = person.id.as_ref().map(DirectoryId::to_string);
            if accounts.users.iter().any(|u| u.disabled_at.is_some() && id.is_some() && u.directory_id == id) {
                continue;
            }
            let Some(username) = person.username.clone() else {
                self.say(&owner, "this owner is in the directory, but their sign-in name can't be read (name it in auth.ldap.attributes.username), so no account is made before their first sign-in");
                continue;
            };
            let audit = Audit {
                detail: Some(serde_json::json!({ "reason": "owner" })),
                ..Audit::new(By::engine(), "user.create")
            };
            match store.save_directory_user(&username, &person.for_store(), &audit).await {
                Ok((user, ductus_store::DirectoryUpdate::Created)) => {
                    tracing::info!(%owner, user = %user.username, "made the directory account of an owner who hasn't signed in yet");
                    done.made += 1;
                }
                Ok((_, ductus_store::DirectoryUpdate::Changed)) => done.changed += 1,
                Ok(_) => {}
                Err(e) => self.say(&owner, &format!("this owner's directory account can't be made: {e:#}")),
            }
        }
    }

    /// The owner texts warned about so far in this run (lowercase).
    pub fn warned(&self) -> impl Iterator<Item = &str> {
        self.said.iter().map(String::as_str)
    }

    /// Warn about an owner text, once per run.
    fn say(&mut self, owner: &str, what: &str) {
        if self.said.insert(owner.to_lowercase()) {
            tracing::warn!(%owner, "{what}");
        }
    }
}

/// Save what the directory said about each account: changes, refusals. What says nothing new is
/// written once for all: when it was asked.
async fn save_verdicts(
    store: &Store,
    people: &[DirectoryAccount],
    verdicts: Vec<Result<Verdict>>,
    done: &mut Refreshed,
) {
    let by = By::engine();
    let mut asked = Vec::new();
    for (account, verdict) in people.iter().zip(verdicts) {
        let name = &account.username;
        let saved = match verdict {
            Ok(Verdict::Allowed(person)) => {
                let audit = Audit::new(by, "user.directory_change");
                store.check_directory_user(name, &person.for_store(), &audit).await.map(|(user, update)| {
                    if user.username != *name {
                        tracing::info!(user = %name, now = %user.username, "a directory user's sign-in name changed there: the account is renamed");
                    }
                    if update != ductus_store::DirectoryUpdate::Unchanged {
                        done.changed += 1;
                    }
                })
            }
            Ok(refused) => {
                let audit = Audit::new(by, "user.directory_refused");
                store.refuse_directory_user(account.id, refused.reason(), &audit).await.map(|newly| {
                    if newly {
                        tracing::warn!(user = %name, reason = refused.reason(), "the directory refused a user: their sessions ended");
                        done.refused += 1;
                    }
                })
            }
            Err(e) => Err(e),
        };
        match saved {
            Ok(()) => {
                done.checked += 1;
                asked.push(account.id);
            }
            Err(e) => {
                tracing::warn!(user = %name, error = %format!("{e:#}"), "couldn't check a user in the directory; trying at the next refresh");
                done.failed += 1;
            }
        }
    }
    if let Err(e) = store.directory_checked(&asked).await {
        tracing::warn!(error = %format!("{e:#}"), "couldn't note when the directory was asked about its users");
    }
}

/// What a refresh round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refreshed {
    pub checked: usize,
    pub changed: usize,
    pub refused: usize,
    /// Owners' accounts made before their first sign-in.
    pub made: usize,
    pub failed: usize,
}

/// Start the refresh rounds (with the engine; nothing if `auth.ldap` isn't configured): the first
/// at once, in the background (a directory that is down never delays a start), then every
/// `refresh`.
pub fn start(engine: &Arc<Engine>) {
    let Some(directory) = engine.directory.clone() else { return };
    let mut round = Round::of(engine);
    let every = directory.auth.refresh;
    // The store, not the engine: a round waiting on a directory that's down holds nothing of it.
    let store = engine.store.clone();
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        loop {
            if weak.strong_count() == 0 {
                return;
            }
            match round.run(&store, &directory).await {
                Ok(r) if r.checked + r.made + r.failed > 0 => tracing::info!(
                    checked = r.checked,
                    changed = r.changed,
                    refused = r.refused,
                    made = r.made,
                    failed = r.failed,
                    "directory users checked"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    "couldn't reach the directory to check its users; sessions continue, trying again at the next refresh"
                ),
            }
            tokio::time::sleep(every).await;
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
