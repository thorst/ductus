//! What a deploy changes, by kind (0.7.5): for each interface, whether the change can be applied
//! in place (its workers restarted between messages, nothing else touched) or needs the full
//! handover, and which change decided it. Worked out from the code versions and interface.yaml
//! **filled in** with each side's own environment file (0.7.5.2.3: `${NAME}` from the running
//! release's `environments/<env>.yaml` before, the next one's after; `${secret:..}` left as
//! written, never decrypted): no gate, microseconds. So a variable whose value changes reaches
//! every interface whose filled-in settings it moves, judged by the same rules as an edit of
//! interface.yaml, and every interface whose code names it as text (reloaded, so `env()` read when
//! a module loads gets the new value too). The code version stays the hash of interface.yaml as
//! written (a release has no environment), so what a variable changed is recorded beside it
//! (`Change::variables`).
//!
//! Every change to an interface goes in place, one of two ways:
//! - **Between two messages** (`Apply::InPlace`): the code (any file a version counts), `script`,
//!   `destinations` (added, removed or changed), `output`, `retry`, `rate_limit`, `timeout`, what
//!   only people and alerts read (`about`, `custom`, `monitor`, `alert`), and `source` (0.7.5.2.2:
//!   a listener moved, its TLS or `allow:` swapped, an HTTP route changed, a file pickup or
//!   schedule restarted: `sources`).
//! - **Restarted alone** (`Apply::Restart`, 0.7.5.2.4): what a start sets up for it, how messages
//!   are stored, laid out in lanes or run (`format`, `ordering`, `isolation`, `workers`,
//!   `dedicated_pools`, `durability`, `dedupe`, `retention`, `quarantined`) and a change between
//!   queue and respond mode: its workers all stop, then start on the new settings
//!   (`Engine::restart_interface`); its senders never notice.
//!
//! Only the project's own files read at a start (`ductus.yaml`, `resources.yaml` or `alerts:` as
//! filled in, `docs-schema.yaml`, `secrets/<env>.json`) go by the handover, the answer saying
//! which. The engine's environment file goes in place (its variables as above, `console:`
//! swapped); another environment's files change nothing here (`deploy::project_plan`).
//!
//! An interface added or removed goes in place whatever its source (0.7.5.2.1): its listener,
//! file pickup or schedule starts or stops alone (`sources`). What only the settings resolved
//! can tell (`${...}` filled in) is decided as it's applied, the deploy then taking the handover
//! with the reason (`sources::in_place`: a port or HTTP route moving from one interface to
//! another, a listener changing kind or host on its own port).

use crate::history::parts::Touches;
use anyhow::{Context, Result};
use ductus_core::yaml::Value;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// interface.yaml's keys a change to which is applied between two messages; any other key
/// restarts the interface alone.
const IN_PLACE: &[&str] = &[
    "script",
    "destinations",
    "output",
    "retry",
    "rate_limit",
    "timeout",
    "about",
    "custom",
    "monitor",
    "alert",
    "source",
];

/// Whether a `source` as written answers requests itself (`http: { mode: respond }`).
fn responds(source: Option<&Value>) -> bool {
    let mode = source.and_then(|s| s.get("http")).and_then(|h| h.get("mode")).and_then(Value::as_str);
    mode == Some("respond")
}

/// How one interface's change is applied (always in place; the project's own files may still
/// need the handover: `Plan::project`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// Its workers take its new code and settings between two messages, each where it runs.
    InPlace,
    /// It restarts alone (its workers all stopped, then started on the new settings, its
    /// listener, claim and counts kept); why, in words.
    Restart(String),
}

/// What changes for one interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub interface: String,
    /// What changed, in words: `code`, `destinations.lab`, `destination lab added`, `source`,
    /// `source (MLLP_PORT)` (a setting a variable fills in), `code reads EMPI_URL`, ...
    pub what: Vec<String>,
    pub apply: Apply,
    /// What new values of the environment's variables change in it (its settings filled in, or
    /// its code naming one): what the deployment records. None for most.
    pub reached: Option<Reached>,
}

/// What a deploy's new variable values change in one interface (0.7.5.2.3): recorded with the
/// deployment (`deployment_variables`), carried to the new engine when it goes by the handover
/// (`handover::Asked`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Reached {
    pub interface: String,
    /// Each variable, by name, with its value before and after.
    pub variables: Vec<VariableChange>,
    /// What they changed, in words: `source (MLLP_PORT)`, `code reads EMPI_URL`.
    pub what: Vec<String>,
    /// The parts of the interface they reach (its history by part shows the change there): its
    /// settings compared filled in, its code by each part's files.
    pub touches: Touches,
}

/// A variable of the engine's environment file whose value a deploy changes (0.7.5.2.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize, utoipa::ToSchema)]
#[schema(as = HistoryVariable)]
pub struct VariableChange {
    pub name: String,
    /// As text (how code reads it); None: the variable is new.
    pub before: Option<String>,
    /// None: the variable is gone.
    pub after: Option<String>,
}

impl std::fmt::Display for VariableChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = |v: &Option<String>| v.as_deref().map_or("(not set)".to_string(), |v| format!("{v:?}"));
        write!(f, "{} {} → {}", self.name, side(&self.before), side(&self.after))
    }
}

/// Every interface's change and whether the whole deploy can go in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Interfaces whose version or filled-in settings change, added or removed, by name.
    pub changes: Vec<Change>,
    /// Why the project's own files need the handover; None when they don't change, or change
    /// only in what goes in place.
    pub project: Option<String>,
    /// What the project's own files change in place, in words (`environments/prod.yaml:
    /// MLLP_PORT 6661 → 6666`, `console: info → warn`, `environments/test.yaml: another
    /// environment's file, nothing here`).
    pub project_in_place: Vec<String>,
    /// The engine's environment file changes (variables or `console:`): the deploy swaps it.
    pub environment_changes: bool,
}

impl Plan {
    /// Every change applies in place (nothing changed counts too): unless the project's own files
    /// need the handover.
    pub fn in_place(&self) -> bool {
        self.project.is_none()
    }

    /// Why the deploy needs the handover: one line per reason; empty when it doesn't.
    pub fn handover_because(&self) -> Vec<String> {
        self.project.iter().cloned().collect()
    }

    /// What the new variable values change, by interface (to record with the deployment).
    pub fn reached(&self) -> Vec<Reached> {
        self.changes.iter().filter_map(|c| c.reached.clone()).collect()
    }
}

/// One side of a deploy, per interface: its code version (hex) and its interface.yaml as written
/// (read where the versions differ, and, when variables change, for every interface).
pub type Side<'a> = BTreeMap<&'a str, (&'a str, &'a str)>;

/// What the project's own files change (`deploy::project_plan` reads them).
#[derive(Debug, Clone, Default)]
pub struct Project {
    /// Why they need the handover; None: they don't.
    pub handover: Option<String>,
    /// What goes in place, in words (`Plan::project_in_place`).
    pub in_place: Vec<String>,
    /// The engine's environment file changes (`Plan::environment_changes`).
    pub environment_changes: bool,
    pub variables: Variables,
}

/// The engine's environment's variables on each side of a deploy.
#[derive(Debug, Clone, Default)]
pub struct Variables {
    pub before: BTreeMap<String, Value>,
    pub after: BTreeMap<String, Value>,
    /// By interface: the changed variables its code names as text (`env("EMPI_URL")`).
    pub in_code: BTreeMap<String, BTreeSet<String>>,
}

impl Variables {
    /// The names whose value differs, is new or is gone.
    pub fn changed(&self) -> BTreeSet<&str> {
        let names = self.before.keys().chain(self.after.keys());
        names.filter(|n| self.before.get(*n) != self.after.get(*n)).map(String::as_str).collect()
    }

    /// `name`'s change, values as text.
    pub fn change(&self, name: &str) -> VariableChange {
        let text = |v: Option<&Value>| v.map(ductus_core::env::value_text);
        VariableChange { name: name.into(), before: text(self.before.get(name)), after: text(self.after.get(name)) }
    }
}

/// Whether interface.yaml's text names one of `changed` (`${NAME}`): an interface that doesn't
/// is skipped without parsing (a thousand interfaces, a handful reached).
fn names_any(yaml: &str, changed: &BTreeSet<&str>) -> bool {
    changed.iter().any(|n| yaml.contains(&format!("${{{n}}}")))
}

/// The plan for going from `running` to `next`, the project's own files judged in `project`.
pub fn plan(running: &Side, next: &Side, project: &Project) -> Result<Plan> {
    let vars = &project.variables;
    let changed = vars.changed();
    let mut changes = Vec::new();
    let names: BTreeSet<&str> = running.keys().chain(next.keys()).copied().collect();
    for name in names {
        let in_code = vars.in_code.get(name).is_some_and(|n| !n.is_empty());
        let change = match (running.get(name), next.get(name)) {
            (Some((before, _)), Some((after, yaml)))
                if before == after && (changed.is_empty() || !(in_code || names_any(yaml, &changed))) =>
            {
                continue;
            }
            (Some((v1, before)), Some((v2, after))) => match interface(name, before, after, v1 != v2, vars)? {
                Some(change) => change,
                None => continue,
            },
            (None, Some(_)) => Change::added_or_removed(name, ADDED),
            (Some(_), None) => Change::added_or_removed(name, REMOVED),
            (None, None) => unreachable!("from either side"),
        };
        changes.push(change);
    }
    Ok(Plan {
        changes,
        project: project.handover.clone(),
        project_in_place: project.in_place.clone(),
        environment_changes: project.environment_changes,
    })
}

/// What `Change::what` says of an interface added or removed.
const ADDED: &str = "added";
const REMOVED: &str = "removed";

impl Change {
    fn added_or_removed(name: &str, what: &str) -> Self {
        Change { interface: name.into(), what: vec![what.into()], apply: Apply::InPlace, reached: None }
    }

    /// The interface is new: the deploy adds it.
    pub fn is_added(&self) -> bool {
        self.what == [ADDED]
    }

    /// The interface is gone: the deploy removes it.
    pub fn is_removed(&self) -> bool {
        self.what == [REMOVED]
    }
}

/// One interface on both sides: which keys of its interface.yaml differ once each side is filled
/// in with its own environment's variables (`${secret:..}` left as written), and how it applies.
/// None when nothing differs and its version is the same (a variable it names kept its value
/// where it's used, or changed only to the same filled-in setting).
fn interface(name: &str, before: &str, after: &str, version_changed: bool, vars: &Variables) -> Result<Option<Change>> {
    let read = |text: &str,
                variables: &BTreeMap<String, Value>|
     -> Result<(BTreeMap<String, Value>, BTreeMap<String, Value>)> {
        let written = ductus_core::yaml::parse(text).with_context(|| format!("reading {name}'s interface.yaml"))?;
        let mut filled = written.clone();
        ductus_core::env::fill_variables(&mut filled, variables);
        let map = |value: &Value| match value {
            Value::Null => Ok(BTreeMap::new()),
            value => entries(Some(value)).with_context(|| format!("{name}'s interface.yaml isn't a mapping")),
        };
        Ok((map(&written)?, map(&filled)?))
    };
    let (written_before, before) = read(before, &vars.before)?;
    let (written_after, after) = read(after, &vars.after)?;
    let changed = vars.changed();
    let keys: BTreeSet<String> = before.keys().chain(after.keys()).cloned().collect();
    let mut what = Vec::new();
    let mut restart = Vec::new();
    let mut reached: BTreeSet<&str> = BTreeSet::new();
    let mut by_variables = Vec::new(); // `what`'s lines a variable moved
    // The settings as they were, but with what no variable moved as it will be: compared with
    // `after`, the parts only the variables reach (history by part).
    let mut moved_only = after.clone();
    for key in keys {
        let (b, a) = (before.get(&key), after.get(&key));
        if b == a {
            continue;
        }
        // The changed variables this key names, on either side as written.
        let mut named = BTreeSet::new();
        for side in [written_before.get(&key), written_after.get(&key)].into_iter().flatten() {
            ductus_core::env::variable_names(side, &mut named);
        }
        let by: Vec<&str> = changed.iter().copied().filter(|n| named.contains(*n)).collect();
        reached.extend(&by);
        if !by.is_empty() {
            match b {
                Some(b) => moved_only.insert(key.clone(), b.clone()),
                None => moved_only.remove(&key),
            };
        }
        let said = if written_before.get(&key) == written_after.get(&key) && !by.is_empty() {
            // Only a variable moved it: say which.
            format!(" ({})", by.join(", "))
        } else {
            String::new()
        };
        let lines: Vec<String> = match key.as_str() {
            "destinations" => destinations(b, a).into_iter().map(|d| format!("{d}{said}")).collect(),
            _ => vec![format!("{key}{said}")],
        };
        if !by.is_empty() {
            by_variables.extend(lines.iter().cloned());
        }
        what.extend(lines);
        if !IN_PLACE.contains(&key.as_str()) {
            restart.push(format!("`{key}`"));
        }
    }
    let in_code = vars.in_code.get(name).map(|n| n.iter().map(String::as_str).collect::<Vec<_>>()).unwrap_or_default();
    if !in_code.is_empty() {
        let line = format!("code reads {}", in_code.join(", "));
        by_variables.push(line.clone());
        what.push(line);
        reached.extend(in_code.iter().copied());
    }
    if what.is_empty() {
        if !version_changed {
            return Ok(None);
        }
        what.push("code".into());
    }
    let reached = (!reached.is_empty()).then(|| Reached {
        interface: name.into(),
        variables: reached.iter().map(|n| vars.change(n)).collect(),
        what: by_variables,
        touches: match moved_only == after {
            true => Touches::default(), // only its code reads them: `deploy::plan_between` adds those parts
            false => {
                // As JSON, which YAML reads (`parts::yaml` takes text).
                let text = |m: &BTreeMap<String, Value>| serde_json::to_string(m).unwrap_or_default();
                crate::history::parts::yaml(Some(&text(&moved_only)), Some(&text(&after)))
            }
        },
    });
    if responds(before.get("source")) != responds(after.get("source")) {
        restart.push("its mode (queue ↔ respond)".into());
    }
    let apply = match restart.is_empty() {
        true => Apply::InPlace,
        false => Apply::Restart(format!(
            "{} changed (what a start sets up for it: how it stores, lays out in lanes or runs its messages)",
            restart.join(", ")
        )),
    };
    Ok(Some(Change { interface: name.into(), what, apply, reached }))
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
        let plan = plan(&running, &next, &Project::default()).unwrap();
        assert!(plan.in_place(), "{plan:#?}");
        let what: Vec<(&str, Vec<&str>)> =
            plan.changes.iter().map(|c| (c.interface.as_str(), c.what.iter().map(String::as_str).collect())).collect();
        assert_eq!(
            what,
            [("adt", vec!["code"]), ("b", vec!["destination archive added", "retry"]), ("c", vec!["destinations.lab"])]
        );
        let removed = side(&[("adt", "v3", "name: adt\nsource: { mllp: { port: 6661 } }\ndestinations: {}\n")]);
        let plan = super::plan(&side(&[("adt", "v1", LAB)]), &removed, &Project::default()).unwrap();
        assert_eq!(plan.changes[0].what, ["destination lab removed"]);
        assert!(plan.in_place());
    }

    /// How messages are stored, lanes, workers, queue ↔ respond: in place, the interface
    /// restarting alone, each saying why; only the project's files need the handover.
    #[test]
    fn what_a_start_sets_up_restarts_the_interface_alone() {
        let format = format!("{LAB}format: json\n");
        let lanes = format!("{LAB}ordering: {{ key: PID-3.1 }}\nisolation: dedicated\nworkers: 4\n");
        let queue = "name: api\nsource: { http: { port: 8081, path: /a } }\ndestinations: {}\n";
        let respond = queue.replace("/a }", "/a, mode: respond }");
        let retry = format!("{LAB}retry: {{ max_attempts: 3 }}\n");
        let running = side(&[("adt", "v1", LAB), ("b", "b1", LAB), ("api", "a1", queue), ("c", "c1", LAB)]);
        let next = side(&[("adt", "v2", &format), ("b", "b2", &lanes), ("api", "a2", &respond), ("c", "c2", &retry)]);
        let plan = plan(&running, &next, &Project::default()).unwrap();
        assert!(plan.in_place(), "{plan:#?}");
        assert!(plan.handover_because().is_empty());
        let why = |n: &str| match &plan.changes.iter().find(|c| c.interface == n).unwrap().apply {
            Apply::Restart(why) => why.clone(),
            Apply::InPlace => String::new(),
        };
        assert!(why("adt").starts_with("`format` changed"), "{plan:#?}");
        assert!(why("b").starts_with("`isolation`, `ordering`, `workers` changed"), "{plan:#?}");
        assert!(why("api").starts_with("its mode (queue ↔ respond) changed"), "{plan:#?}");
        assert_eq!(why("c"), "", "retry: between two messages");

        let project = Project { handover: Some("resources.yaml changed: ...".into()), ..Project::default() };
        let plan = super::plan(&running, &next, &project).unwrap();
        assert!(!plan.in_place());
        assert_eq!(plan.handover_because(), ["resources.yaml changed: ..."]);
        assert!(super::plan(&running, &running, &Project::default()).unwrap().changes.is_empty(), "nothing changes");
    }

    /// A source changed (a port moved, TLS or `allow:`, an HTTP path, a folder): in place, said
    /// as `source`.
    #[test]
    fn a_changed_source_goes_in_place() {
        let port = LAB.replace("6661", "6662");
        let allow = LAB.replace("port: 6661 }", "port: 6661, allow: [10.1.20.0/24] }");
        let queue = "name: api\nsource: { http: { port: 8081, path: /a } }\ndestinations: {}\n";
        let path = queue.replace("/a", "/b");
        let running = side(&[("adt", "v1", LAB), ("lab", "l1", LAB), ("api", "a1", queue)]);
        let next = side(&[("adt", "v2", &port), ("lab", "l2", &allow), ("api", "a2", &path)]);
        let plan = plan(&running, &next, &Project::default()).unwrap();
        assert!(plan.in_place(), "{plan:#?}");
        assert!(plan.changes.iter().all(|c| c.what == ["source"]), "{plan:#?}");
    }

    /// An interface added or removed, whatever its source (a listener, file pickup, a schedule,
    /// routes): in place, said as added or removed.
    #[test]
    fn interfaces_are_added_and_removed_in_place() {
        let internal = "name: inbox\nsource: internal\ndestinations:\n  out: { file: { dir: out } }\n";
        let files = "name: drop\nsource: { file: { dir: in } }\ndestinations:\n  out: { file: { dir: out } }\n";
        let routes = format!("{LAB}  inbox: {{ interface: inbox }}\n");
        let running = side(&[("adt", "v1", LAB), ("old", "o", internal), ("mllp", "m", LAB)]);
        let next = side(&[("adt", "v2", &routes), ("inbox", "i", internal), ("drop", "d", files)]);
        let plan = plan(&running, &next, &Project::default()).unwrap();
        assert!(plan.in_place(), "{plan:#?}");
        let by_name = |n: &str| plan.changes.iter().find(|c| c.interface == n).unwrap();
        assert!(by_name("inbox").is_added() && !by_name("inbox").is_removed());
        assert!(by_name("drop").is_added());
        assert!(by_name("old").is_removed() && !by_name("old").is_added());
        assert!(by_name("mllp").is_removed());
        assert!(!by_name("adt").is_added() && !by_name("adt").is_removed());
    }

    fn variables(before: &str, after: &str) -> Variables {
        let read = |text: &str| ductus_core::env::read_file(text).unwrap().0;
        Variables { before: read(before), after: read(after), in_code: BTreeMap::new() }
    }

    /// A variable's new value reaches the interfaces whose filled-in settings it moves (in place
    /// for a source, restarting it alone for what a start sets), and those whose code names it; an
    /// interface naming only variables that kept their values, or none, isn't touched.
    #[test]
    fn a_variable_reaches_the_interfaces_it_fills_in_or_their_code_reads() {
        let by_port = "name: adt\nsource: { mllp: { port: ${MLLP_PORT} } }\ndestinations:\n  lab: { mllp: { host: ${LAB_HOST}, port: 7001 } }\n";
        let by_days = "name: keep\nsource: internal\nretention: { days: ${DAYS} }\ndestinations: {}\n";
        let in_text =
            "name: url\nsource: internal\ndestinations:\n  api: { http: { url: 'https://${LAB_HOST}/in' } }\n";
        let unrelated = "name: other\nsource: { mllp: { port: ${OTHER_PORT} } }\ndestinations: {}\n";
        let code = "name: code\nsource: internal\ndestinations: {}\n";
        let all = [("adt", "a", by_port), ("keep", "k", by_days), ("url", "u", in_text), ("other", "o", unrelated)];
        let mut all = all.to_vec();
        all.push(("code", "c", code));
        let side = side(&all);
        let mut vars = variables(
            "variables: { MLLP_PORT: 6661, LAB_HOST: lab, DAYS: 30, OTHER_PORT: 7000, EMPI: a }",
            "variables: { MLLP_PORT: 6666, LAB_HOST: lab, DAYS: 60, OTHER_PORT: 7000, EMPI: b }",
        );
        vars.in_code.insert("code".into(), ["EMPI".to_string()].into());
        let project = Project { variables: vars, environment_changes: true, ..Project::default() };
        let plan = plan(&side, &side, &project).unwrap();
        let by_name = |n: &str| plan.changes.iter().find(|c| c.interface == n);
        let adt = by_name("adt").unwrap();
        assert_eq!(adt.what, ["source (MLLP_PORT)"]);
        assert_eq!(adt.apply, Apply::InPlace);
        let reached = adt.reached.as_ref().unwrap();
        let values: Vec<String> = reached.variables.iter().map(ToString::to_string).collect();
        assert_eq!(values, ["MLLP_PORT \"6661\" → \"6666\""]);
        assert_eq!(reached.what, ["source (MLLP_PORT)"]);
        assert_eq!(reached.touches, Touches { source: true, ..Touches::default() }, "its source only");
        let keep = by_name("keep").unwrap();
        assert_eq!(keep.what, ["retention (DAYS)"]);
        assert!(matches!(&keep.apply, Apply::Restart(why) if why.starts_with("`retention` changed")), "{keep:#?}");
        let code = by_name("code").unwrap();
        assert_eq!((code.what.as_slice(), &code.apply), (&["code reads EMPI".to_string()][..], &Apply::InPlace));
        assert!(by_name("url").is_none() && by_name("other").is_none(), "{plan:#?}");
        assert_eq!(plan.changes.len(), 3, "{plan:#?}");

        // A value inside text: the destination it's in, said by name.
        let project = Project {
            variables: variables("variables: { LAB_HOST: lab }", "variables: { LAB_HOST: lab2 }"),
            ..Project::default()
        };
        let plan = super::plan(&side, &side, &project).unwrap();
        let what: Vec<(&str, &[String])> = plan.changes.iter().map(|c| (c.interface.as_str(), &c.what[..])).collect();
        assert_eq!(
            what,
            [
                ("adt", &["destinations.lab (LAB_HOST)".to_string()][..]),
                ("url", &["destinations.api (LAB_HOST)".to_string()][..])
            ]
        );
        assert!(plan.in_place());
        let touches = &plan.changes[0].reached.as_ref().unwrap().touches;
        assert_eq!((touches.destinations.as_slice(), touches.source), (&["lab".to_string()][..], false));
        assert_eq!(plan.reached().len(), 2);

        // A new variable and a gone one; an interface whose interface.yaml changed too says both.
        let edited = by_port.replace("7001", "7002");
        let next = self::side(&[("adt", "a2", &edited)]);
        let project = Project {
            variables: variables("variables: { MLLP_PORT: 6661 }", "variables: { MLLP_PORT: 6666, NEW: x }"),
            ..Project::default()
        };
        let plan = super::plan(&self::side(&[("adt", "a", by_port)]), &next, &project).unwrap();
        assert_eq!(plan.changes[0].what, ["destinations.lab", "source (MLLP_PORT)"]);
        assert_eq!(
            plan.changes[0].reached.as_ref().unwrap().what,
            ["source (MLLP_PORT)"],
            "only what a variable moved"
        );
        let gone = variables("variables: { NEW: x }", "variables: {}").change("NEW");
        assert_eq!(gone.to_string(), "NEW \"x\" → (not set)");
    }

    /// Formatting alone (quotes, flow or block style, key order) isn't a change of settings.
    #[test]
    fn the_yaml_is_compared_as_values_not_text() {
        let block = "name: adt\nsource:\n  mllp:\n    port: 6661\ndestinations:\n  lab:\n    script: lab.ts\n    mllp:\n      port: 7001\n      host: \"lab\"\n";
        let plan = plan(&side(&[("adt", "v1", LAB)]), &side(&[("adt", "v2", block)]), &Project::default()).unwrap();
        assert_eq!(plan.changes[0].what, ["code"], "{plan:#?}");
    }
}
