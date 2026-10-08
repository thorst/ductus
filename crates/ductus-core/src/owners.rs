//! Who to call about what: `about.owner` per interface, and `owner` on a destination whose
//! owner is someone else. `ductus owners` lists them; `ductus owners reassign` moves ownership
//! (someone leaves, a team splits) by editing the `owner:` values in interface.yaml in place,
//! so comments and layout stay as they were.

use crate::InterfaceConfig;
use crate::about::given;
use anyhow::{Result, bail};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

/// Something owned: an interface, or one of its destinations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    /// Empty when nobody is named.
    pub owner: String,
    pub interface: String,
    /// Set for a destination whose owner differs from its interface's.
    pub destination: Option<String>,
}

/// One entry per interface, plus one per destination with an owner of its own (other
/// destinations belong to their interface's owner).
pub fn owned<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<Owned> {
    let mut out = Vec::new();
    for c in configs {
        let owner = given(&c.about.owner).unwrap_or("");
        out.push(Owned { owner: owner.into(), interface: c.name.clone(), destination: None });
        for (name, d) in c.destinations() {
            if let Some(own) = given(&d.owner).filter(|o| !same(o, owner)) {
                out.push(Owned { owner: own.into(), interface: c.name.clone(), destination: Some(name.into()) });
            }
        }
    }
    out
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
    let mut changes = Vec::new();
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let mut edited = String::new();
        let mut rest = 0;
        for value in owner_values(line).into_iter().filter(|v| same(&v.text, from)) {
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

    let mut expected = crate::env::parse_yaml(text)?;
    for owner in owner_slots(&mut expected) {
        if owner.as_str().is_some_and(|o| same(o, from)) {
            *owner = Value::String(to.trim().into());
        }
    }
    let edited = crate::env::parse_yaml(&out);
    if !edited.as_ref().is_ok_and(|e| *e == expected) {
        bail!("couldn't change the owner lines safely (an unusual layout?): change `owner: {from}` by hand");
    }
    Ok((out, changes))
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
        let groups: Vec<(String, Vec<String>)> = by_owner(owned(&configs))
            .into_iter()
            .map(|(owner, items)| {
                let items = items
                    .into_iter()
                    .map(|o| match o.destination {
                        Some(d) => format!("{} → {d}", o.interface),
                        None => o.interface,
                    })
                    .collect();
                (owner, items)
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("Ann Smith".to_string(), vec!["results".to_string()]),
                ("Billing team".into(), vec!["results → billing".into(), "adt".into()]),
                ("Ops".into(), vec!["nightly → default".into()]),
                ("".into(), vec!["nightly".into()]),
            ],
            "emr and archive say the interface's owner (in other case), so they aren't listed apart"
        );
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
