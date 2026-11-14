//! What a deploy changes, by kind (0.7.5): for each interface, whether the change can be applied
//! in place (its workers restarted between messages, nothing else touched) or needs the full
//! handover, and which change decided it. Worked out from interface.yaml **as written** (the same
//! text the code version hashes, before `${...}`) and the code versions: no gate, microseconds.
//!
//! In place: the code (any file a version counts), `script`, `destinations` (added, removed or
//! changed), `output`, `retry`, `rate_limit`, `timeout`, and what only people and alerts read
//! (`about`, `custom`, `monitor`, `alert`). Everything else in interface.yaml shapes how messages
//! are received, stored or laid out in lanes (`source`, `format`, `ordering`, `isolation`,
//! `workers`, `dedicated_pools`, `durability`, `dedupe`, `retention`, `quarantined`): those, an
//! interface added or removed, and the project's own files (`ductus.yaml`, `resources.yaml`,
//! `environments/`, `secrets/`) go by the handover, the answer saying which (0.7.5.2 moves sources
//! and added or removed interfaces in place).

use anyhow::{Context, Result};
use ductus_core::yaml::Value;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// interface.yaml's keys a change to which is applied in place.
const IN_PLACE: &[&str] =
    &["script", "destinations", "output", "retry", "rate_limit", "timeout", "about", "custom", "monitor", "alert"];

/// How one interface's change is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// Its workers restarted between messages on its new code and settings.
    InPlace,
    /// Only by the full handover; why, in words.
    Handover(String),
}

/// What changes for one interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub interface: String,
    /// What changed, in words: `code`, `destinations.lab`, `destination lab added`, `source`, ...
    pub what: Vec<String>,
    pub apply: Apply,
}

/// Every interface's change and whether the whole deploy can go in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Interfaces whose version changes, added or removed, by name.
    pub changes: Vec<Change>,
    /// Why the project's own files need the handover; None when they don't change.
    pub project: Option<String>,
}

impl Plan {
    /// Every change applies in place (nothing changed counts too).
    pub fn in_place(&self) -> bool {
        self.project.is_none() && self.changes.iter().all(|c| c.apply == Apply::InPlace)
    }

    /// Why the deploy needs the handover: one line per reason; empty when it doesn't.
    pub fn handover_because(&self) -> Vec<String> {
        let mut why: Vec<String> = self.project.iter().cloned().collect();
        for c in &self.changes {
            if let Apply::Handover(reason) = &c.apply {
                why.push(format!("{}: {reason}", c.interface));
            }
        }
        why
    }
}

/// One side of a deploy, per interface: its code version (hex) and its interface.yaml as written.
pub type Side<'a> = BTreeMap<&'a str, (&'a str, &'a str)>;

/// The plan for going from `running` to `next`. `project_changed`: their project versions differ.
pub fn plan(running: &Side, next: &Side, project_changed: bool) -> Result<Plan> {
    let mut changes = Vec::new();
    let names: BTreeSet<&str> = running.keys().chain(next.keys()).copied().collect();
    for name in names {
        let change = match (running.get(name), next.get(name)) {
            (Some((before, _)), Some((after, _))) if before == after => continue,
            (Some((_, before)), Some((_, after))) => interface(name, before, after)?,
            (None, Some(_)) => Change {
                interface: name.into(),
                what: vec!["added".into()],
                apply: Apply::Handover("a new interface (it's claimed, received and listened for at a start)".into()),
            },
            (Some(_), None) => Change {
                interface: name.into(),
                what: vec!["removed".into()],
                apply: Apply::Handover("removed (its claim and listener end with the engine)".into()),
            },
            (None, None) => unreachable!("from either side"),
        };
        changes.push(change);
    }
    let project = project_changed.then(|| {
        "the project's own files changed (ductus.yaml, resources.yaml, environments/ or secrets/): shared settings, \
         pools and secrets are read at a start"
            .to_string()
    });
    Ok(Plan { changes, project })
}

/// One interface whose version changed: which keys of its interface.yaml differ, and how it applies.
fn interface(name: &str, before: &str, after: &str) -> Result<Change> {
    let read = |text: &str| -> Result<BTreeMap<String, Value>> {
        match ductus_core::yaml::parse(text).with_context(|| format!("reading {name}'s interface.yaml"))? {
            Value::Null => Ok(BTreeMap::new()),
            value => Ok(entries(Some(&value)).with_context(|| format!("{name}'s interface.yaml isn't a mapping"))?),
        }
    };
    let (before, after) = (read(before)?, read(after)?);
    let keys: BTreeSet<String> = before.keys().chain(after.keys()).cloned().collect();
    let mut what = Vec::new();
    let mut handover = Vec::new();
    for key in keys {
        let (b, a) = (before.get(&key), after.get(&key));
        if b == a {
            continue;
        }
        if key == "destinations" {
            what.extend(destinations(b, a));
        } else {
            what.push(key.clone());
        }
        if !IN_PLACE.contains(&key.as_str()) {
            handover.push(key);
        }
    }
    if what.is_empty() {
        what.push("code".into());
    }
    let apply = match handover.len() {
        0 => Apply::InPlace,
        _ => Apply::Handover(format!(
            "{} changed (how messages are received, stored or laid out in lanes is set at a start)",
            handover.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        )),
    };
    Ok(Change { interface: name.into(), what, apply })
}

/// A mapping's entries by their text keys; None when it isn't one.
fn entries(value: Option<&Value>) -> Option<BTreeMap<String, Value>> {
    let mapping = value?.as_mapping()?;
    Some(mapping.iter().filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.clone()))).collect())
}

/// Each destination added, removed or changed, in words.
fn destinations(before: Option<&Value>, after: Option<&Value>) -> Vec<String> {
    let (before, after) = (entries(before).unwrap_or_default(), entries(after).unwrap_or_default());
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    names
        .into_iter()
        .filter_map(|n| match (before.get(n), after.get(n)) {
            (Some(b), Some(a)) if b == a => None,
            (Some(_), Some(_)) => Some(format!("destinations.{n}")),
            (None, Some(_)) => Some(format!("destination {n} added")),
            (Some(_), None) => Some(format!("destination {n} removed")),
            (None, None) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAB: &str = "name: adt\nsource: { mllp: { port: 6661 } }\ndestinations:\n  lab: { mllp: { host: lab, port: 7001 }, script: lab.ts }\n";

    fn side<'a>(entries: &[(&'a str, &'a str, &'a str)]) -> Side<'a> {
        entries.iter().map(|(n, v, y)| (*n, (*v, *y))).collect()
    }

    /// Code only, destinations added, removed or changed, and the settings workers read: in place.
    #[test]
    fn code_and_destinations_go_in_place() {
        let added = format!("{LAB}  archive: {{ file: {{ dir: out }} }}\nretry: {{ max_attempts: 3 }}\n");
        let changed = LAB.replace("7001", "7002");
        let running = side(&[("adt", "v1", LAB), ("b", "b1", LAB), ("c", "c1", LAB), ("same", "s", LAB)]);
        let next = side(&[("adt", "v2", LAB), ("b", "b2", &added), ("c", "c2", &changed), ("same", "s", LAB)]);
        let plan = plan(&running, &next, false).unwrap();
        assert!(plan.in_place(), "{plan:#?}");
        let what: Vec<(&str, Vec<&str>)> =
            plan.changes.iter().map(|c| (c.interface.as_str(), c.what.iter().map(String::as_str).collect())).collect();
        assert_eq!(
            what,
            [("adt", vec!["code"]), ("b", vec!["destination archive added", "retry"]), ("c", vec!["destinations.lab"])]
        );
        let removed = side(&[("adt", "v3", "name: adt\nsource: { mllp: { port: 6661 } }\ndestinations: {}\n")]);
        let plan = super::plan(&side(&[("adt", "v1", LAB)]), &removed, false).unwrap();
        assert_eq!(plan.changes[0].what, ["destination lab removed"]);
        assert!(plan.in_place());
    }

    /// A source, lanes, an interface added or removed, or the project's files: by the handover,
    /// each saying why.
    #[test]
    fn what_shapes_receiving_or_lanes_needs_the_handover() {
        let port = LAB.replace("6661", "6662");
        let lanes = format!("{LAB}ordering: {{ key: PID-3.1 }}\n");
        let running = side(&[("adt", "v1", LAB), ("b", "b1", LAB), ("gone", "g", LAB)]);
        let next = side(&[("adt", "v2", &port), ("b", "b2", &lanes), ("new", "n", LAB)]);
        let plan = plan(&running, &next, true).unwrap();
        assert!(!plan.in_place());
        let why = plan.handover_because();
        assert!(why[0].starts_with("the project's own files changed"), "{why:#?}");
        assert!(why[1].starts_with("adt: `source` changed"), "{why:#?}");
        assert!(why[2].starts_with("b: `ordering` changed"), "{why:#?}");
        assert!(why[3].starts_with("gone: removed"), "{why:#?}");
        assert!(why[4].starts_with("new: a new interface"), "{why:#?}");
        assert!(super::plan(&running, &running, false).unwrap().changes.is_empty(), "nothing changes");
    }

    /// Formatting alone (quotes, flow or block style, key order) isn't a change of settings.
    #[test]
    fn the_yaml_is_compared_as_values_not_text() {
        let block = "name: adt\nsource:\n  mllp:\n    port: 6661\ndestinations:\n  lab:\n    script: lab.ts\n    mllp:\n      port: 7001\n      host: \"lab\"\n";
        let plan = plan(&side(&[("adt", "v1", LAB)]), &side(&[("adt", "v2", block)]), false).unwrap();
        assert_eq!(plan.changes[0].what, ["code"], "{plan:#?}");
    }
}
