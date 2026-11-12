//! `ductus deploy` (0.7.4.4): send a release to an engine in release mode and deploy it there
//! (`POST /releases` when it's a file the engine hasn't stored, then `POST /deployments`), and
//! say what happened: this environment's rules, the "replaces" check, the gate (run or reused),
//! the handover's steps and what changed.
//!
//! 0.7.4.4.3: **promotion and rollback.** A file deployed gets the record of where it went written
//! into it (its id unchanged: the record isn't in the id), so the next environment, which may
//! require it (`DUCTUS_DEPLOY_REQUIRES`), can read it. `--from URL` fetches a stored release from
//! one engine with that record (`GET /releases/{id}/file`) and deploys it to another.
//! `--rollback [ID]` goes back to a release that ran there before.

use anyhow::{Context, Result, bail};
use ductus_runtime::release;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub struct Deploy {
    /// A release file, or a stored release's id (or its start); None: a rollback's default.
    pub release: Option<String>,
    pub to: String,
    pub token: Option<String>,
    /// Promote: the engine to fetch the release from, and its token (default: `token`).
    pub from: Option<String>,
    pub from_token: Option<String>,
    pub rollback: bool,
    pub reason: Option<String>,
    pub force: bool,
    pub all_tests: bool,
    pub json: bool,
    pub timeout: Duration,
}

fn api(url: &str) -> String {
    format!("{}/api/v1", url.trim_end_matches('/'))
}

/// Deploy; Ok(true) when the release runs there.
pub async fn run(d: Deploy) -> Result<bool> {
    let token = d.token.filter(|t| !t.trim().is_empty()).context(
        "set DUCTUS_API_TOKEN to a token with the `deploy` permission (on the server: `ductus token create ci --role operator --permission deploy`)",
    )?;
    let client = reqwest::Client::builder().timeout(d.timeout).build()?;
    let base = api(&d.to);
    let reach = |base: &str| format!("can't reach the engine's API at {base} (is it running? `--to`, DUCTUS_API_URL)");
    let file = d.release.as_deref().map(Path::new).filter(|p| p.is_file());
    let say = |line: String| {
        if !d.json {
            println!("{line}");
        }
    };
    let id = match (&d.release, file, &d.from) {
        (Some(_), Some(_), Some(_)) => {
            bail!("`--from` fetches a release by its id from another engine: give the id, not a file")
        }
        (Some(given), Some(path), None) => {
            let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            store(&client, &base, &token, bytes, &given.to_string(), &say).await?
        }
        (Some(given), None, Some(from)) => {
            let from_token = d.from_token.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(&token);
            let source = api(from);
            let response = client
                .get(format!("{source}/releases/{given}/file"))
                .bearer_auth(from_token)
                .send()
                .await
                .with_context(|| format!("can't reach the engine's API at {source} (`--from`)"))?;
            let status = response.status();
            if !status.is_success() {
                let body: Value = response.json().await.unwrap_or(Value::Null);
                bail!("fetching release {given} from {from}: {status}: {}", error(&body));
            }
            let bytes = response.bytes().await.context("reading the release fetched")?.to_vec();
            let bundle = release::read_bytes(&bytes, "the release fetched")?;
            let went: Vec<&str> = bundle.manifest.deployed.iter().map(|p| p.environment.as_str()).collect();
            say(format!(
                "fetched release {} from {from} ({}, deployed to: {})",
                release::short(&bundle.manifest.id),
                crate::admin::size(bytes.len() as i64),
                if went.is_empty() { "nowhere yet".to_string() } else { went.join(", ") }
            ));
            store(&client, &base, &token, bytes, given, &say).await?
        }
        (Some(given), None, None) if given.contains(['/', '\\']) || given.ends_with(".tar.gz") => {
            bail!("{given}: no such file (give a release file `ductus release` wrote, or a stored release's id)")
        }
        (Some(given), None, None) => given.clone(),
        (None, _, _) => String::new(), // a rollback to the default
    };
    let mut request = json!({ "reason": d.reason, "force": d.force, "all_tests": d.all_tests, "rollback": d.rollback });
    if !id.is_empty() {
        request["release"] = id.clone().into();
    }
    say(if d.rollback {
        "rolling back: the gate runs, then the handover…".to_string()
    } else {
        "deploying: the gate runs (or a recent passing check is reused), then the handover…".to_string()
    });
    let response = client
        .post(format!("{base}/deployments"))
        .bearer_auth(&token)
        .json(&request)
        .send()
        .await
        .with_context(|| reach(&base))?;
    let status = response.status();
    let report: Value = response.json().await.unwrap_or(Value::Null);
    if report.get("outcome").is_none() {
        bail!("{status}: {}", error(&report));
    }
    if d.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", text(&report));
    }
    let ok = report["ok"] == true;
    // The record of where it went, into the file deployed: what the next environment reads.
    if ok {
        match (file, report.get("place").filter(|p| !p.is_null())) {
            (Some(path), Some(place)) => {
                let place: release::Place = serde_json::from_value(place.clone()).context("reading the record")?;
                let environment = place.environment.clone();
                if record(path, place)? {
                    say(format!("recorded in {}: deployed to {environment}", path.display()));
                }
            }
            (Some(path), None) => say(format!(
                "⚠ the engine's record isn't in yet, so {} doesn't say where it went: promote it with `--from` instead",
                path.display()
            )),
            _ => {}
        }
    }
    Ok(ok)
}

/// Store a release file on the engine (`POST /releases`); its id.
async fn store(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    bytes: Vec<u8>,
    name: &str,
    say: &impl Fn(String),
) -> Result<String> {
    let response = client
        .post(format!("{base}/releases"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/gzip")
        .body(bytes)
        .send()
        .await
        .with_context(|| format!("can't reach the engine's API at {base} (is it running? `--to`, DUCTUS_API_URL)"))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("storing {name} on the engine: {status}: {}", error(&body));
    }
    let short = body["short"].as_str().unwrap_or("?");
    let places = match body["new_places"].as_u64().unwrap_or(0) {
        0 => String::new(),
        1 => " (1 record of where it went added)".to_string(),
        n => format!(" ({n} records of where it went added)"),
    };
    if body["new"] == true {
        say(format!("stored release {short} on the engine{places}"));
    } else {
        say(format!("release {short} was stored on the engine before{places}"));
    }
    Ok(body["id"].as_str().context("the engine didn't say the release's id")?.to_string())
}

/// Add `place` to the release file at `path` (rewritten, the same id); false when it was there.
fn record(path: &Path, place: release::Place) -> Result<bool> {
    let mut bundle = release::read(path)?;
    if !bundle.manifest.add_place(place) {
        return Ok(false);
    }
    release::rewrite(path, &bundle.manifest, &bundle.ancestry, &bundle.files)?;
    Ok(true)
}

fn error(body: &Value) -> &str {
    body["error"].as_str().unwrap_or("no details")
}

/// The report in words.
fn text(r: &Value) -> String {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let what = if r["rollback"] == true { "rollback to release" } else { "release" };
    let names: Vec<&str> = r["branches"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let branch = match names.len() {
        0 => String::new(),
        1 => format!(", branch {}", names[0]),
        _ => format!(", branches {}", names.join(", ")),
    };
    let mut out = format!(
        "{what} {} (project {}{branch}) to environment `{}`\n",
        s(&r["short"]),
        s(&r["project"]),
        s(&r["environment"])
    );
    for rule in r["rules"].as_array().into_iter().flatten() {
        let mark = if r["rollback"] == true { "·" } else { "✗" };
        out += &format!("{mark} {}\n", s(rule));
    }
    if let Some(replaces) = r["replaces"].as_object().filter(|m| !m.is_empty()) {
        let mark = match (r["rollback"] == true, r["forced"] == true) {
            (true, _) => "⚠ undoes",
            (false, true) => "⚠ deployed over",
            (false, false) => "✗ would undo",
        };
        for (part, why) in replaces {
            out += &format!("{mark} {part}: {}\n", s(why));
        }
    }
    let gate = &r["gate"];
    if !gate.is_null() {
        let check = &gate["check"];
        let result = if check["ok"] == true { "passed" } else { "found problems" };
        if gate["reused"] == true {
            out += &format!(
                "gate: reused check #{} ({} by {}: {result}; nothing deployed here since)\n",
                check["id"],
                s(&check["at"]),
                s(&check["by"])
            );
        } else {
            out += &format!("gate: {result} in {} s ({})\n", check["seconds"], s(&gate["why_not_reused"]));
            // What it found (the engine keeps the whole report with the check: `GET /releases/{id}`).
            for c in check["report"]["checks"].as_array().into_iter().flatten() {
                let mark = match c["level"].as_str() {
                    Some("problem") => "✗",
                    Some("warning") => "⚠",
                    _ => continue,
                };
                out += &format!("  {mark} {}\n", s(&c["text"]));
                for line in c["more"].as_array().into_iter().flatten().take(10) {
                    out += &format!("      {}\n", s(line));
                }
            }
        }
    }
    for step in r["steps"].as_array().into_iter().flatten() {
        out += &format!("  {}\n", s(step));
    }
    let mark = if r["ok"] == true { "✓" } else { "✗" };
    out += &format!("{mark} {}\n", s(&r["outcome"]));
    let changes = r["changes"].as_array().cloned().unwrap_or_default();
    if r["ok"] == true {
        match changes.len() {
            0 if r["deployment"].is_null() => {
                out += "the new engine's record isn't in yet: `ductus history` shows it\n"
            }
            0 => out += "no interface's code changed (the project's own files may have)\n",
            n => {
                out += &format!("changed {n} interface{}:\n", if n == 1 { "" } else { "s" });
                for c in &changes {
                    let from = c["from"].as_str().map_or("new here".to_string(), |f| format!("{f} →"));
                    out += &format!("  {} {from} {}\n", s(&c["interface"]), s(&c["to"]));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_whose_work_and_the_gate_isnt_run() {
        let r = json!({
            "ok": false, "outcome": "refused: it would undo someone else's work (adt)", "short": "0a1b2c3d4e5f",
            "project": "integrations", "environment": "prod", "replaces": { "adt": "replaces 1234 (deployed … by ann)" },
            "forced": false, "gate": null, "steps": [], "deployment": null, "changes": []
        });
        let t = text(&r);
        assert!(t.contains("✗ would undo adt: replaces 1234 (deployed … by ann)"), "{t}");
        assert!(t.contains("✗ refused: it would undo") && !t.contains("gate:"), "{t}");
    }

    #[test]
    fn a_deploy_says_the_check_it_reused_and_what_changed() {
        let r = json!({
            "ok": true, "outcome": "handed over to ductus 0.7.0 (pid 9): receiving never stopped, sending paused 180 ms",
            "short": "0a1b2c3d4e5f", "project": "integrations", "environment": "prod", "replaces": {}, "forced": false,
            "gate": { "reused": true, "check": { "id": 12, "at": "2026-11-11T14:02:00Z", "by": "ci", "ok": true } },
            "steps": ["started /usr/bin/ductus (pid 9)"], "deployment": 40,
            "changes": [{ "interface": "adt", "from": "aaaaaaaaaaaa", "to": "bbbbbbbbbbbb" }, { "interface": "lab", "from": null, "to": "cccccccccccc" }]
        });
        let t = text(&r);
        assert!(t.contains("gate: reused check #12 (2026-11-11T14:02:00Z by ci: passed"), "{t}");
        assert!(
            t.contains("✓ handed over")
                && t.contains("  adt aaaaaaaaaaaa → bbbbbbbbbbbb")
                && t.contains("  lab new here cccccccccccc"),
            "{t}"
        );
    }

    /// The environment's rules refuse with what to do; a rollback names what it undoes and that
    /// the rules don't hold it.
    #[test]
    fn rules_and_a_rollback_are_said() {
        let refused = json!({
            "ok": false, "outcome": "refused by this environment's rules (`rules` says why)", "short": "0a1b2c3d4e5f",
            "project": "integrations", "environment": "prod", "branches": ["feature/x"], "rollback": false,
            "rules": ["this environment takes only releases made on main (DUCTUS_DEPLOY_BRANCHES), and this one was made on `feature/x`: merge it, then release from there"],
            "replaces": {}, "forced": false, "gate": null, "steps": [], "deployment": null, "changes": []
        });
        let t = text(&refused);
        assert!(
            t.starts_with("release 0a1b2c3d4e5f (project integrations, branch feature/x) to environment `prod`"),
            "{t}"
        );
        assert!(t.contains("✗ this environment takes only releases made on main"), "{t}");
        let back = json!({
            "ok": true, "outcome": "handed over", "short": "0a1b2c3d4e5f", "project": "integrations",
            "environment": "prod", "rollback": true, "rules": ["a rollback isn't held to this environment's rules (…)"],
            "replaces": { "adt": "replaces 1234 (deployed … by bob)" }, "forced": false,
            "gate": null, "steps": [], "deployment": 41, "changes": []
        });
        let t = text(&back);
        assert!(t.starts_with("rollback to release 0a1b2c3d4e5f"), "{t}");
        assert!(t.contains("· a rollback isn't held") && t.contains("⚠ undoes adt: replaces 1234"), "{t}");
    }
}
