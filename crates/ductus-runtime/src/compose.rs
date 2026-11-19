//! Composed releases (0.7.5.3.2): `ductus deploy R --interface A` deploys only A's work from
//! release R, over what runs. The engine makes a release of its own for it: the release running
//! there, with A's folder and the code A loads from outside it (`lib/...`) taken from R. Stored
//! like any release, so the gate, the "replaces" check, history, rollback and promotion all work
//! on it as they do on a release made by `ductus release`.
//!
//! **What is taken from R**, for each interface named: its folder (`interfaces/<folder>/`: its
//! interface.yaml, code, tests, fixtures) replaces the running one's, and every file its code
//! version lists outside the folder (shared code it imports) comes from R. An interface R doesn't
//! have is removed; one only R has is added. **Everything else is as it runs**: every other
//! interface, and the project's own files (ductus.yaml, resources.yaml, `environments/`,
//! `secrets/`, docs-schema.yaml), even when R's differ (said: deploy R whole to bring them).
//!
//! **Refused** when it would change another interface: a file the named interfaces need from R
//! (or one their running folder held) that differs from what runs and is part of an interface not
//! named (its code version lists it). The refusal names the file and those interfaces: name them
//! too, or deploy R whole.
//!
//! Its id is a hash of its files, like any release's (`release::id`): composing A from R over a
//! release that differs from R only in A gives R itself, and composing what runs already gives the
//! running release (nothing to do).

use crate::code;
use crate::release::{self, Composed, File, Manifest};
use std::collections::{BTreeMap, BTreeSet};

/// A release as composing reads it: its manifest and files.
#[derive(Clone, Copy)]
pub struct Side<'a> {
    pub manifest: &'a Manifest,
    pub files: &'a [File],
}

/// A release composed: its manifest (id worked out), its files, and what it did.
#[derive(Debug)]
pub struct Composition {
    pub manifest: Manifest,
    pub files: Vec<File>,
    /// What each named interface does, by name: `changed`, `added`, `removed` or `unchanged`
    /// (R's version is the one running).
    pub taken: BTreeMap<String, &'static str>,
    /// The project's own files R has that differ from what runs (or that only one has): kept as
    /// they run.
    pub project_differs: Vec<String>,
}

/// Compose `over` (what runs) with `names` from `from`. `made`: who composes it, when (the
/// engine's deploy). Err: why it can't be composed, in words (the deploy's refusal).
pub fn compose(over: Side, from: Side, names: &[String], made: release::Made) -> Result<Composition, String> {
    let names: BTreeSet<&str> = names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return Err("name the interfaces to take from the release (`--interface`)".into());
    }
    if over.manifest.project != from.manifest.project {
        return Err(format!(
            "release {} is of project `{}`, and what runs is `{}`: an interface can only come from a release of the same project",
            release::short(&from.manifest.id),
            from.manifest.project,
            over.manifest.project
        ));
    }
    let running: BTreeMap<&str, &File> = over.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let source: BTreeMap<&str, &File> = from.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let unknown: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !over.manifest.interfaces.contains_key(*n) && !from.manifest.interfaces.contains_key(*n))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "neither release {} nor what runs here ({}) has an interface named {}: `ductus release inspect` lists a \
             release's interfaces",
            release::short(&from.manifest.id),
            release::short(&over.manifest.id),
            unknown.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
        ));
    }

    let mut next: BTreeMap<String, File> = over.files.iter().map(|f| (f.path.clone(), f.clone())).collect();
    let mut interfaces = over.manifest.interfaces.clone();
    let mut taken = BTreeMap::new();
    let mut from_source: BTreeSet<String> = BTreeSet::new();
    for name in &names {
        let was = over.manifest.interfaces.get(*name);
        let now = from.manifest.interfaces.get(*name);
        if let Some(dir) = was.and_then(folder) {
            next.retain(|path, _| !inside(path, dir));
        }
        match now {
            Some(i) => {
                let dir = folder(i).ok_or_else(|| {
                    format!("release {}: `{name}` lists no interface.yaml", release::short(&from.manifest.id))
                })?;
                let theirs = from.files.iter().filter(|f| inside(&f.path, dir)).map(|f| f.path.as_str());
                let needed: Vec<&str> = theirs.chain(i.files.iter().map(String::as_str)).collect();
                for path in needed {
                    let file = source.get(path).ok_or_else(|| {
                        format!(
                            "release {}: `{name}` runs `{path}`, which the release doesn't carry",
                            release::short(&from.manifest.id)
                        )
                    })?;
                    next.insert(path.to_string(), (*file).clone());
                    from_source.insert(path.to_string());
                }
                interfaces.insert(name.to_string(), i.clone());
            }
            None => drop(interfaces.remove(*name)),
        }
        let what = match (was, now) {
            (Some(w), Some(n)) if w.version == n.version => "unchanged",
            (Some(_), Some(_)) => "changed",
            (None, Some(_)) => "added",
            _ => "removed",
        };
        taken.insert(name.to_string(), what);
    }

    // Nothing an interface not named runs may change.
    let mut changes_others: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (other, i) in over.manifest.interfaces.iter().filter(|(n, _)| !names.contains(n.as_str())) {
        for path in &i.files {
            let before = running.get(path.as_str()).map(|f| &f.bytes);
            if next.get(path).map(|f| &f.bytes) != before {
                changes_others.entry(path.as_str()).or_default().push(other.as_str());
            }
        }
    }
    if !changes_others.is_empty() {
        let lines: Vec<String> = changes_others
            .iter()
            .map(|(path, others)| {
                let them = others.iter().map(|o| format!("`{o}`")).collect::<Vec<_>>().join(", ");
                format!(
                    "`{path}` is different in release {}, and {them} run{} it too",
                    release::short(&from.manifest.id),
                    if others.len() == 1 { "s" } else { "" }
                )
            })
            .collect();
        let also: BTreeSet<&str> = changes_others.values().flatten().copied().collect();
        let flags: String = names.iter().chain(also.iter()).map(|n| format!(" --interface {n}")).collect();
        return Err(format!(
            "taking {} alone would change other interfaces: {}. Take them too (`ductus deploy {}{flags}`), or deploy \
             the release whole",
            names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", "),
            lines.join("; "),
            release::short(&from.manifest.id),
        ));
    }

    let project_differs: Vec<String> = running
        .keys()
        .chain(source.keys())
        .filter(|p| code::is_project_file(p))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|p| running.get(*p).map(|f| &f.bytes) != source.get(*p).map(|f| &f.bytes))
        .map(|p| p.to_string())
        .collect();

    let files: Vec<File> = next.into_values().collect();
    let (project_version, project_files) = release::project_version(&files);
    let manifest = Manifest {
        format: release::FORMAT,
        id: code::hex(&release::id(&files)),
        project: over.manifest.project.clone(),
        made,
        git: from.manifest.git.clone(),
        uncommitted: from.manifest.uncommitted.iter().filter(|p| from_source.contains(*p)).cloned().collect(),
        over: None,
        project_version: release::ProjectVersion { version: code::hex(&project_version), files: project_files },
        interfaces,
        tests: release::Tests::default(),
        files: files.len(),
        bytes: files.iter().map(|f| f.bytes.len() as u64).sum(),
        binary: files.iter().filter(|f| f.binary).map(|f| f.path.clone()).collect(),
        deployed: Vec::new(),
        composed: Some(Composed {
            over: over.manifest.id.clone(),
            from: from.manifest.id.clone(),
            interfaces: names.iter().map(|n| n.to_string()).collect(),
        }),
    };
    Ok(Composition { manifest, files, taken, project_differs })
}

/// An interface's folder in a release (`interfaces/<folder>`): where its interface.yaml is.
fn folder(i: &release::Interface) -> Option<&str> {
    i.files
        .iter()
        .find(|f| f.starts_with("interfaces/") && f.ends_with("/interface.yaml"))
        .and_then(|f| f.strip_suffix("/interface.yaml"))
}

fn inside(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn made() -> release::Made {
        release::Made {
            at: "2026-11-18T00:00:00Z".into(),
            by: "ann".into(),
            host: "h".into(),
            reason: None,
            binary: "0.7.0".into(),
            platform: "linux-x86_64".into(),
        }
    }

    /// A release of `files` (path, text), whose interfaces are `interfaces` (name, the files its
    /// code version lists besides its folder's interface.yaml); versions hashed from those files.
    fn release(files: &[(&str, &str)], interfaces: &[(&str, &[&str])], commit: &str) -> (Manifest, Vec<File>) {
        let files: Vec<File> = files.iter().map(|(p, t)| File::new(*p, t.as_bytes().to_vec())).collect();
        let bytes: BTreeMap<&str, &File> = files.iter().map(|f| (f.path.as_str(), f)).collect();
        let interfaces = interfaces
            .iter()
            .map(|(name, extra)| {
                let mut listed = vec![format!("interfaces/{name}/interface.yaml")];
                listed.extend(extra.iter().map(|e| e.to_string()));
                let text: String = listed
                    .iter()
                    .map(|p| code::hex(&bytes[p.as_str()].hash()) + p.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let version = code::hex(&release::id(&[File::new("v", text.into_bytes())]));
                (name.to_string(), release::Interface { version, files: listed, parts: Default::default() })
            })
            .collect();
        let (version, own) = release::project_version(&files);
        let manifest = Manifest {
            format: 1,
            id: code::hex(&release::id(&files)),
            project: "p".into(),
            made: made(),
            git: Some(release::GitSource {
                commit: commit.repeat(40),
                folder: String::new(),
                branch: Some("main".into()),
                commits: 1,
                ancestry: Vec::new(),
            }),
            uncommitted: Vec::new(),
            over: None,
            project_version: release::ProjectVersion { version: code::hex(&version), files: own },
            interfaces,
            tests: Default::default(),
            files: files.len(),
            bytes: 0,
            binary: Vec::new(),
            deployed: Vec::new(),
            composed: None,
        };
        (manifest, files)
    }

    fn side<'a>(r: &'a (Manifest, Vec<File>)) -> Side<'a> {
        Side { manifest: &r.0, files: &r.1 }
    }

    fn text<'a>(c: &'a Composition, path: &str) -> Option<&'a str> {
        c.files.iter().find(|f| f.path == path).map(|f| std::str::from_utf8(&f.bytes).unwrap())
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    /// Running: A and B sharing `lib/codes.ts`, C alone. R: A changed (and a new fixture), B
    /// changed, `lib/new.ts` for A, prod's environment file changed. A alone from R: A's folder
    /// and its new shared file from R, B and the project's files as they run, said.
    #[test]
    fn an_interface_is_taken_from_a_release_and_everything_else_stays_as_it_runs() {
        let running = release(
            &[
                ("ductus.yaml", "{}\n"),
                ("environments/prod.yaml", "variables: { PORT: 1 }\n"),
                ("interfaces/a/interface.yaml", "name: a\n"),
                ("interfaces/a/a.ts", "v1\n"),
                ("interfaces/a/tests/old.test.ts", "old\n"),
                ("interfaces/b/interface.yaml", "name: b\n"),
                ("interfaces/c/interface.yaml", "name: c\n"),
                ("lib/codes.ts", "codes\n"),
            ],
            &[("a", &["interfaces/a/a.ts", "lib/codes.ts"]), ("b", &["lib/codes.ts"]), ("c", &[])],
            "1",
        );
        let next = release(
            &[
                ("ductus.yaml", "{}\n"),
                ("environments/prod.yaml", "variables: { PORT: 2 }\n"),
                ("interfaces/a/interface.yaml", "name: a\n"),
                ("interfaces/a/a.ts", "v2\n"),
                ("interfaces/a/tests/new.test.ts", "new\n"),
                ("interfaces/b/interface.yaml", "name: b\nretry: 3\n"),
                ("interfaces/c/interface.yaml", "name: c\n"),
                ("lib/codes.ts", "codes\n"),
                ("lib/new.ts", "new\n"),
            ],
            &[("a", &["interfaces/a/a.ts", "lib/codes.ts", "lib/new.ts"]), ("b", &["lib/codes.ts"]), ("c", &[])],
            "2",
        );
        let c = compose(side(&running), side(&next), &names(&["a"]), made()).unwrap();
        assert_eq!(c.taken, BTreeMap::from([("a".to_string(), "changed")]));
        assert_eq!((text(&c, "interfaces/a/a.ts"), text(&c, "lib/new.ts")), (Some("v2\n"), Some("new\n")));
        assert_eq!(text(&c, "interfaces/a/tests/new.test.ts"), Some("new\n"));
        assert_eq!(text(&c, "interfaces/a/tests/old.test.ts"), None, "its folder is R's, whole");
        assert_eq!(text(&c, "interfaces/b/interface.yaml"), Some("name: b\n"), "b as it runs");
        assert_eq!(text(&c, "environments/prod.yaml"), Some("variables: { PORT: 1 }\n"));
        assert_eq!(c.project_differs, ["environments/prod.yaml"]);
        let m = &c.manifest;
        assert_eq!(
            (m.interfaces["a"].clone(), m.interfaces["b"].clone()),
            (next.0.interfaces["a"].clone(), running.0.interfaces["b"].clone())
        );
        assert_eq!(m.project_version, running.0.project_version, "the project's files as they run");
        assert_eq!(m.git.as_ref().unwrap().commit, "2".repeat(40), "the commit a came from");
        assert_eq!(m.id, code::hex(&release::id(&c.files)));
        let composed = m.composed.as_ref().unwrap();
        assert_eq!(
            (&composed.over, &composed.from, &composed.interfaces[..]),
            (&running.0.id, &next.0.id, &names(&["a"])[..])
        );
        assert_eq!(m.files, c.files.len());
    }

    /// A shared file A needs from R that B runs too: refused, naming the file and B, with the
    /// command that takes both; taking both is fine.
    #[test]
    fn taking_an_interface_that_would_change_another_is_refused_naming_it() {
        let running = release(
            &[("interfaces/a/interface.yaml", "a\n"), ("interfaces/b/interface.yaml", "b\n"), ("lib/codes.ts", "1\n")],
            &[("a", &["lib/codes.ts"]), ("b", &["lib/codes.ts"])],
            "1",
        );
        let next = release(
            &[("interfaces/a/interface.yaml", "a2\n"), ("interfaces/b/interface.yaml", "b\n"), ("lib/codes.ts", "2\n")],
            &[("a", &["lib/codes.ts"]), ("b", &["lib/codes.ts"])],
            "2",
        );
        let e = compose(side(&running), side(&next), &names(&["a"]), made()).unwrap_err();
        assert!(e.contains("taking `a` alone would change other interfaces"), "{e}");
        assert!(e.contains("`lib/codes.ts` is different in release") && e.contains("and `b` runs it too"), "{e}");
        assert!(e.contains("--interface a --interface b`)"), "{e}");
        let both = compose(side(&running), side(&next), &names(&["b", "a"]), made()).unwrap();
        assert_eq!(both.manifest.id, next.0.id, "everything R changes: R itself");

        // B importing a file from A's folder: A's folder replaced changes B.
        let running = release(
            &[
                ("interfaces/a/interface.yaml", "a\n"),
                ("interfaces/a/x.ts", "1\n"),
                ("interfaces/b/interface.yaml", "b\n"),
            ],
            &[("a", &["interfaces/a/x.ts"]), ("b", &["interfaces/a/x.ts"])],
            "1",
        );
        let next = release(
            &[
                ("interfaces/a/interface.yaml", "a\n"),
                ("interfaces/a/x.ts", "2\n"),
                ("interfaces/b/interface.yaml", "b\n"),
            ],
            &[("a", &["interfaces/a/x.ts"]), ("b", &["interfaces/a/x.ts"])],
            "2",
        );
        let e = compose(side(&running), side(&next), &names(&["a"]), made()).unwrap_err();
        assert!(e.contains("`interfaces/a/x.ts` is different") && e.contains("`b` runs it too"), "{e}");
    }

    /// Added (only R has it), removed (R has none), unchanged (R's is what runs: the running
    /// release again), unknown (neither has it), another project.
    #[test]
    fn an_interface_is_added_removed_or_already_runs_and_an_unknown_one_is_refused() {
        let running = release(
            &[("interfaces/a/interface.yaml", "a\n"), ("interfaces/old/interface.yaml", "old\n")],
            &[("a", &[]), ("old", &[])],
            "1",
        );
        let next = release(
            &[("interfaces/a/interface.yaml", "a\n"), ("interfaces/new/interface.yaml", "new\n")],
            &[("a", &[]), ("new", &[])],
            "2",
        );
        let c = compose(side(&running), side(&next), &names(&["new", "old"]), made()).unwrap();
        assert_eq!(c.taken, BTreeMap::from([("new".into(), "added"), ("old".into(), "removed")]));
        assert_eq!(c.manifest.interfaces.keys().collect::<Vec<_>>(), ["a", "new"]);
        assert_eq!(c.manifest.id, next.0.id);
        let same = compose(side(&running), side(&next), &names(&["a"]), made()).unwrap();
        assert_eq!((same.taken["a"], same.manifest.id.as_str()), ("unchanged", running.0.id.as_str()));
        let e = compose(side(&running), side(&next), &names(&["nope", "a"]), made()).unwrap_err();
        assert!(e.contains("has an interface named `nope`"), "{e}");
        let e = compose(side(&running), side(&next), &[], made()).unwrap_err();
        assert!(e.contains("name the interfaces"), "{e}");
        let mut other = next.clone();
        other.0.project = "q".into();
        let e = compose(side(&running), side(&other), &names(&["a"]), made()).unwrap_err();
        assert!(e.contains("is of project `q`"), "{e}");
    }
}
