//! `ductus reload` (0.7.5.3.1): ask the running engine to apply its project folder as it is now,
//! in place where it can (only the interfaces that changed take the change; the others never
//! pause), by the handover only for what needs it. Over the control socket where there is one
//! (Linux: the package's `/run/ductus/control`; containers), else the engine's API (Windows, a
//! plain `ductus run`, another machine), with a token that has the `deploy` permission.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub struct Reload {
    /// The control socket named (`--control`, DUCTUS_CONTROL); None: the default one if it's there.
    pub control: Option<PathBuf>,
    /// The engine's API, when there's no control socket.
    pub api: String,
    pub token: Option<String>,
    pub reason: Option<String>,
    pub in_place_only: bool,
    /// Pass this command's environment along (`--with-environment`: the unit's reload, ductus.env
    /// read again): a `DUCTUS_*` value that differs hands over.
    pub with_environment: bool,
    pub json: bool,
    pub timeout: Duration,
}

/// The control socket to use: the one named, else the default when it's there.
fn socket(named: Option<PathBuf>) -> Option<PathBuf> {
    if named.is_some() {
        return named;
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let default = ductus_runtime::handover::control_path().unwrap_or_else(|| "/run/ductus/control".into());
    default.exists().then_some(default)
}

/// Reload; Ok(true) when the engine runs the folder as it is now.
pub async fn run(r: Reload) -> Result<bool> {
    let report = match socket(r.control.clone()) {
        Some(path) => over_socket(&r, &path).await?,
        None => over_api(&r).await?,
    };
    let ok = report["ok"].as_bool().unwrap_or(false);
    if r.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print(&report);
    }
    Ok(ok)
}

async fn over_socket(r: &Reload, path: &std::path::Path) -> Result<Value> {
    use ductus_runtime::handover;
    let environment = r.with_environment.then(|| {
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
    });
    let request = handover::Request {
        timeout_secs: r.timeout.as_secs(),
        environment,
        asked: handover::Asked::here(r.reason.clone()),
        in_place: true,
        in_place_only: r.in_place_only,
    };
    let started = std::time::Instant::now();
    let mut said = Vec::new();
    let json = r.json;
    let done = handover::ask(path, &request, |line| {
        if !json {
            println!("{line}");
        }
        said.push(line.to_string());
    })
    .await?;
    Ok(match done.report {
        Some(report) => report,
        // An engine before 0.7.5.3 (it hands over), or a reload the handover took.
        None => json!({
            "ok": done.ok, "applied": if done.ok { "handover" } else { "refused" }, "outcome": done.text,
            "steps": said, "changes": [], "warnings": {}, "took_ms": started.elapsed().as_millis() as u64,
        }),
    })
}

async fn over_api(r: &Reload) -> Result<Value> {
    if r.with_environment {
        bail!(
            "`--with-environment` passes this command's environment over the control socket, and there's none here: \
               restart the engine to apply its environment"
        );
    }
    let token = r.token.as_deref().filter(|t| !t.trim().is_empty()).context(
        "no control socket here, so `ductus reload` asks the engine's API: set DUCTUS_API_TOKEN to a token with the \
         `deploy` permission (`ductus token create reload --role operator --permission deploy`), and DUCTUS_API_URL \
         (`--api`) when it isn't http://127.0.0.1:7700",
    )?;
    let base = format!("{}/api/v1", r.api.trim_end_matches('/'));
    let client = reqwest::Client::builder().timeout(r.timeout + Duration::from_secs(30)).build()?;
    let response = client
        .post(format!("{base}/engine/reload"))
        .bearer_auth(token)
        .json(&json!({ "reason": r.reason, "in_place_only": r.in_place_only }))
        .send()
        .await
        .with_context(|| format!("can't reach the engine's API at {base} (is it running? `--api`, DUCTUS_API_URL)"))?;
    let status = response.status();
    let report: Value = response.json().await.unwrap_or(Value::Null);
    if report.get("outcome").is_none() {
        bail!("{status}: {}", report["error"].as_str().unwrap_or("no details"));
    }
    Ok(report)
}

/// The report as people read it.
fn print(report: &Value) {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let applied = text(&report["applied"]);
    if applied == "handover" {
        println!("by the handover: {}", text(&report["outcome"]));
        return;
    }
    let plan = &report["plan"];
    for change in plan["changes"].as_array().into_iter().flatten() {
        let what: Vec<String> = change["what"].as_array().into_iter().flatten().map(text).collect();
        let restarts = change["restarts"].as_str().map(|r| format!(" (restarts alone: {r})")).unwrap_or_default();
        println!("  {}: {}{restarts}", text(&change["interface"]), what.join(", "));
    }
    for line in plan["project"].as_array().into_iter().flatten() {
        println!("  {}", text(line));
    }
    for step in report["steps"].as_array().into_iter().flatten() {
        println!("  {}", text(step));
    }
    for (interface, warning) in report["warnings"].as_object().into_iter().flatten() {
        println!("  ⚠ {interface}: over someone else's change: {}", text(warning));
    }
    match report["ok"].as_bool() {
        Some(true) => println!("{} ({} ms)", text(&report["outcome"]), report["took_ms"].as_u64().unwrap_or_default()),
        _ => eprintln!("error: {}", text(&report["outcome"])),
    }
}
