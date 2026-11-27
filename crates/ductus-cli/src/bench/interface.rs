//! `ductus bench interface <name>` (0.8.2): what one interface's code does on this machine
//! (`ductus_runtime::load_test`), its lookups mocked, said in words with the numbers.

use super::report::{took, whole};
use crate::project::{interface_dirs, names_interface};
use anyhow::{Context, Result, anyhow, bail};
use ductus_core::Environment;
use ductus_runtime::load_test::{self, Load, Mocks, Until};
use std::path::PathBuf;
use std::time::Duration;

pub struct Args {
    pub root: PathBuf,
    pub env: String,
    /// Its name, or a file or folder inside its folder.
    pub interface: String,
    pub files: Vec<PathBuf>,
    pub threads: Option<usize>,
    pub seconds: u64,
    pub count: Option<u64>,
    /// `KIND=LATENCY` or `NAME=LATENCY`, as typed.
    pub mock: Vec<String>,
    /// `NAME=FILE.json`, as typed.
    pub rows: Vec<String>,
    pub json: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let env = Environment::load(&args.root, &args.env)?;
    let interfaces = interface_dirs(&args.root, &env)?;
    let iface = interfaces.iter().find(|i| names_interface(&args.root, &args.interface, i)).with_context(|| {
        let names: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
        format!("`{}` isn't an interface (there are: {})", args.interface, names.join(", "))
    })?;
    let mocks = mocks(&args.mock, &args.rows)?;
    let until = match args.count {
        Some(n) => Until::Count(n),
        None => Until::Seconds(Duration::from_secs(args.seconds)),
    };
    let options = load_test::Options {
        root: args.root.clone(),
        env: args.env.clone(),
        interface: iface.label(),
        files: args.files.clone(),
        threads: args.threads,
        until,
        mocks: mocks.clone(),
    };
    let load = tokio::task::spawn_blocking(move || load_test::run(options))
        .await
        .map_err(|e| anyhow!("the load test failed: {e}"))??;
    if args.json {
        let document =
            serde_json::json!({ "machine": super::report::machine(), "mocks": mocked(&mocks), "load": load });
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        print!("{}", text(&load, args.files.is_empty()));
    }
    Ok(())
}

/// The mocks from `--mock` and `--rows`: a kind (`db`, `api`, `fetch`) or a resources.yaml name,
/// each to a latency; a name to the rows (or body) in a JSON file.
fn mocks(latencies: &[String], rows: &[String]) -> Result<Mocks> {
    let mut mocks = Mocks::default();
    for given in latencies {
        let (name, latency) = given.split_once('=').with_context(|| {
            format!("`--mock {given}`: give a kind or a resource and a latency, as `db=5ms` or `provider_db=12ms`")
        })?;
        let latency = ductus_core::config::parse_duration(latency).with_context(|| format!("`--mock {given}`"))?;
        match name.trim() {
            "db" => mocks.db = latency,
            "api" => mocks.api = latency,
            "fetch" => mocks.fetch = latency,
            "" => bail!("`--mock {given}`: name a kind (db, api, fetch) or a resource before the `=`"),
            name => {
                mocks.named.insert(name.to_string(), latency);
            }
        }
    }
    for given in rows {
        let (name, file) = given.split_once('=').with_context(|| {
            format!("`--rows {given}`: give a resource and a JSON file, as `provider_db=rows.json`")
        })?;
        let text = std::fs::read_to_string(file).with_context(|| format!("`--rows {given}`: reading {file}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("`--rows {given}`: {file} isn't JSON"))?;
        mocks.rows.insert(name.trim().to_string(), value);
    }
    Ok(mocks)
}

fn mocked(mocks: &Mocks) -> serde_json::Value {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    serde_json::json!({
        "db_ms": ms(mocks.db),
        "api_ms": ms(mocks.api),
        "fetch_ms": ms(mocks.fetch),
        "named_ms": mocks.named.iter().map(|(n, d)| (n.clone(), ms(*d))).collect::<std::collections::BTreeMap<_, _>>(),
        "rows": mocks.rows.keys().collect::<Vec<_>>(),
    })
}

/// For people: the rate, each message's time, what each destination decided, and the lookups.
pub fn text(load: &Load, fixtures: bool) -> String {
    let threads = match load.threads {
        1 => "1 thread".to_string(),
        n => format!("{n} threads"),
    };
    let from = if fixtures { "its fixtures" } else { "the files given" };
    let cycled = match load.inputs {
        1 => format!("1 message from {from}, again and again"),
        n => format!("{n} messages from {from}, in turn"),
    };
    let mut out = format!(
        "{}: {:.1} s on {threads} ({}), {cycled}; databases and APIs mocked, nothing sent\n",
        load.interface, load.seconds, load.threads_why
    );
    let per_thread =
        if load.threads > 1 { format!(" ({} per thread)", whole(load.per_thread_per_second)) } else { String::new() };
    out += &format!("  {} msg/s{per_thread}, {} messages\n", whole(load.per_second), whole(load.messages as f64));
    out += &format!(
        "  each message: p50 {}, p95 {}, p99 {}, slowest {}\n",
        took(load.p50_ms),
        took(load.p95_ms),
        took(load.p99_ms),
        took(load.max_ms)
    );
    for d in &load.decisions {
        let mut parts = Vec::new();
        for (n, what) in
            [(d.sent, "sent"), (d.filtered, "filtered"), (d.quarantined, "quarantined"), (d.retried, "retried")]
        {
            if n > 0 {
                parts.push(format!("{} {what}", percent(n, load.messages)));
            }
        }
        out += &format!("  {}: {}\n", d.destination, parts.join(", "));
        if let Some(why) = &d.first_reason {
            out += &format!("      first: {}\n", why.lines().next().unwrap_or(""));
        }
    }
    if load.lookups.is_empty() {
        out += "  no database or API calls\n";
    }
    for l in &load.lookups {
        let per = l.calls as f64 / load.messages.max(1) as f64;
        let what = match l.kind {
            "db" => format!("database `{}`", l.name),
            "api" => format!("API `{}`", l.name),
            _ => "fetch() to URLs".to_string(),
        };
        let reads = l.calls - l.writes;
        let rows = match (l.given_rows, l.kind) {
            _ if reads == 0 => "writes, answered as done (nothing is written)".to_string(),
            (true, _) => "with the rows given".to_string(),
            (false, "db") => format!("answering no rows (give some: `--rows {}=rows.json`)", l.name),
            (false, _) => format!("answering 200 `{{}}` (give a body: `--rows {}=body.json`)", l.name),
        };
        // Waits well past the mock's latency: more threads than this machine has cores.
        let waited = if l.waited_ms > l.latency_ms * 1.2 + 0.5 {
            format!(" (waited {} on average: the threads outnumber the cores)", took(l.waited_ms))
        } else {
            String::new()
        };
        out +=
            &format!("  {what}: {per:.2} calls per message, each mocked at {}{waited}, {rows}\n", took(l.latency_ms));
    }
    let off_path = load.decisions.iter().any(|d| d.quarantined + d.retried > 0);
    if off_path && load.lookups.iter().any(|l| !l.given_rows && l.calls > l.writes) {
        out += "  → some messages were quarantined or retried with lookups answering nothing: the numbers are for that path. Give the lookups rows (`--rows`) to measure the usual one.\n";
    }
    out
}

/// `n` of `of` as a share, rounded: "100%", "38%", "<1%" (never 0% for some, nor 100% for not all).
fn percent(n: u64, of: u64) -> String {
    let p = (n as f64 * 100.0 / of.max(1) as f64).round() as u64;
    match p {
        _ if n == of => "100%".into(),
        0 => "<1%".into(),
        100 => ">99%".into(),
        p => format!("{p}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn shares_round_and_never_say_none_or_all_when_it_isnt() {
        assert_eq!([percent(1, 2), percent(1, 3), percent(2, 3)], ["50%", "33%", "67%"]);
        assert_eq!([percent(1, 1000), percent(999, 1000), percent(7, 7)], ["<1%", ">99%", "100%"]);
    }
}
