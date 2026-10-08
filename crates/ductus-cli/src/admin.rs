//! `ductus token` (API tokens, straight in the engine's database: that's how the first one is
//! made) and `ductus drain` / `ductus resume` (through a running engine's REST API).

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
            "set DUCTUS_API_TOKEN to an operator token (create one on the server: `ductus token create <name> --role operator`)",
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
