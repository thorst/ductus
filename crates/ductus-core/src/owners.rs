//! Who to call about what: `about.owner` per interface, `owner` on a destination whose owner is
//! someone else, and `owner` on resources.yaml entries. `ductus owners` lists them and says
//! which are people with an account (`find`); `ductus owners reassign` moves ownership (someone
//! leaves, a team splits) by editing the `owner:` values in interface.yaml and resources.yaml in
//! place, so comments and layout stay as they were.

use crate::InterfaceConfig;
use crate::about::given;
use crate::alerts::{OwnerContact, is_address};
use crate::resources::{Kind, Resources};
use anyhow::{Result, bail};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

/// Something owned: an interface, one of its destinations, or a resources.yaml entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    /// Empty when nobody is named.
    pub owner: String,
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    Interface(String),
    /// A destination whose owner differs from its interface's.
    Destination {
        interface: String,
        destination: String,
    },
    Resource {
        kind: Kind,
        name: String,
    },
}

impl What {
    /// `adt`, `adt → lab`, `directory corp-ad`.
    pub fn label(&self) -> String {
        match self {
            What::Interface(i) => i.clone(),
            What::Destination { interface, destination } => format!("{interface} → {destination}"),
            What::Resource { kind, name } => format!("{} {name}", kind.label()),
        }
    }

    /// The interface it is or belongs to.
    pub fn interface(&self) -> Option<&str> {
        match self {
            What::Interface(i) | What::Destination { interface: i, .. } => Some(i),
            What::Resource { .. } => None,
        }
    }
}

/// One entry per interface, plus one per destination with an owner of its own (other
/// destinations belong to their interface's owner), plus one per resources.yaml entry that names
/// an owner (an unowned resource isn't listed: most are fine without one).
pub fn owned<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Owned> {
    let mut out = Vec::new();
    for c in configs {
        let owner = given(&c.about.owner).unwrap_or("");
        out.push(Owned { owner: owner.into(), what: What::Interface(c.name.clone()) });
        for (name, d) in c.destinations() {
            if let Some(own) = given(&d.owner).filter(|o| !same(o, owner)) {
                let what = What::Destination { interface: c.name.clone(), destination: name.into() };
                out.push(Owned { owner: own.into(), what });
            }
        }
    }
    let kinds = [
        (Kind::Database, resources.databases.keys().collect::<Vec<_>>()),
        (Kind::Api, resources.apis.keys().collect()),
        (Kind::Sftp, resources.sftp.keys().collect()),
        (Kind::Ldap, resources.ldap.keys().collect()),
        (Kind::Certificate, resources.certificates.keys().collect()),
    ];
    for (kind, names) in kinds {
        for name in names {
            if let Some(owner) = resources.owner(name) {
                out.push(Owned { owner: owner.into(), what: What::Resource { kind, name: name.clone() } });
            }
        }
    }
    out
}

/// A user account, as far as owners go.
#[derive(Debug, Clone)]
pub struct Person<'a> {
    pub username: &'a str,
    pub display_name: Option<&'a str>,
    pub email: Option<&'a str>,
    /// Disabled here, or refused by the directory.
    pub disabled: bool,
}

/// Which account an owner text names.
#[derive(Debug, Clone, Copy)]
pub enum Found<'p, 'a> {
    /// Exactly one account.
    One(&'p Person<'a>),
    /// Several accounts at the same level (two people with one display name): none is chosen.
    Several(usize),
    None,
}

/// The account an owner text names, ignoring case and surrounding spaces: a username first, then
/// an email address, then a display name. The first of those levels that matches anyone decides;
/// two matches there name nobody (a guess would send someone else's alerts to the wrong person).
pub fn find<'p, 'a>(owner: &str, people: &'p [Person<'a>]) -> Found<'p, 'a> {
    let owner = owner.trim();
    if owner.is_empty() {
        return Found::None;
    }
    let same = |a: Option<&str>| a.is_some_and(|a| a.trim().eq_ignore_ascii_case(owner));
    let levels: [&dyn Fn(&Person) -> bool; 3] =
        [&|p| same(Some(p.username)), &|p| same(p.email), &|p| same(p.display_name)];
    for level in levels {
        let hits: Vec<&Person> = people.iter().filter(|p| level(p)).collect();
        match hits.len() {
            0 => continue,
            1 => return Found::One(hits[0]),
            n => return Found::Several(n),
        }
    }
    Found::None
}

/// Who an owner is, for reports and `ductus owners --check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// An account (its username).
    User(String),
    /// An account that is disabled (or refused by the directory): someone who left, probably.
    Disabled(String),
    /// Names several accounts.
    Several(usize),
    /// A team or list in `owners:` (ductus.yaml).
    Team,
    /// Not an account, but an email address.
    Address,
    /// Nobody this engine knows.
    Nobody,
}

impl Reach {
    /// Whether `ductus owners --check` accepts it.
    pub fn ok(&self) -> bool {
        matches!(self, Reach::User(_) | Reach::Team | Reach::Address)
    }
}

/// What an owner text is: an account, else a team in `owners:`, else an address.
pub fn reach(owner: &str, people: &[Person], teams: &BTreeMap<String, OwnerContact>) -> Reach {
    match find(owner, people) {
        Found::One(p) if p.disabled => Reach::Disabled(p.username.into()),
        Found::One(p) => Reach::User(p.username.into()),
        Found::Several(n) => Reach::Several(n),
        Found::None if teams.keys().any(|t| same(t, owner)) => Reach::Team,
        Found::None if is_address(owner) => Reach::Address,
        Found::None => Reach::Nobody,
    }
}

/// Grouped by owner (ignoring case; written as first seen), owners in name order and the
/// unowned last.
pub fn by_owner(items: Vec<Owned>) -> Vec<(String, Vec<Owned>)> {
    let mut groups: BTreeMap<(bool, String), (String, Vec<Owned>)> = BTreeMap::new();
    for item in items {
        let key = (item.owner.is_empty(), item.owner.to_lowercase());
        groups.entry(key).or_insert_with(|| (item.owner.clone(), Vec::new())).1.push(item);
    }
    groups.into_values().collect()
}

/// Owner names match ignoring case and surrounding spaces.
pub fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// One `owner:` line changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// 1-based.
    pub line: usize,
    pub before: String,
    pub after: String,
}

/// interface.yaml with every `owner: <from>` (about and destinations) changed to `to`. Only the
/// values change; comments, quoting and layout are kept. Fails rather than write anything but
/// exactly that change (the result is parsed and compared with the intended config).
pub fn reassign(text: &str, from: &str, to: &str) -> Result<(String, Vec<Change>)> {
    if given(&Some(to.to_string())).is_none() {
        bail!("the new owner can't be blank");
    }
    let (out, changes, _) = edit(text, from, to, |_| true);
    let mut expected = crate::env::parse_yaml(text)?;
    for owner in owner_slots(&mut expected) {
        if owner.as_str().is_some_and(|o| same(o, from)) {
            *owner = Value::String(to.trim().into());
        }
    }
    if !crate::env::parse_yaml(&out).is_ok_and(|e| e == expected) {
        bail!("couldn't change the owner lines safely (an unusual layout?): change `owner: {from}` by hand");
    }
    Ok((out, changes))
}

/// One `owner:` line of resources.yaml changed, and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceChange {
    pub kind: Kind,
    pub name: String,
    pub change: Change,
}

/// resources.yaml with `owner: <from>` changed to `to` in every entry, or only in the entries
/// named in `only`. Like `reassign`: only the values change, and the result is checked.
pub fn reassign_resources(text: &str, from: &str, to: &str, only: &[String]) -> Result<(String, Vec<ResourceChange>)> {
    if given(&Some(to.to_string())).is_none() {
        bail!("the new owner can't be blank");
    }
    let by_hand =
        || format!("couldn't change the owner lines safely (an unusual layout?): change `owner: {from}` by hand");
    let original = crate::env::parse_yaml(text)?;
    let set = |picked: &dyn Fn(&str) -> bool| {
        let mut v = original.clone();
        for (_, name, owner) in resource_slots(&mut v) {
            if picked(&name) && owner.as_str().is_some_and(|o| same(o, from)) {
                *owner = Value::String(to.trim().into());
            }
        }
        v
    };
    // Whose is each `owner: <from>` on the page? Change it alone and see which entry changed.
    let (_, _, hits) = edit(text, from, to, |_| false);
    let mut whose: Vec<(Kind, String)> = Vec::with_capacity(hits);
    for hit in 0..hits {
        let (alone, _, _) = edit(text, from, to, |n| n == hit);
        let Ok(alone) = crate::env::parse_yaml(&alone) else { bail!(by_hand()) };
        let mut slots = original.clone();
        let entries: Vec<(Kind, String)> =
            resource_slots(&mut slots).into_iter().map(|(kind, name, _)| (kind, name)).collect();
        let Some(entry) = entries.into_iter().find(|(_, name)| set(&|n| n == name) == alone) else {
            bail!(by_hand());
        };
        whose.push(entry);
    }
    let picked = |name: &str| only.is_empty() || only.iter().any(|o| o == name);
    let (out, changes, _) = edit(text, from, to, |n| picked(&whose[n].1));
    if !crate::env::parse_yaml(&out).is_ok_and(|e| e == set(&picked)) {
        bail!(by_hand());
    }
    // Each changed line belongs to the entry of its first change.
    let mut hit = 0;
    let mut first_on_line = BTreeMap::new();
    for (n, line) in text.split_inclusive('\n').enumerate() {
        for _ in owner_values(line).into_iter().filter(|v| same(&v.text, from)) {
            first_on_line.entry(n + 1).or_insert(hit);
            hit += 1;
        }
    }
    let changes = changes
        .into_iter()
        .map(|change| {
            let (kind, name) = whose[first_on_line[&change.line]].clone();
            ResourceChange { kind, name, change }
        })
        .collect();
    Ok((out, changes))
}

/// `text` with the `owner: <from>` values that `keep` picks (by their order on the page)
/// changed to `to`; the changed lines, and how many such values there are.
fn edit(text: &str, from: &str, to: &str, keep: impl Fn(usize) -> bool) -> (String, Vec<Change>, usize) {
    let mut changes = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut hit = 0;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let mut edited = String::new();
        let mut rest = 0;
        for value in owner_values(line).into_iter().filter(|v| same(&v.text, from)) {
            hit += 1;
            if !keep(hit - 1) {
                continue;
            }
            edited.push_str(&line[rest..value.start]);
            edited.push_str(&render(to.trim(), value.quote, value.flow));
            rest = value.end;
        }
        if rest == 0 {
            out.push_str(line);
            continue;
        }
        edited.push_str(&line[rest..]);
        changes.push(Change { line: n + 1, before: line.trim_end().into(), after: edited.trim_end().into() });
        out.push_str(&edited);
    }
    (out, changes, hit)
}

/// Where owners live in resources.yaml: `databases.<name>.<postgres|sqlserver>.owner`,
/// `apis|sftp|ldap|certificates.<name>.owner`.
fn resource_slots(config: &mut Value) -> Vec<(Kind, String, &mut Value)> {
    let Value::Mapping(top) = config else { return Vec::new() };
    let mut slots = Vec::new();
    for (key, value) in top.iter_mut() {
        let kind = match key.as_str() {
            Some("databases") => Kind::Database,
            Some("apis") => Kind::Api,
            Some("sftp") => Kind::Sftp,
            Some("ldap") => Kind::Ldap,
            Some("certificates") => Kind::Certificate,
            _ => continue,
        };
        let Value::Mapping(entries) = value else { continue };
        for (name, entry) in entries.iter_mut() {
            let (Some(name), Value::Mapping(m)) = (name.as_str(), entry) else { continue };
            let name = name.to_string();
            if kind == Kind::Database {
                for inner in m.values_mut() {
                    if let Value::Mapping(db) = inner {
                        slots.extend(db.get_mut("owner").map(|o| (kind, name.clone(), o)));
                    }
                }
            } else {
                slots.extend(m.get_mut("owner").map(|o| (kind, name, o)));
            }
        }
    }
    slots
}

/// Where owners live in interface.yaml: `about.owner`, `destination.owner`,
/// `destinations.<name>.owner`.
fn owner_slots(config: &mut Value) -> Vec<&mut Value> {
    let Value::Mapping(top) = config else { return Vec::new() };
    let mut slots = Vec::new();
    for (key, value) in top.iter_mut() {
        match (key.as_str(), value) {
            (Some("about" | "destination"), Value::Mapping(m)) => slots.extend(m.get_mut("owner")),
            (Some("destinations"), Value::Mapping(all)) => {
                for d in all.values_mut() {
                    if let Value::Mapping(m) = d {
                        slots.extend(m.get_mut("owner"));
                    }
                }
            }
            _ => {}
        }
    }
    slots
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    Plain,
    Single,
    Double,
}

/// An `owner:` value on a line: where it is (byte range, quotes included) and what it says.
#[derive(Debug)]
struct OwnerValue {
    start: usize,
    end: usize,
    text: String,
    quote: Quote,
    /// Inside `{ ... }`.
    flow: bool,
}

/// The `owner: value` pairs on one line, block (`  owner: Ann`) or flow (`{ owner: Ann }`).
/// Anything else that says "owner:" (`co_owner:`, inside a value) is skipped; the parse check
/// in `reassign` catches what this can't see.
fn owner_values(line: &str) -> Vec<OwnerValue> {
    let mut found = Vec::new();
    for (at, _) in line.match_indices("owner:") {
        let before = line[..at].trim_end();
        let flow = before.ends_with(['{', ',']);
        if !(before.is_empty() || flow) {
            continue;
        }
        let after = at + "owner:".len();
        let start = after + line[after..].len() - line[after..].trim_start_matches([' ', '\t']).len();
        if start == after && start < line.len() && !line[start..].starts_with(['\n', '\r']) {
            continue; // `owner:x` is not a key
        }
        let value = &line[start..];
        let (end, text, quote) = match value.chars().next() {
            Some('"') => {
                let mut escaped = false;
                let Some(close) = value[1..].find(|c| {
                    let hit = c == '"' && !escaped;
                    escaped = c == '\\' && !escaped;
                    hit
                }) else {
                    continue;
                };
                (start + close + 2, value[1..close + 1].replace("\\\"", "\"").replace("\\\\", "\\"), Quote::Double)
            }
            Some('\'') => {
                let bytes = value.as_bytes();
                let mut i = 1;
                while i < bytes.len() && !(bytes[i] == b'\'' && bytes.get(i + 1) != Some(&b'\'')) {
                    i += if bytes[i] == b'\'' { 2 } else { 1 };
                }
                if i >= bytes.len() {
                    continue;
                }
                (start + i + 1, value[1..i].replace("''", "'"), Quote::Single)
            }
            _ => {
                let mut stop = value.find(" #").unwrap_or(value.len());
                if flow {
                    stop = stop.min(value.find([',', '}']).unwrap_or(value.len()));
                }
                let text = value[..stop].trim_end();
                if text.is_empty() || text.starts_with(['#', '|', '>', '&', '*', '!', '[', '{']) {
                    continue;
                }
                (start + text.len(), text.to_string(), Quote::Plain)
            }
        };
        found.push(OwnerValue { start, end, text, quote, flow });
    }
    found
}

/// A reassignment as a unified diff of `path` (relative to the project, `/`-separated), for
/// `git apply` in the project's repository: how the web UI hands the change to whoever commits
/// it (the engine doesn't edit its own project). `after` must be `reassign`'s output for
/// `before`: lines are replaced one for one, never added or removed. Empty if nothing changed.
pub fn patch(path: &str, before: &str, after: &str) -> String {
    const CONTEXT: usize = 3;
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    debug_assert_eq!(old.len(), new.len(), "reassign keeps every line");
    let changed: Vec<usize> = (0..old.len().min(new.len())).filter(|&n| old[n] != new[n]).collect();
    if changed.is_empty() {
        return String::new();
    }
    // Hunks: changed lines with their context, merged where the context would overlap.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &n in &changed {
        let (start, end) = (n.saturating_sub(CONTEXT), (n + CONTEXT + 1).min(old.len()));
        match hunks.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => hunks.push((start, end)),
        }
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let line = |out: &mut String, sign: char, text: &str| {
        out.push(sign);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    for (start, end) in hunks {
        let len = end - start;
        out.push_str(&format!("@@ -{},{len} +{},{len} @@\n", start + 1, start + 1));
        for n in start..end {
            if old[n] == new[n] {
                line(&mut out, ' ', old[n]);
            } else {
                line(&mut out, '-', old[n]);
                line(&mut out, '+', new[n]);
            }
        }
    }
    out
}

/// The new value written like the old one: same quotes, or quoted if it can't stay plain.
fn render(value: &str, quote: Quote, flow: bool) -> String {
    let double = || format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    match quote {
        Quote::Double => double(),
        Quote::Single => format!("'{}'", value.replace('\'', "''")),
        Quote::Plain => {
            let special = value.starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@`".contains(c))
                || value.contains(": ")
                || value.contains(" #")
                || value.ends_with(':')
                || (flow && value.contains([',', '[', ']', '{', '}']))
                || value.parse::<f64>().is_ok()
                || ["true", "false", "yes", "no", "on", "off", "null", "~"].contains(&value.to_lowercase().as_str());
            if special { double() } else { value.to_string() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"name: results   # owner: Ann Smith (a comment, not a key)
source: { sftp: { server: quest, dir: /out } }
about:
  description: Results from Quest
  owner: Ann Smith            # since 2024
  from: { vendor: Quest }
destinations:
  emr: { mllp: { host: epic, port: 7000 }, owner: 'ann smith' }
  billing:
    http: { api: billing, headers: { co_owner: Ann Smith } }
    owner: "Billing team"
  archive: { file: { dir: out }, owner: Ann Smith, vendor: Local }
"#;

    fn config(yaml: &str) -> InterfaceConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn lists_owners_with_destinations_of_their_own() {
        let configs = [
            config(YAML),
            config(
                "name: adt\nsource: { mllp: { port: 1 } }\nabout: { owner: billing TEAM }\ndestination: { file: { dir: o } }",
            ),
            config("name: nightly\nsource: { schedule: { every: 1h } }\ndestination: { file: { dir: o }, owner: Ops }"),
        ];
        let resources: Resources = serde_yaml_ng::from_str(RESOURCES).unwrap();
        let groups: Vec<(String, Vec<String>)> = by_owner(owned(&configs, &resources))
            .into_iter()
            .map(|(owner, items)| (owner, items.into_iter().map(|o| o.what.label()).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                ("Ann Smith".to_string(), vec!["results".to_string(), "database patients".into()]),
                ("Billing team".into(), vec!["results → billing".into(), "adt".into(), "api billing".into()]),
                ("Directory Services".into(), vec!["directory corp-ad".into()]),
                ("Ops".into(), vec!["nightly → default".into()]),
                ("".into(), vec!["nightly".into()]),
            ],
            "emr and archive say the interface's owner (in other case), so they aren't listed apart; \
             resources without an owner aren't listed"
        );
    }

    const RESOURCES: &str = r#"# Shared connections
databases:
  patients:
    postgres: { host: db, database: p, user: u, password: x, owner: Ann Smith }
  billing_db:
    sqlserver:
      host: sql
      database: b
      user: u
      password: x
apis:
  billing: { url: "https://billing", owner: "billing TEAM" }
  empi:
    url: https://empi   # owner: Ann Smith (a comment, not a key)
ldap:
  corp-ad:
    url: ldaps://dc1
    bind_dn: cn=svc
    bind_password: x
    owner: Directory Services   # since the merger
"#;

    fn people() -> Vec<Person<'static>> {
        let p = |username, display_name, email, disabled| Person {
            username,
            display_name: Some(display_name),
            email: Some(email),
            disabled,
        };
        vec![
            p("anna", "Anna Smith", "anna@h.org", false),
            p("asmith2", "Anna Smith", "a.smith@h.org", false),
            p("olaf", "Olaf Night", "olaf@h.org", false),
            p("bob", "Bob Jones", "bob@h.org", true),
            // Someone whose username is another's display name: the username wins.
            p("Olaf Night", "Someone Else", "else@h.org", false),
        ]
    }

    #[test]
    fn owners_name_accounts_by_username_then_email_then_display_name() {
        let people = people();
        let user = |owner: &str| match find(owner, &people) {
            Found::One(p) => p.username.to_string(),
            Found::Several(n) => format!("{n} people"),
            Found::None => "nobody".into(),
        };
        assert_eq!(user("  ANNA "), "anna");
        assert_eq!(user("A.Smith@H.org"), "asmith2", "an email names its account");
        assert_eq!(user("olaf night"), "Olaf Night", "a username before a display name");
        assert_eq!(user("Anna Smith"), "2 people", "two display names: nobody is guessed");
        assert_eq!(user("Lab Systems"), "nobody");
        assert_eq!(user(""), "nobody");

        let teams: BTreeMap<String, OwnerContact> =
            [("Lab Systems".to_string(), OwnerContact { email: vec!["lab@h.org".into()] })].into();
        let reach = |owner: &str| reach(owner, &people, &teams);
        assert_eq!(reach("olaf@h.org"), Reach::User("olaf".into()));
        assert_eq!(reach("Bob Jones"), Reach::Disabled("bob".into()));
        assert_eq!(reach("Anna Smith"), Reach::Several(2));
        assert_eq!(reach("lab systems"), Reach::Team);
        assert_eq!(reach("oncall@h.org"), Reach::Address);
        assert_eq!(reach("Night shift"), Reach::Nobody);
        assert!(Reach::Team.ok() && Reach::Address.ok() && !Reach::Disabled("bob".into()).ok());
        assert!(!Reach::Several(2).ok() && !Reach::Nobody.ok());
    }

    #[test]
    fn reassigns_resources_yaml_entries_all_or_named() {
        let (out, changes) = reassign_resources(RESOURCES, "ann smith", "Bob Jones", &[]).unwrap();
        assert_eq!(
            changes.iter().map(|c| (c.kind, c.name.as_str(), c.change.line)).collect::<Vec<_>>(),
            [(Kind::Database, "patients", 4)],
            "not the comment"
        );
        assert_eq!(
            changes[0].change.after,
            "    postgres: { host: db, database: p, user: u, password: x, owner: Bob Jones }"
        );
        assert_eq!(out.lines().count(), RESOURCES.lines().count());
        let resources: Resources = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(resources.owner("patients"), Some("Bob Jones"));

        // Two entries with one owner; only one named.
        let two = RESOURCES.replace("owner: Directory Services", "owner: Billing Team");
        let (out, changes) = reassign_resources(&two, "Billing Team", "Finance", &["corp-ad".into()]).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].kind, changes[0].name.as_str()), (Kind::Ldap, "corp-ad"));
        assert_eq!(changes[0].change.after, "    owner: Finance   # since the merger");
        let resources: Resources = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!((resources.owner("billing"), resources.owner("corp-ad")), (Some("billing TEAM"), Some("Finance")));
        let (_, changes) = reassign_resources(&two, "Billing Team", "Finance", &[]).unwrap();
        assert_eq!(changes.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["billing", "corp-ad"]);

        // Nothing matching changes nothing; an owner value inside something else is refused.
        let (out, changes) = reassign_resources(RESOURCES, "Nobody", "X", &[]).unwrap();
        assert!(changes.is_empty() && out == RESOURCES);
        let odd =
            "apis:\n  a: { url: \"https://a\", owner: Ann }\n  b:\n    url: https://b\n    headers: { owner: Ann }\n";
        let err = reassign_resources(odd, "Ann", "Bob", &[]).unwrap_err().to_string();
        assert!(err.contains("by hand"), "{err}");
        assert!(reassign_resources(RESOURCES, "Ann Smith", " ", &[]).is_err());
    }

    #[test]
    fn reassigns_owner_values_and_keeps_everything_else() {
        let (out, changes) = reassign(YAML, "ANN SMITH", "Bob Jones").unwrap();
        assert_eq!(
            changes.iter().map(|c| c.line).collect::<Vec<_>>(),
            [5, 8, 12],
            "about, a quoted flow value and a plain flow value; not the comment or co_owner"
        );
        assert_eq!(changes[0].after, "  owner: Bob Jones            # since 2024");
        assert_eq!(changes[1].after, "  emr: { mllp: { host: epic, port: 7000 }, owner: 'Bob Jones' }");
        assert_eq!(changes[2].after, "  archive: { file: { dir: out }, owner: Bob Jones, vendor: Local }");
        assert_eq!(out.lines().count(), YAML.lines().count());
        assert!(out.contains("# owner: Ann Smith (a comment") && out.contains("co_owner: Ann Smith"));
        let c = config(&out);
        assert_eq!(c.about.owner.as_deref(), Some("Bob Jones"));
        assert_eq!(c.destinations["billing"].owner.as_deref(), Some("Billing team"));

        // Values that can't stay plain get quotes; nothing matching changes nothing.
        let (out, _) = reassign(YAML, "Billing team", "Ops: nights, #2").unwrap();
        assert!(out.contains(r#"owner: "Ops: nights, #2""#), "{out}");
        let (out, _) = reassign("about:\n  owner: Ann\n", "Ann", "yes").unwrap();
        assert_eq!(out, "about:\n  owner: \"yes\"\n");
        let (out, changes) = reassign(YAML, "Nobody", "X").unwrap();
        assert!(changes.is_empty() && out == YAML);
        assert!(reassign(YAML, "Ann Smith", "  ").is_err());
    }

    #[test]
    fn a_reassignment_as_a_patch_git_applies() {
        let (out, _) = reassign(YAML, "Ann Smith", "Bob Jones").unwrap();
        let diff = patch("interfaces/results/interface.yaml", YAML, &out);
        assert_eq!(
            diff,
            "--- a/interfaces/results/interface.yaml\n+++ b/interfaces/results/interface.yaml\n\
             @@ -2,11 +2,11 @@\n source: { sftp: { server: quest, dir: /out } }\n about:\n   description: Results from Quest\n\
             -  owner: Ann Smith            # since 2024\n+  owner: Bob Jones            # since 2024\n\
             \x20  from: { vendor: Quest }\n destinations:\n\
             -  emr: { mllp: { host: epic, port: 7000 }, owner: 'ann smith' }\n\
             +  emr: { mllp: { host: epic, port: 7000 }, owner: 'Bob Jones' }\n\
             \x20  billing:\n     http: { api: billing, headers: { co_owner: Ann Smith } }\n     owner: \"Billing team\"\n\
             -  archive: { file: { dir: out }, owner: Ann Smith, vendor: Local }\n\
             +  archive: { file: { dir: out }, owner: Bob Jones, vendor: Local }\n"
        );
        assert_eq!(patch("x.yaml", YAML, YAML), "", "nothing changed, no patch");

        // Far apart: two hunks. No newline at the end: said the way git says it.
        let far =
            format!("about:\n  owner: Ann\n{}destination: {{ file: {{ dir: o }}, owner: Ann }}", "# -\n".repeat(8));
        let (out, _) = reassign(&far, "Ann", "Bob").unwrap();
        let diff = patch("i.yaml", &far, &out);
        assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
        assert!(diff.ends_with("+destination: { file: { dir: o }, owner: Bob }\n\\ No newline at end of file\n"));

        // A Windows checkout has CRLF (Git for Windows sets core.autocrlf): the edit and the patch
        // keep the file's own line endings, the only patch `git apply` takes with autocrlf on or off.
        let crlf = |s: &str| s.replace('\n', "\r\n");
        let results = reassign(YAML, "Ann Smith", "Bob Jones").unwrap().0;
        let (yaml_crlf, far_crlf) = (crlf(YAML), crlf(&far));
        let results_crlf = reassign(&yaml_crlf, "Ann Smith", "Bob Jones").unwrap().0;
        let out_crlf = reassign(&far_crlf, "Ann", "Bob").unwrap().0;
        assert_eq!((&results_crlf, &out_crlf), (&crlf(&results), &crlf(&out)), "CRLF kept");
        assert!(patch("i.yaml", &far_crlf, &out_crlf).contains("+  owner: Bob\r\n"));

        // The real check: git applies each (skipped where git isn't installed). With autocrlf on,
        // git writes LF files back as CRLF, so it's only given CRLF files, as a checkout would be.
        let dir = std::env::temp_dir().join(format!("ductus-patch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("interfaces/results")).unwrap();
        for (autocrlf, file, before, after) in [
            ("false", "interfaces/results/interface.yaml", YAML, results.as_str()),
            ("false", "i.yaml", far.as_str(), out.as_str()),
            ("false", "crlf.yaml", yaml_crlf.as_str(), results_crlf.as_str()),
            ("true", "crlf.yaml", yaml_crlf.as_str(), results_crlf.as_str()),
            ("true", "i.yaml", far_crlf.as_str(), out_crlf.as_str()),
        ] {
            std::fs::write(dir.join(file), before).unwrap();
            std::fs::write(dir.join("change.patch"), patch(file, before, after)).unwrap();
            let Ok(status) = std::process::Command::new("git")
                .args(["-c", &format!("core.autocrlf={autocrlf}"), "apply", "change.patch"])
                .current_dir(&dir)
                .status()
            else {
                eprintln!("git not found: patch not applied");
                break;
            };
            assert!(status.success(), "git apply failed for {file} (autocrlf {autocrlf})");
            assert_eq!(std::fs::read_to_string(dir.join(file)).unwrap(), after, "{file} (autocrlf {autocrlf})");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_an_edit_it_cant_verify() {
        // A header named `owner` in flow style looks like an owner line but isn't one.
        let yaml =
            "name: x\nabout: { owner: Ann }\ndestination:\n  http: { url: 'http://h', headers: { owner: Ann } }\n";
        let err = reassign(yaml, "Ann", "Bob").unwrap_err().to_string();
        assert!(err.contains("change `owner: Ann` by hand"), "{err}");
        // Environment references survive.
        let (out, _) = reassign("about:\n  owner: Ann\n  description: ${DESC}\n", "Ann", "Bob").unwrap();
        assert_eq!(out, "about:\n  owner: Bob\n  description: ${DESC}\n");
    }
}
