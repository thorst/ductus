//! Sign-in with a directory: `auth.ldap` in ductus.yaml, and the pure parts of talking to one
//! (search filters with the username escaped, group membership filters, groups → role). The
//! connection itself is `ductus-runtime/src/ldap.rs`; the directory is a `ldap:` entry in
//! resources.yaml (`crate::resources::LdapServer`).
//!
//! ```yaml
//! auth:
//!   ldap:
//!     resource: corp-ad                          # an `ldap:` entry in resources.yaml
//!     user_base: OU=Staff,DC=hospital,DC=org
//!     user_filter: (sAMAccountName={username})   # the default; OpenLDAP: (uid={username})
//!     attributes: { display_name: displayName, email: mail }   # id: auto (objectGUID, else entryUUID)
//!     groups:                                    # highest role wins; no role group = may not sign in
//!       admin:    CN=Ductus Admins,OU=Groups,DC=hospital,DC=org
//!       operator: CN=Ductus Operators,OU=Groups,DC=hospital,DC=org
//!       viewer:   [CN=Ductus Viewers,OU=Groups,DC=hospital,DC=org, CN=Lab IT,OU=Groups,DC=hospital,DC=org]
//!       permissions:
//!         payloads: CN=Ductus PHI,OU=Groups,DC=hospital,DC=org
//!         audit:    CN=Ductus Auditors,OU=Groups,DC=hospital,DC=org
//!     nested: true                               # members of groups inside these groups count too
//!     refresh: 15m                               # re-read signed-in people's groups and account state
//! ```

use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::Duration;

/// `auth.ldap` in ductus.yaml.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapAuth {
    /// The directory to use: the name of an `ldap:` entry in resources.yaml (its address, TLS and
    /// service account). Required. Example: `resource: corp-ad`.
    pub resource: String,
    /// Where people are searched for, as a DN: the whole subtree under it. Narrow it to the
    /// branch your staff are in, so service accounts and contacts can't sign in by accident.
    /// Required. Example: `user_base: "OU=Staff,DC=hospital,DC=org"`.
    pub user_base: String,
    /// How a person is found from the name they type: an LDAP filter where `{username}` stands
    /// for it, escaped so it can't change the filter. More than one entry found is refused.
    /// `(sAMAccountName={username})` is Active Directory's sign-in name; OpenLDAP and FreeIPA use
    /// `(uid={username})`; `(|(sAMAccountName={username})(mail={username}))` takes either.
    /// Default: `(sAMAccountName={username})`. Example: `user_filter: "(uid={username})"`.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    /// Which of the person's attributes hold their name, email, sign-in name and lasting id.
    /// The defaults suit Active Directory. Default: `displayName`, `mail`, the filter's
    /// attribute, and `auto` for the id.
    #[serde(default)]
    pub attributes: LdapAttributes,
    /// Which directory groups give which role (`admin`, `operator`, `viewer`) and which add a
    /// permission (`permissions.payloads`, `permissions.audit`). The highest role a person's
    /// groups give wins; a person in none can't sign in. At least one role group. Required.
    /// Example: `groups: { operator: "CN=Interface Team,OU=Groups,DC=hospital,DC=org" }`.
    pub groups: LdapGroups,
    /// Count members of groups inside the configured groups too (a department group placed in
    /// "Ductus Operators"). Active Directory answers this itself; other directories are walked
    /// group by group, at most 8 levels deep. Default: `false` (direct members only). Example:
    /// `nested: true`.
    #[serde(default)]
    pub nested: bool,
    /// Where groups are searched for when nested groups are walked (not Active Directory, which
    /// answers by itself), as a DN. Default: the directory's own top (its naming context).
    /// Example: `group_base: "ou=groups,dc=example,dc=org"`.
    #[serde(default)]
    pub group_base: Option<String>,
    /// How often the engine asks the directory again about everyone with a session or a token,
    /// and about directory people who own something (at least `1m`). Someone removed from the
    /// groups, disabled or deleted there loses access within this long, sessions and tokens
    /// alike; a renamed person keeps their account. Shorter means quicker revocation and more
    /// directory searches. Default: `15m`. Example: `refresh: 5m`.
    #[serde(default = "default_refresh", deserialize_with = "crate::config::duration")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::Duration"))]
    pub refresh: Duration,
}

/// Which directory attributes hold a person's name, email, sign-in name and id.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapAttributes {
    /// The attribute with the person's name as others see it in the web UI and the audit log.
    /// Default: `displayName`. Example: `display_name: cn`.
    #[serde(default = "default_display_name")]
    pub display_name: String,
    /// The attribute with the person's email address: shown in the web UI, matched against owner
    /// texts (`about.owner: anna@hospital.org`), and used to find owners in the directory before
    /// their first sign-in. Default: `mail`. Example: `email: userPrincipalName`.
    #[serde(default = "default_email")]
    pub email: String,
    /// The attribute with the sign-in name, so a refresh notices it change and renames the
    /// account. Needed only when `user_filter` doesn't match `{username}` against one attribute
    /// plainly (`ductus validate` warns then). Default: the attribute the filter first matches
    /// `{username}` against (`sAMAccountName` for the default filter). Example: `username: uid`.
    #[serde(default)]
    pub username: Option<String>,
    /// The attribute that tells one person from another through any rename: `auto` (Active
    /// Directory's `objectGUID`, else the standard `entryUUID`), another attribute's name
    /// (`nsUniqueId`), or `none`. With an id, a renamed person keeps their account, and a sign-in
    /// name given to someone new gets a new account instead of the old one's role and history.
    /// `none` matches by sign-in name only, so those two can't be told apart (validate warns).
    /// Default: `auto`. Example: `id: nsUniqueId`.
    #[serde(default = "default_id")]
    pub id: String,
}

impl Default for LdapAttributes {
    fn default() -> Self {
        Self { display_name: default_display_name(), email: default_email(), username: None, id: default_id() }
    }
}

/// Where a person's directory id comes from (`attributes.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdSource<'a> {
    /// `objectGUID` when the entry has one (Active Directory), else `entryUUID`.
    Auto,
    Named(&'a str),
    None,
}

impl LdapAttributes {
    pub fn id_source(&self) -> IdSource<'_> {
        match self.id.trim() {
            a if a.eq_ignore_ascii_case("auto") => IdSource::Auto,
            a if a.eq_ignore_ascii_case("none") => IdSource::None,
            a => IdSource::Named(a),
        }
    }
}

/// OpenLDAP's (and 389 Directory Server's, FreeIPA's) id of an entry, RFC 4530.
pub const ENTRY_UUID: &str = "entryUUID";
/// Active Directory's id of an object: 16 bytes.
pub const OBJECT_GUID: &str = "objectGUID";

/// A person's id in the directory, kept as `users.directory_id`: `<attribute>:<value>`, or
/// `<attribute>:hex:<hex>` for a binary one (`objectGUID`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryId {
    pub attribute: String,
    pub value: IdValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdValue {
    Text(String),
    Bytes(Vec<u8>),
}

impl DirectoryId {
    /// The filter that finds this entry (the value escaped; each byte of a binary one as `\xx`).
    pub fn filter(&self) -> String {
        match &self.value {
            IdValue::Text(v) => format!("({}={})", self.attribute, escape(v)),
            IdValue::Bytes(b) => {
                let bytes: String = b.iter().map(|x| format!("\\{x:02x}")).collect();
                format!("({}={bytes})", self.attribute)
            }
        }
    }

    /// Read back what `Display` wrote.
    pub fn parse(s: &str) -> Option<Self> {
        let (attribute, rest) = s.split_once(':')?;
        let value = match rest.strip_prefix("hex:") {
            Some(hex) if hex.len() % 2 == 0 => IdValue::Bytes(
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                    .collect::<Result<_, _>>()
                    .ok()?,
            ),
            Some(_) => return None,
            None => IdValue::Text(rest.to_string()),
        };
        (!attribute.is_empty()).then(|| Self { attribute: attribute.to_string(), value })
    }
}

impl std::fmt::Display for DirectoryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.value {
            IdValue::Text(v) => write!(f, "{}:{v}", self.attribute),
            IdValue::Bytes(b) => {
                write!(f, "{}:hex:", self.attribute)?;
                b.iter().try_for_each(|x| write!(f, "{x:02x}"))
            }
        }
    }
}

/// The attribute a stored id (`users.directory_id`) was read from, as written there.
pub fn id_attribute(stored: &str) -> &str {
    stored.split_once(':').map_or(stored, |(a, _)| a)
}

/// Groups (their DNs; one or a list each) → role and permissions.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapGroups {
    /// The group (a DN), or groups, whose members are admins: everything operators can do, plus
    /// accounts, tokens and every permission. Write several one per line in a YAML list, not as
    /// `[a, b]` (DNs have commas). Default: none. Example: `admin: "CN=Ductus Admins,OU=Groups,DC=hospital,DC=org"`.
    #[serde(default, deserialize_with = "one_or_many")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::OneOrMany"))]
    pub admin: Vec<String>,
    /// The group or groups whose members are operators: what viewers can do, plus resend, edit,
    /// dismiss, pause, drain and run connection checks. Default: none. Example: `operator:
    /// "CN=Interface Team,OU=Groups,DC=hospital,DC=org"`.
    #[serde(default, deserialize_with = "one_or_many")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::OneOrMany"))]
    pub operator: Vec<String>,
    /// The group or groups whose members are viewers: they see interfaces, health, queues and
    /// messages' details, but change nothing (and see payloads only with `payloads`). Default:
    /// none. Example: `viewer: "CN=Help Desk,OU=Groups,DC=hospital,DC=org"`.
    #[serde(default, deserialize_with = "one_or_many")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::OneOrMany"))]
    pub viewer: Vec<String>,
    /// Groups that add a permission on top of the role: `payloads` (read message contents,
    /// usually patient data) and `audit` (read the audit log). Admins have both anyway. Default:
    /// none.
    #[serde(default)]
    pub permissions: LdapPermissionGroups,
}

/// Groups that add a permission to the role (`payloads`, `audit`).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapPermissionGroups {
    /// The group or groups whose members may read message payloads (and edit them, as
    /// operators): the people who need to see patient data to do their work. Every read is in
    /// the audit log. Default: none. Example: `payloads: "CN=Ductus PHI,OU=Groups,DC=hospital,DC=org"`.
    #[serde(default, deserialize_with = "one_or_many")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::OneOrMany"))]
    pub payloads: Vec<String>,
    /// The group or groups whose members may read and export the audit log: compliance and
    /// security staff. Default: none. Example: `audit: "CN=Ductus Auditors,OU=Groups,DC=hospital,DC=org"`.
    #[serde(default, deserialize_with = "one_or_many")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::OneOrMany"))]
    pub audit: Vec<String>,
}

fn default_user_filter() -> String {
    "(sAMAccountName={username})".into()
}

fn default_display_name() -> String {
    "displayName".into()
}

fn default_email() -> String {
    "mail".into()
}

fn default_id() -> String {
    "auto".into()
}

fn default_refresh() -> Duration {
    Duration::from_secs(15 * 60)
}

/// A DN, or a list of them.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// How deep nested groups are walked outside Active Directory.
pub const NESTING_LIMIT: usize = 8;

/// The placeholder in `user_filter`.
pub const USERNAME: &str = "{username}";

/// Active Directory says so in its root entry's `supportedCapabilities`.
pub const AD_CAPABILITY: &str = "1.2.840.113556.1.4.800";

/// Active Directory's "in this group, directly or through groups inside it" matching rule
/// (LDAP_MATCHING_RULE_IN_CHAIN).
pub const AD_IN_CHAIN: &str = "1.2.840.113556.1.4.1941";

/// The role a person's groups give them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirectoryRole {
    Viewer = 0,
    Operator = 1,
    Admin = 2,
}

/// What a person's groups give them: a role, and permissions on top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub role: DirectoryRole,
    pub permissions: Vec<String>,
}

impl LdapAuth {
    /// Problems that stop the engine from starting.
    pub fn check(&self) -> Result<()> {
        if self.resource.trim().is_empty() || self.user_base.trim().is_empty() {
            bail!("`resource` and `user_base` are required");
        }
        if !self.user_filter.contains(USERNAME) {
            bail!("`user_filter` must contain {USERNAME}, e.g. (sAMAccountName={USERNAME})");
        }
        if !balanced(&self.user_filter) {
            bail!(
                "`user_filter` `{}` isn't a filter: it must start with ( and its parentheses must match",
                self.user_filter
            );
        }
        let g = &self.groups;
        if g.admin.is_empty() && g.operator.is_empty() && g.viewer.is_empty() {
            bail!(
                "`groups` must name at least one group for admin, operator or viewer: without one nobody can sign in"
            );
        }
        if self.groups.all().any(|dn| !dn.contains('=')) {
            bail!("`groups` must be full DNs, like CN=Ductus Admins,OU=Groups,DC=hospital,DC=org");
        }
        // A list in flow style (`[CN=A,DC=h, CN=B,DC=h]`) splits at every comma of its DNs, and
        // each piece still looks like a DN: a group is never directly under the root.
        if let Some(dn) = self.groups.all().find(|dn| !dn.contains(',')) {
            bail!(
                "`groups`: `{dn}` is only part of a DN (a group's DN has several parts, like CN=Ductus Admins,OU=Groups,DC=hospital,DC=org); in a [flow, list] the commas of a DN split it: write lists one DN per line (- CN=...)"
            );
        }
        if self.refresh < Duration::from_secs(60) {
            bail!("`refresh` must be at least 1m");
        }
        // These go into search filters as written: an attribute's name, nothing else.
        let a = &self.attributes;
        let named = [("attributes.username", a.username.as_deref()), ("attributes.id", Some(a.id.as_str()))];
        for (key, value) in named {
            if let Some(v) = value.map(str::trim)
                && (v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            {
                bail!("`{key}` must be an attribute's name (like objectGUID), not `{v}`");
            }
        }
        Ok(())
    }

    /// The filter that finds `username`.
    pub fn filter_for(&self, username: &str) -> String {
        self.user_filter.replace(USERNAME, &escape(username))
    }

    /// The attribute holding the sign-in name: `attributes.username`, else the one `user_filter`
    /// first matches `{username}` against (`(uid={username})` → `uid`). None when the filter
    /// matches it some other way (a refresh then can't see the name change).
    pub fn username_attribute(&self) -> Option<&str> {
        if let Some(a) = self.attributes.username.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            return Some(a);
        }
        let at = self.user_filter.find(&format!("={USERNAME})"))?;
        let attribute = &self.user_filter[self.user_filter[..at].rfind('(')? + 1..at];
        let plain = !attribute.is_empty() && attribute.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        plain.then_some(attribute)
    }
}

impl LdapGroups {
    /// Every DN named, as written.
    pub fn all(&self) -> impl Iterator<Item = &String> {
        let p = &self.permissions;
        self.admin.iter().chain(&self.operator).chain(&self.viewer).chain(&p.payloads).chain(&p.audit)
    }

    /// What membership of `groups` (normalized DNs, see `normalize_dn`) gives: the highest role
    /// named and the permissions, or None when no role group is among them (permission groups
    /// alone give nothing: they add to a role).
    pub fn access(&self, groups: &BTreeSet<String>) -> Option<Access> {
        let any = |dns: &[String]| dns.iter().any(|dn| groups.contains(&normalize_dn(dn)));
        let role = if any(&self.admin) {
            DirectoryRole::Admin
        } else if any(&self.operator) {
            DirectoryRole::Operator
        } else if any(&self.viewer) {
            DirectoryRole::Viewer
        } else {
            return None;
        };
        let mut permissions = Vec::new();
        if any(&self.permissions.payloads) {
            permissions.push("payloads".to_string());
        }
        if any(&self.permissions.audit) {
            permissions.push("audit".to_string());
        }
        Some(Access { role, permissions })
    }
}

/// A value made safe to put in a search filter (RFC 4515): `*`, `(`, `)`, `\` and NUL become
/// `\2a`, `\28`, `\29`, `\5c`, `\00`, so what someone types can only ever be matched, never
/// change the filter (`*` would match everyone, `)(` would add conditions).
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\\' => out.push_str("\\5c"),
            '\0' => out.push_str("\\00"),
            c => out.push(c),
        }
    }
    out
}

/// The filter that matches a group `dn` is a member of: directly (`member`, or `uniqueMember`
/// for groupOfUniqueNames), or with `in_chain`, through groups inside it too (Active Directory's
/// rule; other directories ignore or refuse it).
pub fn member_filter(dn: &str, in_chain: bool) -> String {
    let dn = escape(dn);
    if in_chain { format!("(member:{AD_IN_CHAIN}:={dn})") } else { format!("(|(member={dn})(uniqueMember={dn}))") }
}

/// A DN in one spelling, for comparing: lower case, no spaces around `,`, `=` and `+` (escaped
/// characters kept as they are). Directories compare DNs this way too.
pub fn normalize_dn(dn: &str) -> String {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = dn.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                current.push(c);
                current.extend(chars.next());
            }
            ',' | '=' | '+' => {
                parts.push((std::mem::take(&mut current), Some(c)));
            }
            c => current.push(c),
        }
    }
    parts.push((current, None));
    let mut out = String::with_capacity(dn.len());
    for (text, sep) in parts {
        out.push_str(text.trim());
        out.extend(sep);
    }
    out.to_lowercase()
}

/// Active Directory's `userAccountControl` says the account is disabled (bit 2, ACCOUNTDISABLE).
pub fn ad_disabled(user_account_control: Option<&str>) -> bool {
    user_account_control.and_then(|v| v.trim().parse::<u32>().ok()).is_some_and(|v| v & 2 != 0)
}

/// Parentheses that open first and match up.
fn balanced(filter: &str) -> bool {
    let filter = filter.trim();
    if !filter.starts_with('(') || !filter.ends_with(')') {
        return false;
    }
    let mut depth = 0i32;
    let mut escaped = false;
    for c in filter.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(yaml: &str) -> Result<LdapAuth> {
        let a: LdapAuth = serde_yaml_ng::from_str(yaml)?;
        a.check()?;
        Ok(a)
    }

    const BASE: &str = "resource: corp\nuser_base: OU=Staff,DC=h,DC=org\n";

    #[test]
    fn what_someone_types_can_never_change_the_filter() {
        let a = auth(&format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\n")).unwrap();
        assert_eq!(a.filter_for("anna"), "(sAMAccountName=anna)");
        // `*` would match everyone; `)(` would add a condition; a backslash would start an escape.
        assert_eq!(a.filter_for("*"), "(sAMAccountName=\\2a)");
        assert_eq!(a.filter_for("x)(|(objectClass=*)"), "(sAMAccountName=x\\29\\28|\\28objectClass=\\2a\\29)");
        assert_eq!(a.filter_for("a\\2a"), "(sAMAccountName=a\\5c2a)");
        assert_eq!(a.filter_for("nul\0"), "(sAMAccountName=nul\\00)");
        assert_eq!(a.filter_for("Zoë O'Brien"), "(sAMAccountName=Zoë O'Brien)", "nothing else changes");

        let a = auth(&format!(
            "{BASE}user_filter: (&(objectClass=person)(|(uid={{username}})(mail={{username}})))\ngroups: {{ admin: 'CN=A,DC=h' }}\n"
        ))
        .unwrap();
        assert_eq!(a.filter_for("b*"), "(&(objectClass=person)(|(uid=b\\2a)(mail=b\\2a)))", "every placeholder");
    }

    #[test]
    fn group_filters_escape_the_dn() {
        assert_eq!(
            member_filter("uid=anna,ou=Staff,dc=h", false),
            "(|(member=uid=anna,ou=Staff,dc=h)(uniqueMember=uid=anna,ou=Staff,dc=h))"
        );
        // Active Directory's transitive rule (verified against real AD only at the first pilot).
        assert_eq!(
            member_filter("CN=Smith\\, Anna,OU=Staff,DC=h", true),
            "(member:1.2.840.113556.1.4.1941:=CN=Smith\\5c, Anna,OU=Staff,DC=h)"
        );
        assert_eq!(member_filter("cn=a(b)*", true), "(member:1.2.840.113556.1.4.1941:=cn=a\\28b\\29\\2a)");
    }

    #[test]
    fn dns_compare_in_one_spelling() {
        assert_eq!(
            normalize_dn(" CN=Ductus Admins, OU=Groups ,DC=Hospital,DC=org "),
            "cn=ductus admins,ou=groups,dc=hospital,dc=org"
        );
        assert_eq!(normalize_dn("cn = a + uid = b,dc=x"), "cn=a+uid=b,dc=x");
        assert_eq!(
            normalize_dn("CN=Smith\\, Anna,DC=h"),
            "cn=smith\\, anna,dc=h",
            "an escaped comma isn't a separator"
        );
    }

    #[test]
    fn the_highest_role_group_wins_and_permissions_only_add() {
        let a = auth(&format!(
            "{BASE}groups:\n  admin: CN=Admins,DC=h\n  operator:\n    - CN=Ops,DC=h\n    - CN=Night Ops,DC=h\n  viewer: CN=Viewers,DC=h\n  permissions:\n    payloads: CN=PHI,DC=h\n    audit: ['CN=Audit,DC=h']\n"
        ))
        .unwrap();
        let set = |dns: &[&str]| dns.iter().map(|d| normalize_dn(d)).collect::<BTreeSet<_>>();
        let access = |dns: &[&str]| a.groups.access(&set(dns));
        assert_eq!(access(&["cn=viewers,dc=h"]), Some(Access { role: DirectoryRole::Viewer, permissions: vec![] }));
        assert_eq!(
            access(&["CN=Viewers, DC=h", "CN=Night Ops,DC=h", "CN=PHI,DC=h"]),
            Some(Access { role: DirectoryRole::Operator, permissions: vec!["payloads".into()] })
        );
        assert_eq!(
            access(&["CN=Admins,DC=h", "CN=Audit,DC=h", "CN=PHI,DC=h"]),
            Some(Access { role: DirectoryRole::Admin, permissions: vec!["payloads".into(), "audit".into()] })
        );
        assert_eq!(access(&["CN=PHI,DC=h", "CN=Audit,DC=h"]), None, "permissions without a role give nothing");
        assert_eq!(access(&["CN=Other,DC=h"]), None);
        assert_eq!(access(&[]), None);
        assert_eq!(a.groups.all().count(), 6);
    }

    #[test]
    fn settings_are_checked() {
        let a = auth(&format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\n")).unwrap();
        assert_eq!(
            (a.user_filter.as_str(), a.attributes.display_name.as_str(), a.attributes.email.as_str()),
            ("(sAMAccountName={username})", "displayName", "mail")
        );
        assert_eq!((a.refresh, a.nested, a.group_base), (Duration::from_secs(900), false, None));

        let err = |yaml: String| format!("{:#}", auth(&yaml).unwrap_err());
        assert!(
            err(format!("{BASE}groups: {{ permissions: {{ payloads: 'CN=P,DC=h' }} }}\n"))
                .contains("at least one group")
        );
        assert!(
            err(format!("{BASE}user_filter: (uid=anna)\ngroups: {{ viewer: 'CN=V,DC=h' }}\n"))
                .contains("must contain {username}")
        );
        assert!(
            err(format!("{BASE}user_filter: uid={{username}}\ngroups: {{ viewer: 'CN=V,DC=h' }}\n"))
                .contains("isn't a filter")
        );
        assert!(
            err(format!("{BASE}user_filter: (uid={{username}}))(\ngroups: {{ viewer: 'CN=V,DC=h' }}\n"))
                .contains("isn't a filter")
        );
        assert!(err(format!("{BASE}groups: {{ viewer: Ductus Viewers }}\n")).contains("full DNs"));
        let split =
            err(format!("{BASE}groups:\n  viewer: CN=V,DC=h\n  permissions: {{ payloads: [CN=P,DC=h, CN=Q,DC=h] }}\n"));
        assert!(split.contains("`CN=P` is only part of a DN") && split.contains("one DN per line"), "{split}");
        assert!(err(format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\nrefresh: 30s\n")).contains("at least 1m"));
        assert!(err(format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\nsurprise: 1\n")).contains("unknown field"));
        assert!(balanced("(&(a=\\29)(b=c))"), "an escaped parenthesis doesn't count");
    }

    #[test]
    fn the_sign_in_attribute_comes_from_the_filter_unless_named() {
        let a = auth(&format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\n")).unwrap();
        assert_eq!((a.username_attribute(), a.attributes.id_source()), (Some("sAMAccountName"), IdSource::Auto));
        let with = |rest: &str| auth(&format!("{BASE}groups: {{ viewer: 'CN=V,DC=h' }}\n{rest}"));
        let a = with("user_filter: (&(objectClass=person)(|(uid={username})(mail={username})))\n").unwrap();
        assert_eq!(a.username_attribute(), Some("uid"), "the first one");
        let a = with("user_filter: (|(cn=*{username}*))\n").unwrap();
        assert_eq!(a.username_attribute(), None, "not a plain match");
        let a = with("user_filter: (|(cn=*{username}*))\nattributes: { username: uid, id: nsUniqueId }\n").unwrap();
        assert_eq!((a.username_attribute(), a.attributes.id_source()), (Some("uid"), IdSource::Named("nsUniqueId")));
        assert_eq!(with("attributes: { id: None }\n").unwrap().attributes.id_source(), IdSource::None);
        let err = format!("{:#}", with("attributes: { id: 'x)(uid=*' }\n").unwrap_err());
        assert!(err.contains("`attributes.id` must be an attribute's name"), "{err}");
        assert!(with("attributes: { username: '' }\n").is_err());
    }

    #[test]
    fn directory_ids_are_kept_and_searched_for() {
        let uuid = DirectoryId { attribute: ENTRY_UUID.into(), value: IdValue::Text("6f1c-a*".into()) };
        assert_eq!(uuid.to_string(), "entryUUID:6f1c-a*");
        assert_eq!(uuid.filter(), "(entryUUID=6f1c-a\\2a)", "escaped like any value");
        let guid = DirectoryId { attribute: OBJECT_GUID.into(), value: IdValue::Bytes(vec![0x00, 0xff, 0x2a, 0x10]) };
        assert_eq!(guid.to_string(), "objectGUID:hex:00ff2a10");
        assert_eq!(guid.filter(), "(objectGUID=\\00\\ff\\2a\\10)", "every byte escaped (AD's way)");
        for id in [&uuid, &guid] {
            assert_eq!(DirectoryId::parse(&id.to_string()).as_ref(), Some(id));
        }
        assert_eq!(id_attribute("objectGUID:hex:00ff"), "objectGUID");
        assert_eq!(DirectoryId::parse("objectGUID:hex:0"), None);
        assert_eq!(DirectoryId::parse("no-colon"), None);
    }

    #[test]
    fn active_directory_disabled_accounts_are_seen() {
        assert!(ad_disabled(Some("514")), "NORMAL_ACCOUNT | ACCOUNTDISABLE");
        assert!(!ad_disabled(Some("512")));
        assert!(!ad_disabled(Some("66048")), "DONT_EXPIRE_PASSWORD, enabled");
        assert!(!ad_disabled(None), "OpenLDAP has no such attribute");
        assert!(!ad_disabled(Some("junk")));
    }
}
