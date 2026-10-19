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
//!     attributes: { display_name: displayName, email: mail }
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
#[serde(deny_unknown_fields)]
pub struct LdapAuth {
    /// The `ldap:` entry in resources.yaml to use.
    pub resource: String,
    /// Where people are searched for (the whole subtree).
    pub user_base: String,
    /// How a person is found; `{username}` is what they typed, escaped (RFC 4515), so it can't
    /// change the filter.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    #[serde(default)]
    pub attributes: LdapAttributes,
    pub groups: LdapGroups,
    /// Members of groups inside the configured groups count too. Active Directory answers this
    /// itself (its transitive-membership rule); other directories are walked, at most
    /// `NESTING_LIMIT` levels deep.
    #[serde(default)]
    pub nested: bool,
    /// Where groups are searched for when nested groups are walked (not Active Directory):
    /// default, the directory's own naming context.
    #[serde(default)]
    pub group_base: Option<String>,
    /// How often the groups and account state of people with a session or token are read again
    /// (at least 1m). Someone removed from the groups, disabled or deleted in the directory loses
    /// access within this long.
    #[serde(default = "default_refresh", deserialize_with = "crate::config::duration")]
    pub refresh: Duration,
}

/// Which directory attributes hold a person's name and email.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LdapAttributes {
    #[serde(default = "default_display_name")]
    pub display_name: String,
    #[serde(default = "default_email")]
    pub email: String,
}

impl Default for LdapAttributes {
    fn default() -> Self {
        Self { display_name: default_display_name(), email: default_email() }
    }
}

/// Groups (their DNs; one or a list each) → role and permissions.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LdapGroups {
    #[serde(default, deserialize_with = "one_or_many")]
    pub admin: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub operator: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub viewer: Vec<String>,
    #[serde(default)]
    pub permissions: LdapPermissionGroups,
}

/// Groups that add a permission to the role (`payloads`, `audit`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LdapPermissionGroups {
    #[serde(default, deserialize_with = "one_or_many")]
    pub payloads: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
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
        Ok(())
    }

    /// The filter that finds `username`.
    pub fn filter_for(&self, username: &str) -> String {
        self.user_filter.replace(USERNAME, &escape(username))
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
    fn active_directory_disabled_accounts_are_seen() {
        assert!(ad_disabled(Some("514")), "NORMAL_ACCOUNT | ACCOUNTDISABLE");
        assert!(!ad_disabled(Some("512")));
        assert!(!ad_disabled(Some("66048")), "DONT_EXPIRE_PASSWORD, enabled");
        assert!(!ad_disabled(None), "OpenLDAP has no such attribute");
        assert!(!ad_disabled(Some("junk")));
    }
}
