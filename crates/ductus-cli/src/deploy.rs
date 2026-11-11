//! `ductus deploy` (0.7.4.4): send a release to an engine in release mode and deploy it there
//! (`POST /releases` when it's a file the engine hasn't stored, then `POST /deployments`), and
//! say what happened: the "replaces" check, the gate (run or reused), the handover's steps and
//! what changed.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub struct Deploy {
    /// A release file, or a stored release's id (or its start).
    pub release: String,
    pub to: String,
    pub token: Option<String>,
    pub reason: Option<String>,
    pub force: bool,
    pub all_tests: bool,
    pub json: bool,
    pub timeout: Duration,
}

/// Deploy; Ok(true) when the release runs there.
pub async fn run(d: Deploy) -> Result<bool> {
    let token = d.token.filter(|t| !t.trim().is_empty()).context(
        "set DUCTUS_API_TOKEN to a token with the `deploy` permission (on the server: `ductus token create ci --role operator --permission deploy`)",
    )?;
    let client = reqwest::Client::builder().timeout(d.timeout).build()?;
    let base = format!("{}/api/v1", d.to.trim_end_matches('/'));
    let reach = || format!("can't reach the engine's API at {base} (is it running? `--to`, DUCTUS_API_URL)");
    let path = Path::new(&d.release);
    let id = if path.is_file() {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let response = client
            .post(format!("{base}/releases"))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "application/gzip")
            .body(bytes)
            .send()
            .await
            .with_context(reach)?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("storing {} on the engine: {status}: {}", path.display(), error(&body));
        }
        if !d.json {
            let short = body["short"].as_str().unwrap_or("?");
            if body["new"] == true {
                println!("stored release {short} on the engine");
            } else {
                println!("release {short} was stored on the engine before");
            }
        }
        body["id"].as_str().context("the engine didn't say the release's id")?.to_string()
    } else if d.release.contains(['/', '\\']) || d.release.ends_with(".tar.gz") {
        bail!("{}: no such file (give a release file `ductus release` wrote, or a stored release's id)", d.release);
    } else {
        d.release.clone()
    };
    let request = json!({ "release": id, "reason": d.reason, "force": d.force, "all_tests": d.all_tests });
    if !d.json {
        println!("deploying: the gate runs (or a recent passing check is reused), then the handover…");
    }
    let response = client
        .post(format!("{base}/deployments"))
        .bearer_auth(&token)
        .json(&request)
        .send()
        .await
        .with_context(reach)?;
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
    Ok(report["ok"] == true)
}

fn error(body: &Value) -> &str {
    body["error"].as_str().unwrap_or("no details")
}

/// The report in words.
fn text(r: &Value) -> String {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = format!(
        "release {} (project {}) to environment `{}`\n",
        s(&r["short"]),
        s(&r["project"]),
        s(&r["environment"])
    );
    if let Some(replaces) = r["replaces"].as_object().filter(|m| !m.is_empty()) {
        let mark = if r["forced"] == true { "⚠ deployed over" } else { "✗ would undo" };
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
}
