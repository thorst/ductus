//! `ductus token` (API tokens, straight in the engine's database: that's how the first one is
//! made), and through a running engine's REST API: `ductus drain` / `ductus resume`, `ductus
//! retention`.

use anyhow::{Context, Result, bail};
use ductus_store::{Role, Store};
use serde_json::Value;
use std::time::Duration;

pub enum TokenCommand {
    Create { name: String, role: String },
    List { revoked: bool },
    Revoke { name: String },
}

pub async fn token(database_url: &str, command: TokenCommand) -> Result<()> {
    let store = Store::connect(database_url, 2).await?;
    match command {
        TokenCommand::Create { name, role } => {
            let role =
                Role::parse(&role).with_context(|| format!("unknown role `{role}` (viewer, operator or admin)"))?;
            let (token, secret) = store.create_token(&name, role).await?;
            println!("{secret}");
            eprintln!(
                "token `{}` ({}) created. This is the only time it's shown: keep it in a secret store.\n\
                 Use it as `Authorization: Bearer <token>`, or DUCTUS_API_TOKEN for `ductus drain`.",
                token.name,
                role.label()
            );
        }
        TokenCommand::List { revoked } => {
            let tokens = store.tokens(revoked).await?;
            if tokens.is_empty() {
                println!("no tokens yet: create one with `ductus token create <name> --role admin`");
            }
            let when = |t: Option<chrono::DateTime<chrono::Utc>>| {
                t.map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or("-".into())
            };
            for t in tokens {
                let revoked = t.revoked_at.map(|r| format!("  revoked {}", when(Some(r)))).unwrap_or_default();
                println!(
                    "{:<24} {:<9} created {}  last used {}{revoked}",
                    t.name,
                    t.role.label(),
                    when(Some(t.created_at)),
                    when(t.last_used_at)
                );
            }
        }
        TokenCommand::Revoke { name } => {
            if !store.revoke_token(&name).await? {
                bail!("no live token named `{name}` (see `ductus token list`)");
            }
            println!("token `{name}` revoked: it stops working at once");
        }
    }
    Ok(())
}

/// Talks to a running engine's REST API.
pub struct Remote {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl Remote {
    pub fn new(api: &str, token: Option<String>) -> Result<Self> {
        let token = token.filter(|t| !t.trim().is_empty()).context(
            "set DUCTUS_API_TOKEN to a token (create one on the server: `ductus token create <name> --role operator`)",
        )?;
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { base: format!("{}/api/v1", api.trim_end_matches('/')), token, client })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.with_context(|| {
            format!("can't reach the engine's API at {} (is it running? `api.listen` in ductus.yaml)", self.base)
        })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{}: {}", status, body["error"].as_str().unwrap_or("no details"));
        }
        Ok(body)
    }
}

fn describe(drain: &Value) -> String {
    let pending = drain["pending"].as_i64().unwrap_or(0);
    let waiting: Vec<&str> = drain["waiting_for"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    match drain["state"].as_str().unwrap_or("?") {
        "draining" if waiting.is_empty() => format!("draining: {pending} pending"),
        "draining" => format!("draining: {pending} pending ({})", waiting.join(", ")),
        state => state.to_string(),
    }
}

/// `ductus drain`: stop taking messages; with `wait`, until nothing is pending.
pub async fn drain(remote: Remote, stop: bool, wait: bool) -> Result<()> {
    let mut status = remote.call(reqwest::Method::POST, "/drain", Some(serde_json::json!({ "stop": stop }))).await?;
    println!(
        "not taking new messages; delivering what's stored{}",
        if stop { " (the engine exits once drained)" } else { "" }
    );
    let mut last = describe(&status);
    println!("{last}");
    if !wait {
        if status["state"] != "drained" {
            println!("(`ductus drain --wait` waits until drained; `ductus resume` takes messages again)");
        }
        return Ok(());
    }
    while status["state"] != "drained" {
        tokio::time::sleep(Duration::from_secs(1)).await;
        status = match remote.call(reqwest::Method::GET, "/drain", None).await {
            Ok(s) => s,
            // With `stop`, the engine exits once drained: its API going away is the end.
            Err(_) if stop => {
                println!("drained; the engine has stopped");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let now = describe(&status);
        if now != last {
            println!("{now}");
            last = now;
        }
    }
    Ok(())
}

/// `ductus resume`: take messages again.
pub async fn resume(remote: Remote) -> Result<()> {
    let status = remote.call(reqwest::Method::DELETE, "/drain", None).await?;
    println!("{}: taking new messages", status["state"].as_str().unwrap_or("?"));
    Ok(())
}

/// One interface's retention, as `GET /retention` describes it.
fn retention_line(i: &Value) -> String {
    let days = |v: &Value| v.as_i64().map(|d| format!("{d}d")).unwrap_or("?".into());
    let mut line = format!("{:<28} {}", i["name"].as_str().unwrap_or("?"), days(&i["days"]));
    if let Some(change) = i["change"].as_object() {
        let left =
            change.get("rows_left").and_then(Value::as_i64).map(|n| format!(", {n} messages left")).unwrap_or_default();
        line += &format!("  changing from {}{left}", days(&change["from"]));
    }
    if let Some(waiting) = i["waiting"].as_i64() {
        if i["needs_confirmation"].as_bool() == Some(true) {
            let deletes = i["would_delete"].as_i64().map(|n| format!(", deletes {n} messages")).unwrap_or_default();
            line += &format!(
                "  configured {waiting}d{deletes}: needs `ductus retention confirm {}`",
                i["name"].as_str().unwrap_or("?")
            );
        } else {
            line += &format!("  configured {waiting}d: waits for the running change");
        }
    }
    line
}

/// `ductus retention`: each interface's retention and what's in progress.
pub async fn retention(remote: Remote) -> Result<()> {
    let report = remote.call(reqwest::Method::GET, "/retention", None).await?;
    for i in report["interfaces"].as_array().into_iter().flatten() {
        println!("{}", retention_line(i));
    }
    let held = report["held"].as_array().cloned().unwrap_or_default();
    if !held.is_empty() {
        println!(
            "
expired partitions kept:"
        );
    }
    for h in &held {
        let mut why = Vec::new();
        for (what, key) in [("pending", "pending"), ("quarantined", "quarantined")] {
            if let Some(n) = h[key].as_i64().filter(|n| *n > 0) {
                why.push(format!("{n} {what}"));
            }
        }
        let kept: Vec<&str> = h["kept_for"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if !kept.is_empty() {
            why.push(format!("kept longer by {}", kept.join(", ")));
        }
        println!("  {}: {}", h["partition"].as_str().unwrap_or("?"), why.join("; "));
    }
    Ok(())
}

/// `ductus retention confirm <interface>`: apply a shorter retention, which deletes messages.
pub async fn confirm_retention(remote: Remote, interface: &str) -> Result<()> {
    let path = format!("/retention/{}/confirm", urlencoding(interface));
    let state = remote.call(reqwest::Method::POST, &path, None).await?;
    println!("confirmed: {}", retention_line(&state));
    Ok(())
}

/// Interface names are lower-case letters, digits, `-` and `_` in practice; anything else is
/// percent-encoded so it stays one path segment.
fn urlencoding(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
