//! `ductus try`: one message through an interface's code, every destination, sending nothing
//! (`ductus_runtime::try_message`). The message from a file, a fixture, or the store: the
//! database (`DUCTUS_DATABASE_URL`) or a running engine's API (`--from`, a token with `payloads`),
//! with what each destination recorded then beside what the code decides now.

use crate::project::{interface_dirs, names_interface};
use anyhow::{Context, Result, bail};
use ductus_core::Environment;
use ductus_runtime::inspect::Attach;
use ductus_runtime::replay::Decision;
use ductus_runtime::try_message::{self, Message, Recorded, Trial};
use std::path::PathBuf;
use std::sync::Arc;

/// Where the message comes from.
pub enum Source {
    File(PathBuf),
    Fixture(String),
    /// A stored message: from the database at `database_url`, or a running engine's API.
    Stored {
        id: i64,
        api: Option<(String, Option<String>)>,
        database_url: Option<String>,
    },
}

pub struct Args {
    pub root: PathBuf,
    pub env: String,
    /// Its name, or a file or folder inside its folder.
    pub interface: String,
    pub source: Source,
    /// Only this destination's code; None: every destination.
    pub destination: Option<String>,
    pub json: bool,
    pub inspect: Option<std::net::SocketAddr>,
}

pub async fn run(args: Args) -> Result<()> {
    let env = Environment::load(&args.root, &args.env)?;
    let interfaces = interface_dirs(&args.root, &env)?;
    let iface = interfaces.iter().find(|i| names_interface(&args.root, &args.interface, i)).with_context(|| {
        let names: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
        format!("`{}` isn't an interface (there are: {})", args.interface, names.join(", "))
    })?;
    let name = iface.label();
    let (message, said) = match &args.source {
        Source::File(path) => (from_text(read(path)?), format!("{}", path.display())),
        Source::Fixture(fixture) => {
            let path = iface.dir.join("fixtures").join(fixture);
            (from_text(read(&path)?), format!("fixture {fixture}"))
        }
        Source::Stored { id, api: Some((url, token)), .. } => {
            (from_api(*id, &name, url, token.clone()).await?, format!("message {id} from {url}"))
        }
        Source::Stored { id, database_url, .. } => {
            let url = crate::tools::database_url(database_url.clone())?;
            (from_store(*id, &name, &url).await?, format!("message {id}"))
        }
    };
    let received = message.received_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let received_at = message.received_at.to_rfc3339();
    let stored = message.id.is_some();
    let options = try_message::Options {
        root: args.root.clone(),
        env: args.env.clone(),
        interface: name.clone(),
        message,
        destination: args.destination.clone(),
        inspect: args.inspect,
        waiting: Arc::new(move |url| {
            eprintln!(
                "waiting for a debugger on {url}: VS Code's \"Ductus: debug this message\", or chrome://inspect in Chrome"
            )
        }),
        say: Arc::new(|what| match what {
            Attach::Attached => eprintln!("debugger attached: no time limit on the code while it is"),
            Attach::Detached => eprintln!("debugger detached: the time limit applies again"),
        }),
    };
    let trial = try_message::run(options).await?;
    if args.json {
        let document = serde_json::json!({ "source": said, "received_at": received_at, "trial": trial });
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        print!("{}", text(&trial, &said, &received, stored));
    }
    Ok(())
}

fn read(path: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A message from a file or fixture: received now, stored nowhere.
fn from_text(payload: String) -> Message {
    Message { id: None, received_at: chrono::Utc::now(), origin: None, payload, recorded: Vec::new() }
}

async fn from_store(id: i64, interface: &str, url: &str) -> Result<Message> {
    let store = ductus_store::Store::connect_read_only(url).await?;
    let (row, payload) = store.message(id).await?.with_context(|| format!("no message {id} in the database"))?;
    if row.interface != interface {
        bail!("message {id} is `{}`'s, not `{interface}`'s", row.interface);
    }
    let reference = ductus_store::MessageRef { id: row.id, received_at: row.received_at };
    let recorded = store
        .deliveries(reference)
        .await?
        .into_iter()
        .map(|d| Recorded {
            destination: d.destination,
            status: d.status.label().to_string(),
            error: d.last_error,
            output: d.output.map(|o| String::from_utf8_lossy(&o).into_owned()),
            code_version: d.code_version.as_deref().map(ductus_runtime::code::hex),
        })
        .collect();
    let payload = String::from_utf8_lossy(&payload).into_owned();
    Ok(Message { id: Some(id), received_at: row.received_at, origin: row.origin_id, payload, recorded })
}

/// Through the API: the read is audited there, as every payload read is.
async fn from_api(id: i64, interface: &str, url: &str, token: Option<String>) -> Result<Message> {
    let remote = crate::admin::Remote::new(url, token)?;
    let found = remote.get(&format!("/messages/{id}")).await.with_context(|| format!("reading message {id}"))?;
    if found["interface"] != interface {
        bail!("message {id} is `{}`'s, not `{interface}`'s", found["interface"].as_str().unwrap_or("?"));
    }
    let Some(payload) = found["payload"].as_str() else {
        bail!(
            "the token may not read payloads: give it the `payloads` permission (`ductus token create <name> --permission payloads`)"
        );
    };
    let received_at = found["received_at"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .context("the API's message has no received_at")?
        .with_timezone(&chrono::Utc);
    let text = |v: &serde_json::Value| v.as_str().map(String::from);
    let recorded = found["deliveries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| Recorded {
            destination: text(&d["destination"]).unwrap_or_default(),
            status: text(&d["status"]).unwrap_or_default(),
            error: text(&d["last_error"]),
            output: text(&d["output"]),
            code_version: text(&d["decided_by"]["hash"]),
        })
        .collect();
    Ok(Message { id: Some(id), received_at, origin: None, payload: payload.to_string(), recorded })
}

/// Text for people: each destination's decision, its output, the code's console lines, and what
/// it recorded then.
fn text(trial: &Trial, said: &str, received: &str, stored: bool) -> String {
    let version = match &trial.code_version {
        Some(hex) => format!(" (code version {})", &hex[..ductus_runtime::code::SHOWN.min(hex.len())]),
        None => String::new(),
    };
    let mut out = format!(
        "{}: {said}, received {received}, through the code as it is now{version}; nothing sent\n",
        trial.interface
    );
    let lines = |text: &str, prefix: &str| -> String {
        text.replace("\r\n", "\n").replace('\r', "\n").lines().map(|l| format!("{prefix}{l}\n")).collect()
    };
    if let Some(why) = &trial.without_resources {
        out += &format!("  (without databases and APIs: resources.yaml {why})\n");
    }
    if !trial.loading.is_empty() {
        out += &format!("  while loading\n{}", lines(&trial.loading, "      │ "));
    }
    for t in &trial.destinations {
        let looked = if t.looked_up { " (it looked something up: today's data)" } else { "" };
        out += &format!("  {}: {}{looked}\n", t.destination, decided(&t.decision));
        out += &lines(&t.console, "      │ ");
        if let Decision::Sent { output } = &t.decision {
            out += &lines(output, "      ");
        }
        match &t.recorded {
            Some(then) => {
                out += &format!("    then: {}\n", compared(&t.decision, then));
                if let Some(other) = other_code(then, trial) {
                    out += &format!("          {other}\n");
                }
            }
            None if stored => out += "    then: no delivery recorded for it\n",
            None => {}
        }
    }
    out
}

/// When the record was decided by another code version than the one tried, say so: a difference
/// from it is then the code's change as much as anything else.
fn other_code(then: &Recorded, trial: &Trial) -> Option<String> {
    let (then_version, now) = (then.code_version.as_deref()?, trial.code_version.as_deref()?);
    let short = |hex: &str| hex.chars().take(ductus_runtime::code::SHOWN).collect::<String>();
    (then_version != now)
        .then(|| format!("decided by code version {}, not this one ({})", short(then_version), short(now)))
}

fn decided(decision: &Decision) -> String {
    match decision {
        Decision::Sent { .. } => "sent".into(),
        Decision::Filtered { reason: None } => "filtered".into(),
        Decision::Filtered { reason: Some(why) } => format!("filtered ({why})"),
        Decision::Quarantined { reason } => format!("quarantined: {}", reason.lines().next().unwrap_or("")),
        Decision::NotComparable { why } => format!("not decided: {why}"),
    }
}

/// What it recorded then, said against what it decides now.
fn compared(now: &Decision, then: &Recorded) -> String {
    let same_status = matches!(
        (now, then.status.as_str()),
        (Decision::Sent { .. }, "sent")
            | (Decision::Filtered { .. }, "filtered")
            | (Decision::Quarantined { .. }, "quarantined")
    );
    let error = then.error.as_deref().map(|e| format!(": {}", e.lines().next().unwrap_or(""))).unwrap_or_default();
    match (now, &then.output) {
        (Decision::Sent { output }, Some(kept)) if same_status && kept == output => "sent, the same output".into(),
        (Decision::Sent { .. }, Some(_)) if same_status => "sent, a different output (kept: save_output)".into(),
        _ if same_status => format!("{}{error} (the same decision)", then.status),
        _ => format!("{}{error}", then.status),
    }
}
