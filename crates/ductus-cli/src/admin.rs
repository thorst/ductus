//! `ductus token` (API tokens, straight in the engine's database: that's how the first one is
//! made), and through a running engine's REST API: `ductus drain` / `ductus resume`, `ductus
//! pause`, `ductus retention`, `ductus hotspots`.

use anyhow::{Context, Result, bail};
use ductus_store::{Role, Store};
use serde_json::Value;
use std::time::Duration;

pub enum TokenCommand {
    Create { name: String, role: String, permissions: Vec<String> },
    List { revoked: bool },
    Revoke { name: String },
}

pub async fn token(database_url: &str, command: TokenCommand) -> Result<()> {
    let store = Store::connect(database_url, 2).await?;
    match command {
        TokenCommand::Create { name, role, permissions } => {
            let role =
                Role::parse(&role).with_context(|| format!("unknown role `{role}` (viewer, operator or admin)"))?;
            let (token, secret) = store.create_token(&name, role, &permissions, None).await?;
            println!("{secret}");
            eprintln!(
                "token `{}` ({}) created. This is the only time it's shown: keep it in a secret store.\n\
                 Use it as `Authorization: Bearer <token>`, or DUCTUS_API_TOKEN for `ductus drain`.",
                token.name,
                role.label()
            );
        }
        TokenCommand::List { revoked } => {
            let tokens = store.tokens(revoked, None).await?;
            if tokens.is_empty() {
                println!("no tokens yet: create one with `ductus token create <name> --role admin`");
            }
            let when = |t: Option<chrono::DateTime<chrono::Utc>>| {
                t.map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or("-".into())
            };
            for t in tokens {
                let revoked = t.revoked_at.map(|r| format!("  revoked {}", when(Some(r)))).unwrap_or_default();
                let extra =
                    if t.permissions.is_empty() { String::new() } else { format!("  [{}]", t.permissions.join(", ")) };
                let owner = t.owner.map(|o| format!("  of {o}")).unwrap_or_default();
                println!(
                    "{:<24} {:<9} created {}  last used {}{extra}{owner}{revoked}",
                    t.name,
                    t.role.label(),
                    when(Some(t.created_at)),
                    when(t.last_used_at)
                );
            }
        }
        TokenCommand::Revoke { name } => {
            if !store.revoke_token(&name, None).await? {
                bail!("no live token named `{name}` (see `ductus token list`)");
            }
            println!("token `{name}` revoked: it stops working at once");
        }
    }
    Ok(())
}

pub enum UserCommand {
    Create {
        username: String,
        role: String,
        display_name: Option<String>,
        email: Option<String>,
        permissions: Vec<String>,
        password_stdin: bool,
        must_change: bool,
    },
    List,
    Set {
        username: String,
        role: Option<String>,
        permissions: Vec<String>,
    },
    Password {
        username: String,
        password_stdin: bool,
    },
    Disable {
        username: String,
        disabled: bool,
    },
    Unlock {
        username: String,
    },
}

/// A password from stdin (one line), or asked for twice without showing it.
fn read_password(from_stdin: bool, username: &str, min_length: usize) -> Result<String> {
    use std::io::BufRead;
    let password = if from_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).context("reading the password from stdin")?;
        line.trim_end_matches(['\r', '\n']).to_string()
    } else {
        let first = rpassword::prompt_password("Password: ").context("reading the password")?;
        let again = rpassword::prompt_password("Again: ").context("reading the password")?;
        if first != again {
            bail!("the two passwords differ");
        }
        first
    };
    ductus_core::password::check_policy(username, &password, min_length).map_err(|why| anyhow::anyhow!(why))?;
    Ok(password)
}

/// `ductus user ...`: accounts, straight in the engine's database.
pub async fn user(database_url: &str, root: &std::path::Path, command: UserCommand) -> Result<()> {
    let settings = ductus_core::settings::Settings::load(root)?;
    let min_length = settings.auth.password.min_length;
    let store = Store::connect(database_url, 2).await?;
    let find = |name: String| {
        let store = store.clone();
        async move {
            store.user_by_name(&name).await?.with_context(|| format!("no user named `{name}` (see `ductus user list`)"))
        }
    };
    match command {
        UserCommand::Create { username, role, display_name, email, permissions, password_stdin, must_change } => {
            let role =
                Role::parse(&role).with_context(|| format!("unknown role `{role}` (viewer, operator or admin)"))?;
            let password = read_password(password_stdin, &username, min_length)?;
            let hash = ductus_core::password::hash(&password)?;
            let user = store
                .create_user(&ductus_store::NewUser {
                    username: &username,
                    display_name: display_name.as_deref().unwrap_or(""),
                    email: email.as_deref().unwrap_or(""),
                    role,
                    permissions: &permissions,
                    password_hash: Some(&hash),
                    must_change_password: must_change,
                    source: "local",
                })
                .await?;
            println!("user `{}` ({}) created", user.username, user.role.label());
        }
        UserCommand::List => {
            let users = store.users().await?;
            if users.is_empty() {
                println!("no users yet: create one with `ductus user create <name> --role admin`");
            }
            for u in users {
                let state = if u.disabled_at.is_some() {
                    "  disabled"
                } else if u.locked_until.is_some_and(|t| t > chrono::Utc::now()) {
                    "  locked"
                } else if u.must_change_password {
                    "  must change password"
                } else {
                    ""
                };
                let extra =
                    if u.permissions.is_empty() { String::new() } else { format!("  [{}]", u.permissions.join(", ")) };
                println!("{:<24} {:<9} {}{extra}{state}", u.username, u.role.label(), u.email);
            }
        }
        UserCommand::Set { username, role, permissions } => {
            let user = find(username).await?;
            let role = role
                .map(|r| Role::parse(&r).with_context(|| format!("unknown role `{r}` (viewer, operator or admin)")))
                .transpose()?;
            let change = ductus_store::UserChange {
                role,
                permissions: (!permissions.is_empty()).then_some(permissions),
                ..Default::default()
            };
            let user = store.update_user(user.id, &change).await?;
            println!("user `{}` is now {} [{}]", user.username, user.role.label(), user.permissions.join(", "));
        }
        UserCommand::Password { username, password_stdin } => {
            let user = find(username).await?;
            let password = read_password(password_stdin, &user.username, min_length)?;
            let hash = ductus_core::password::hash(&password)?;
            store.set_password(user.id, &hash, true, None).await?;
            println!("`{}` has a new temporary password; they must change it at their next sign-in", user.username);
        }
        UserCommand::Disable { username, disabled } => {
            let user = find(username).await?;
            let change = ductus_store::UserChange { disabled: Some(disabled), ..Default::default() };
            let user = store.update_user(user.id, &change).await?;
            println!(
                "user `{}` {}",
                user.username,
                if disabled { "disabled: signed out, tokens stopped" } else { "enabled" }
            );
        }
        UserCommand::Unlock { username } => {
            let user = find(username).await?;
            store.unlock_user(user.id).await?;
            println!("user `{}` can try again", user.username);
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

/// Each interface's and destination's state from the running engine (`ductus status`).
pub async fn health(remote: &Remote) -> Result<String> {
    let interfaces = remote.call(reqwest::Method::GET, "/interfaces", None).await?;
    let mut out = String::new();
    for i in interfaces.as_array().into_iter().flatten() {
        let text = |v: &Value| v["health"]["summary"].as_str().unwrap_or("?").to_string();
        out += &format!("{:<20} {}\n", i["name"].as_str().unwrap_or("?"), text(i));
        for d in i["destinations"].as_array().into_iter().flatten() {
            out += &format!("{:<20} {}\n", format!("  → {}", d["name"].as_str().unwrap_or("?")), text(d));
        }
    }
    Ok(out)
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

/// The API path of a destination's pause. One given with `destination:` is named `default`.
fn pause_path(interface: &str, destination: Option<&str>) -> String {
    let destination = destination.unwrap_or(ductus_core::config::DEFAULT_DESTINATION);
    format!("/interfaces/{}/destinations/{}/pause", urlencoding(interface), urlencoding(destination))
}

/// "`adt` destination `lab`" (or "`adt`" for its only destination).
fn destination_label(status: &Value) -> String {
    let (interface, destination) = (status["interface"].as_str().unwrap_or("?"), status["destination"].as_str());
    match destination {
        Some(d) if d != ductus_core::config::DEFAULT_DESTINATION => format!("`{interface}` destination `{d}`"),
        _ => format!("`{interface}`"),
    }
}

fn local_time(value: &Value) -> String {
    value
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

/// `ductus pause <interface> [destination] [--reason]`.
pub async fn pause(remote: Remote, interface: &str, destination: Option<&str>, reason: Option<&str>) -> Result<()> {
    let body = serde_json::json!({ "reason": reason });
    let status = remote.call(reqwest::Method::POST, &pause_path(interface, destination), Some(body)).await?;
    let paused = &status["paused"];
    let pending = status["pending"].as_i64().unwrap_or(0);
    println!(
        "{} paused by {} at {}: nothing is sent until `ductus resume {interface}{}` ({pending} waiting)",
        destination_label(&status),
        paused["by"].as_str().unwrap_or("?"),
        local_time(&paused["at"]),
        destination.map(|d| format!(" {d}")).unwrap_or_default(),
    );
    match paused["reason"].as_str() {
        Some(reason) => println!("reason: {reason}"),
        None => println!(
            "no reason yet: add one when you can with `ductus pause {interface}{} --reason \"...\"`",
            destination.map(|d| format!(" {d}")).unwrap_or_default()
        ),
    }
    Ok(())
}

/// `ductus resume <interface> [destination]`: end a destination's pause.
pub async fn unpause(remote: Remote, interface: &str, destination: Option<&str>) -> Result<()> {
    let status = remote.call(reqwest::Method::DELETE, &pause_path(interface, destination), None).await?;
    let pending = status["pending"].as_i64().unwrap_or(0);
    match status["paused"].as_object() {
        Some(_) => println!(
            "{} resumed (paused since {}): sending {pending} waiting, in order",
            destination_label(&status),
            local_time(&status["paused"]["at"])
        ),
        None => println!("{} wasn't paused ({pending} waiting)", destination_label(&status)),
    }
    Ok(())
}

/// One interface's retention, as `GET /retention` describes it.
fn retention_line(i: &Value) -> String {
    // The API gives days; retention is written in weeks.
    let weeks = |v: &Value| match v.as_i64() {
        Some(d) if d % 7 == 0 => format!("{}w", d / 7),
        Some(d) => format!("{d}d"),
        None => "?".into(),
    };
    let mut line = format!("{:<28} {}", i["name"].as_str().unwrap_or("?"), weeks(&i["days"]));
    if let Some(change) = i["change"].as_object() {
        let left =
            change.get("rows_left").and_then(Value::as_i64).map(|n| format!(", {n} messages left")).unwrap_or_default();
        line += &format!("  changing from {}{left}", weeks(&change["from"]));
    }
    if !i["waiting"].is_null() {
        let waiting = weeks(&i["waiting"]);
        if i["needs_confirmation"].as_bool() == Some(true) {
            let deletes = i["would_delete"].as_i64().map(|n| format!(", deletes {n} messages")).unwrap_or_default();
            line += &format!(
                "  configured {waiting}{deletes}: needs `ductus retention confirm {}`",
                i["name"].as_str().unwrap_or("?")
            );
        } else {
            line += &format!("  configured {waiting}: waits for the running change");
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
    let kept = report["kept"].as_array().cloned().unwrap_or_default();
    if !kept.is_empty() {
        println!(
            "
expired partitions kept:"
        );
    }
    for h in &kept {
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
    let held = &report["held"];
    let count = |v: &Value, key: &str| v[key].as_i64().unwrap_or(0);
    if count(held, "messages") > 0 {
        println!(
            "\nheld area (messages past their retention that someone still needs): {} pending, {} quarantined, {} in all, {}",
            count(held, "pending"),
            count(held, "quarantined"),
            count(held, "messages"),
            size(count(held, "bytes")),
        );
        for m in held["months"].as_array().into_iter().flatten() {
            println!(
                "  {}: {} pending, {} quarantined, {} in all, {}",
                m["month"].as_str().unwrap_or("?"),
                count(m, "pending"),
                count(m, "quarantined"),
                count(m, "messages"),
                size(count(m, "bytes")),
            );
        }
        for i in held["interfaces"].as_array().into_iter().flatten() {
            println!(
                "  {:<28} {} pending, {} quarantined",
                i["name"].as_str().unwrap_or("?"),
                count(i, "pending"),
                count(i, "quarantined")
            );
        }
    }
    Ok(())
}

/// `ductus hotspots [interface]`: where each destination's backlog sits, lane by lane.
pub async fn hotspots(remote: Remote, interface: Option<&str>, lanes: Option<usize>) -> Result<()> {
    let mut query = Vec::new();
    if let Some(i) = interface {
        query.push(format!("interface={}", urlencoding(i)));
    }
    if let Some(n) = lanes {
        query.push(format!("lanes={n}"));
    }
    let path = if query.is_empty() { "/hotspots".to_string() } else { format!("/hotspots?{}", query.join("&")) };
    let report = remote.call(reqwest::Method::GET, &path, None).await?;
    let destinations = report.as_array().cloned().unwrap_or_default();
    if destinations.is_empty() {
        println!("nothing waiting{}", interface.map(|i| format!(" for `{i}`")).unwrap_or_default());
        return Ok(());
    }
    let int = |v: &Value| v.as_i64().unwrap_or(0);
    for (n, d) in destinations.iter().enumerate() {
        if n > 0 {
            println!();
        }
        let destination = d["destination"].as_str().unwrap_or("?");
        let name = match destination {
            ductus_core::config::DEFAULT_DESTINATION => d["interface"].as_str().unwrap_or("?").to_string(),
            _ => format!("{} → {destination}", d["interface"].as_str().unwrap_or("?")),
        };
        println!("{name}: {}", d["summary"].as_str().unwrap_or("?"));
        let workers = d["workers"].as_array().cloned().unwrap_or_default();
        if int(&d["workers_total"]) > 1 {
            println!("  {:<8} {:>6} {:>10}", "worker", "lanes", "waiting");
            for w in &workers {
                let blocked = w["blocked_by"].as_i64().map(|l| format!("  blocked (lane {l})")).unwrap_or_default();
                println!(
                    "  {:<8} {:>6} {:>10}{blocked}",
                    int(&w["worker"]),
                    int(&w["lanes_waiting"]),
                    int(&w["waiting"])
                );
            }
        }
        println!(
            "  {:<6} {:<7} {:>9}  {:<12} {:<16} {:>10} {:>8}  last error",
            "lane", "worker", "waiting", "first msg", "key", "age", "attempts"
        );
        for l in d["lanes"].as_array().into_iter().flatten() {
            let head = &l["head"];
            let worker = l["worker"].as_i64().map(|w| w.to_string()).unwrap_or_else(|| "none".into());
            let waited = ductus_core::monitor::human(Duration::from_secs(int(&head["waiting_s"]).max(0) as u64));
            let error = head["last_error"].as_str().and_then(|e| e.lines().next()).unwrap_or("");
            println!(
                "  {:<6} {:<7} {:>9}  {:<12} {:<16} {:>10} {:>8}  {error}",
                int(&l["lane"]),
                worker,
                int(&l["waiting"]),
                int(&head["id"]),
                head["ordering_key"].as_str().unwrap_or("-"),
                waited,
                int(&head["attempts"]),
            );
        }
        let listed = d["lanes"].as_array().map_or(0, Vec::len) as i64;
        if int(&d["lanes_waiting"]) > listed {
            println!("  ({} more lanes waiting: --lanes N lists more)", int(&d["lanes_waiting"]) - listed);
        }
    }
    Ok(())
}

/// Bytes as people read them: `512 KB`, `3.2 GB`.
fn size(bytes: i64) -> String {
    let mut value = bytes as f64;
    for unit in ["bytes", "KB", "MB", "GB"] {
        if value < 1024.0 {
            return if unit == "bytes" { format!("{bytes} bytes") } else { format!("{value:.1} {unit}") };
        }
        value /= 1024.0;
    }
    format!("{value:.1} TB")
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
