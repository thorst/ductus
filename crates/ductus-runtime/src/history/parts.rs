//! What a history entry touches (0.7.4.2): the interface's source, some of its destinations, or
//! the interface as a whole only, so `ductus history <interface> --destination lab` (and the
//! History tab's picker) shows what changed for that connection and nothing else.
//!
//! - **interface.yaml** (a deployment's two versions, from `code_files`; a commit's two blobs) is
//!   compared block by block, as [`yaml`] lists.
//! - **Code** is compared by the files each part runs (`code_versions.parts`: each destination's
//!   function's file and everything it imports, the HTTP `respond` answer's for the source): a
//!   change of a file several destinations load (`lib/`, the interface's script) touches each of
//!   them. Versions saved before 0.7.4.2 have no parts: a change of their code is shown under
//!   every part, marked as not known ([`Touches::unsure`]), never guessed.
//! - **The audit log**: a pause, a resume or a destination's alert by its subject; a resend,
//!   correction or dismissal by the destinations its message actions name; raised lanes are the
//!   source's. `ductus_store`'s `history_audit` filters by the same rules.
//! - **Documentation**: `destinations.<name>.…` fields are that destination's; the rest (the
//!   description, the owner, `custom`) the whole interface's.

use crate::code::Parts;
use ductus_core::yaml::Value;
use ductus_store::VersionFiles;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use utoipa::ToSchema;

/// One part of an interface: what a history is narrowed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Source,
    Destination(String),
}

impl Part {
    pub fn store(&self) -> ductus_store::Part<'_> {
        match self {
            Part::Source => ductus_store::Part::Source,
            Part::Destination(name) => ductus_store::Part::Destination(name),
        }
    }

    /// "the source", "destination `lab`".
    pub fn label(&self) -> String {
        match self {
            Part::Source => "the source".into(),
            Part::Destination(name) => format!("destination `{name}`"),
        }
    }
}

/// The parts of the interface an entry touches. None of them: the interface as a whole only (its
/// owner, its retention, its alerts' recipients), shown only when no part is picked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = HistoryTouches)]
pub struct Touches {
    /// Every part alike: the interface came or went (or was renamed), its interface.yaml stopped or
    /// started parsing, a start ran it unchanged.
    pub everything: bool,
    /// Where messages come from: `source:`, `format`, `ordering`, `dedupe`, `durability`,
    /// `about.from`, the `monitor` times; an HTTP `respond` interface's code.
    pub source: bool,
    /// The destinations it touches, by name, in name order.
    pub destinations: Vec<String>,
    /// Its code changed, but which part's wasn't recorded (a version saved before 0.7.4.2, or a
    /// file only such versions named): it may touch any part, so every part's history shows it,
    /// marked.
    pub unsure: bool,
}

impl Touches {
    fn everything() -> Self {
        Self { everything: true, ..Self::default() }
    }

    fn destination(&mut self, name: &str) {
        if let Err(at) = self.destinations.binary_search_by(|d| d.as_str().cmp(name)) {
            self.destinations.insert(at, name.to_string());
        }
    }

    /// These and `other`'s.
    pub fn with(mut self, other: Touches) -> Self {
        self.add(other);
        self
    }

    fn add(&mut self, other: Touches) {
        self.everything |= other.everything;
        self.source |= other.source;
        self.unsure |= other.unsure;
        for d in &other.destinations {
            self.destination(d);
        }
    }

    /// Whether `part`'s history shows it: it touches that part, or may (`unsure`).
    pub fn shows(&self, part: &Part) -> bool {
        self.unsure || self.surely(part)
    }

    /// Whether it's known to touch `part`.
    pub fn surely(&self, part: &Part) -> bool {
        self.everything
            || match part {
                Part::Source => self.source,
                Part::Destination(name) => self.destinations.iter().any(|d| d == name),
            }
    }

    /// Whether it names any part (else it's the whole interface's only).
    pub fn any(&self) -> bool {
        self.everything || self.source || self.unsure || !self.destinations.is_empty()
    }

    /// In a line of text: "source, lab", "everything", "not known (before 0.7.4.2)".
    pub fn words(&self) -> String {
        if self.everything {
            return "every part".into();
        }
        let mut named: Vec<String> = Vec::new();
        if self.source {
            named.push("source".into());
        }
        named.extend(self.destinations.iter().cloned());
        if self.unsure {
            named.push("code of parts not recorded (before 0.7.4.2)".into());
        }
        named.join(", ")
    }
}

/// Which parts a change of interface.yaml from `before` to `after` touches (None: the file isn't
/// there). Block by block, each compared as parsed (comments, spacing and key order don't count):
///
/// | blocks | part |
/// |---|---|
/// | `destinations.<name>` (and the singular `destination:` before 0.7.4, as `default`) | that destination |
/// | `source`, `format`, `ordering`, `dedupe`, `durability`, `about.from`, `monitor`'s times | the source |
/// | `script`, `output`, `retry`, `rate_limit`, `monitor.behind` | each destination without its own |
/// | `timeout`, `isolation`, `workers`, `dedicated_pools`, `about.to` | every destination |
/// | `about`'s description and owner, `custom`, `retention`, `quarantined`, `alert` | the whole interface only |
/// | `name`, the file added or removed, a text that doesn't parse, a block this version doesn't know | every part |
///
/// "Each destination" on an interface without destinations (HTTP `respond`) is its source.
pub fn yaml(before: Option<&str>, after: Option<&str>) -> Touches {
    if before == after {
        return Touches::default();
    }
    let (Some(before), Some(after)) = (before, after) else { return Touches::everything() };
    let (Ok(before), Ok(after)) = (ductus_core::yaml::parse(before), ductus_core::yaml::parse(after)) else {
        return Touches::everything();
    };
    let (Some(b), Some(a)) = (before.as_mapping(), after.as_mapping()) else { return Touches::everything() };
    let mut touches = Touches::default();
    // Destinations as either version has them, and which of them have a setting of their own.
    let names: BTreeSet<String> = [b, a].iter().flat_map(|m| keys(m.get("destinations"))).collect();
    let own = |setting: &str| -> BTreeSet<String> {
        names
            .iter()
            .filter(|n| {
                [b, a].iter().all(|m| {
                    let block = m.get("destinations").and_then(|d| d.get(n.as_str()));
                    // Absent from one version: it's that destination's own change (`destinations`).
                    block.is_none_or(|block| block.get(setting).is_some())
                })
            })
            .cloned()
            .collect()
    };
    let each = |touches: &mut Touches, except: &BTreeSet<String>| {
        if names.is_empty() {
            touches.source = true;
        }
        for name in names.iter().filter(|n| !except.contains(*n)) {
            touches.destination(name);
        }
    };
    let none = BTreeSet::new();
    let top: BTreeSet<String> = keys(Some(&before)).into_iter().chain(keys(Some(&after))).collect();
    for key in &top {
        let (x, y) = (b.get(key.as_str()), a.get(key.as_str()));
        if x == y {
            continue;
        }
        match key.as_str() {
            "destinations" => {
                for name in &names {
                    if x.and_then(|v| v.get(name.as_str())) != y.and_then(|v| v.get(name.as_str())) {
                        touches.destination(name);
                    }
                }
            }
            "destination" => touches.destination("default"),
            "source" | "format" | "ordering" | "dedupe" | "durability" => touches.source = true,
            "script" | "output" | "retry" | "rate_limit" => each(&mut touches, &own(key)),
            "timeout" | "isolation" | "workers" | "dedicated_pools" => each(&mut touches, &none),
            "about" | "monitor" => {
                let subs: BTreeSet<String> = keys(x).into_iter().chain(keys(y)).collect();
                for sub in subs {
                    if x.and_then(|v| v.get(sub.as_str())) == y.and_then(|v| v.get(sub.as_str())) {
                        continue;
                    }
                    match (key.as_str(), sub.as_str()) {
                        ("about", "from") => touches.source = true,
                        ("about", "to") => each(&mut touches, &none),
                        ("about", _) => {} // its description and owner: the whole interface's
                        ("monitor", "behind") => {
                            let own: BTreeSet<String> = names
                                .iter()
                                .filter(|n| {
                                    [b, a].iter().all(|m| {
                                        let block = m.get("destinations").and_then(|d| d.get(n.as_str()));
                                        block.is_none_or(|block| {
                                            block.get("monitor").and_then(|m| m.get("behind")).is_some()
                                        })
                                    })
                                })
                                .cloned()
                                .collect();
                            each(&mut touches, &own);
                        }
                        _ => touches.source = true, // `active_within`, `between`, `active_by`, `from`
                    }
                }
            }
            "custom" | "retention" | "quarantined" | "alert" => {}
            _ => return Touches::everything(), // `name`, or a block this version doesn't know
        }
    }
    touches
}

/// A mapping's keys as text (none when it isn't one).
fn keys(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_mapping)
        .map(|m| m.keys().filter_map(|k| k.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn is_yaml(path: &str) -> bool {
    path.rsplit('/').next() == Some("interface.yaml")
}

fn parts(version: &VersionFiles) -> Option<Parts> {
    serde_json::from_str(version.parts.as_deref()?).ok()
}

/// Which parts a change of code from `from` to `to` touches: each part whose files (by path and
/// content) differ. A version without parts (saved before 0.7.4.2) can't say: `unsure` when the
/// code differs at all. interface.yaml isn't code here ([`yaml`]).
pub fn code(from: &VersionFiles, to: &VersionFiles) -> Touches {
    fn code(v: &VersionFiles) -> HashMap<&str, &str> {
        v.files.iter().filter(|(p, _)| !is_yaml(p)).map(|(p, h)| (p.as_str(), h.as_str())).collect()
    }
    let (was, now) = (code(from), code(to));
    if was == now {
        return Touches::default();
    }
    let (Some(a), Some(b)) = (parts(from), parts(to)) else { return Touches { unsure: true, ..Touches::default() } };
    let of = |files: &[String], hashes: &HashMap<&str, &str>| -> BTreeSet<(String, Option<String>)> {
        files.iter().map(|p| (p.clone(), hashes.get(p.as_str()).map(|h| h.to_string()))).collect()
    };
    let mut touches = Touches { source: of(&a.source, &was) != of(&b.source, &now), ..Touches::default() };
    let names: BTreeSet<&String> = a.destinations.keys().chain(b.destinations.keys()).collect();
    for name in names {
        let files = |p: &Parts| p.destinations.get(name).cloned().unwrap_or_default();
        if of(&files(&a), &was) != of(&files(&b), &now) {
            touches.destination(name);
        }
    }
    touches
}

/// Which parts each file of an interface belongs to, from every version recorded (and what the
/// project runs now): what tells which parts a commit on a file touches.
#[derive(Debug, Default)]
pub struct Owners {
    by_path: HashMap<String, Touches>,
    /// Files only versions without parts named (saved before 0.7.4.2).
    unknown: HashSet<String>,
}

impl Owners {
    pub fn of<'a>(versions: impl IntoIterator<Item = &'a VersionFiles>) -> Self {
        let mut owners = Self::default();
        let mut known: HashSet<String> = HashSet::new();
        for v in versions {
            match parts(v) {
                Some(p) => {
                    known.extend(v.files.iter().map(|(path, _)| path.clone()));
                    for path in &p.source {
                        owners.by_path.entry(path.clone()).or_default().source = true;
                    }
                    for (name, paths) in &p.destinations {
                        for path in paths {
                            owners.by_path.entry(path.clone()).or_default().destination(name);
                        }
                    }
                }
                None => owners.unknown.extend(v.files.iter().map(|(path, _)| path.clone())),
            }
        }
        owners.unknown.retain(|p| !known.contains(p));
        owners
    }

    /// Which parts a commit touches: each interface.yaml it changed compared ([`yaml`]), each other
    /// file by the parts that load it. A file loaded by no part (the interface's script when every
    /// destination has its own) is the whole interface's.
    pub fn commit(&self, c: &crate::git::Change) -> Touches {
        let mut touches = Touches::default();
        for (path, _) in &c.files {
            if is_yaml(path) {
                if let Some(y) = c.yaml.iter().find(|y| &y.path == path) {
                    touches.add(yaml(y.before.as_deref(), y.after.as_deref()));
                }
            } else if let Some(t) = self.by_path.get(path) {
                touches.add(t.clone());
            } else if self.unknown.contains(path) {
                touches.unsure = true;
            }
        }
        touches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "name: a
source: { mllp: { port: 6661 } }
script: shared.ts
retry: { max_attempts: 5 }
about: { owner: ann, from: { application: Epic } }
destinations:
  lab: { mllp: { host: lab, port: 7001 }, script: lab.ts }
  billing: { mllp: { host: billing, port: 7002 }, retry: { max_attempts: 9 } }
";

    fn change(from: &str, to: &str) -> (bool, bool, Vec<String>) {
        let t = yaml(Some(BASE), Some(&BASE.replace(from, to)));
        (t.everything, t.source, t.destinations)
    }

    #[test]
    fn interface_yaml_is_compared_block_by_block() {
        let none: Vec<String> = Vec::new();
        assert_eq!(change("port: 6661", "port: 6662"), (false, true, none.clone()), "source");
        assert_eq!(change("port: 7001", "port: 7003"), (false, false, vec!["lab".into()]), "a destination's block");
        assert_eq!(change("owner: ann", "owner: bob"), (false, false, none.clone()), "the owner: the whole only");
        assert_eq!(change("Epic", "Cerner"), (false, true, none.clone()), "about.from: the source");
        assert_eq!(
            change("retry: { max_attempts: 5 }", "retry: { max_attempts: 6 }"),
            (false, false, vec!["lab".into()]),
            "the interface's retry: only destinations without their own"
        );
        assert_eq!(
            change("script: shared.ts", "script: other.ts"),
            (false, false, vec!["billing".into()]),
            "the interface's script: only destinations without their own"
        );
        assert_eq!(
            change("about: {", "timeout: 1s\nabout: {"),
            (false, false, vec!["billing".into(), "lab".into()]),
            "a timeout: every destination"
        );
        assert_eq!(
            change("  billing:", "  archive: { file: { dir: out } }\n  billing:"),
            (false, false, vec!["archive".into()]),
            "a destination added"
        );
        assert_eq!(change("name: a", "name: b"), (true, false, none.clone()), "renamed");
        assert_eq!(change("about: {", "brand_new: 1\nabout: {"), (true, false, none.clone()), "unknown block");
        assert_eq!(change("# nothing", "# nothing"), (false, false, none.clone()));
        assert!(yaml(Some(BASE), Some(&format!("{BASE}# a comment\n"))) == Touches::default(), "comments don't count");
        assert!(yaml(None, Some(BASE)).everything, "added");
        assert!(yaml(Some(BASE), Some("source: [")).everything, "doesn't parse");
        let old = "name: a\nsource: { mllp: { port: 1 } }\ndestination: { mllp: { host: x, port: 2 } }\n";
        let new = "name: a\nsource: { mllp: { port: 1 } }\ndestinations: { lab: { mllp: { host: x, port: 2 } } }\n";
        assert_eq!(
            yaml(Some(old), Some(new)).destinations,
            ["default", "lab"],
            "the singular destination: was `default`"
        );
        let respond = "name: r\nsource: { http: { port: 1, mode: respond } }\nscript: a.ts\n";
        let t = yaml(Some(respond), Some(&respond.replace("a.ts", "b.ts")));
        assert!(t.source && t.destinations.is_empty(), "no destinations: the source's code");
    }

    fn version(files: &[(&str, &str)], parts: Option<&str>) -> VersionFiles {
        VersionFiles {
            files: files.iter().map(|(p, h)| (p.to_string(), h.to_string())).collect(),
            parts: parts.map(str::to_string),
        }
    }

    const PARTS: &str = r#"{"source": [], "destinations": {"lab": ["i/lab.ts", "lib/x.ts"], "billing": ["i/billing.ts", "lib/x.ts"], "archive": []}}"#;

    #[test]
    fn code_is_compared_by_each_part_s_files() {
        let a = version(
            &[("i/interface.yaml", "1"), ("i/lab.ts", "1"), ("i/billing.ts", "1"), ("lib/x.ts", "1")],
            Some(PARTS),
        );
        let lab = version(
            &[("i/interface.yaml", "2"), ("i/lab.ts", "2"), ("i/billing.ts", "1"), ("lib/x.ts", "1")],
            Some(PARTS),
        );
        assert_eq!(code(&a, &lab).destinations, ["lab"]);
        let shared = version(
            &[("i/interface.yaml", "1"), ("i/lab.ts", "1"), ("i/billing.ts", "1"), ("lib/x.ts", "2")],
            Some(PARTS),
        );
        assert_eq!(code(&a, &shared).destinations, ["billing", "lab"], "a shared file: each destination that loads it");
        assert_eq!(code(&a, &a), Touches::default());
        let yaml_only =
            version(&[("i/interface.yaml", "9"), ("i/lab.ts", "1"), ("i/billing.ts", "1"), ("lib/x.ts", "1")], None);
        assert_eq!(code(&a, &yaml_only), Touches::default(), "the same code: nothing, parts or not");
        let old =
            version(&[("i/interface.yaml", "1"), ("i/lab.ts", "0"), ("i/billing.ts", "1"), ("lib/x.ts", "1")], None);
        let t = code(&old, &a);
        assert!(t.unsure && t.destinations.is_empty(), "before 0.7.4.2: not guessed");
        assert!(t.shows(&Part::Destination("billing".into())) && t.shows(&Part::Source), "shown under every part");
    }

    #[test]
    fn a_commit_touches_the_parts_that_load_its_files() {
        let owners = Owners::of(&[
            version(
                &[("i/interface.yaml", "1"), ("i/lab.ts", "1"), ("i/billing.ts", "1"), ("lib/x.ts", "1")],
                Some(PARTS),
            ),
            version(&[("i/interface.yaml", "0"), ("i/old.ts", "1")], None),
        ]);
        let commit = |files: &[&str]| crate::git::Change {
            id: "c".into(),
            at: chrono::DateTime::default(),
            author: String::new(),
            email: String::new(),
            summary: String::new(),
            files: files.iter().map(|f| (f.to_string(), crate::git::How::Modified)).collect(),
            yaml: Vec::new(),
        };
        assert_eq!(owners.commit(&commit(&["i/lab.ts"])).destinations, ["lab"]);
        assert_eq!(owners.commit(&commit(&["lib/x.ts"])).destinations, ["billing", "lab"]);
        assert!(owners.commit(&commit(&["i/old.ts"])).unsure, "only a version before 0.7.4.2 named it");
        let mut c = commit(&["i/interface.yaml"]);
        c.yaml.push(crate::git::Yaml {
            path: "i/interface.yaml".into(),
            before: Some(BASE.into()),
            after: Some(BASE.replace("port: 7002", "port: 7004")),
        });
        assert_eq!(owners.commit(&c).destinations, ["billing"]);
    }
}
