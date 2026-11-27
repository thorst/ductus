//! `ductus bench interface <name>` (0.8.2): what one interface's code does on this machine
//! (`ductus_runtime::load_test`), its lookups mocked, said in words with the numbers.

use super::report::{took, whole};
use crate::project::{interface_dirs, names_interface};
use anyhow::{Context, Result, anyhow, bail};
use ductus_core::Environment;
use ductus_runtime::load_test::{self, Load, Mocks, Until};
use std::io::{IsTerminal, Write};
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
    /// The environment's real databases and APIs instead of mocks.
    pub real: bool,
    /// With `real`: don't ask first.
    pub yes: bool,
    /// The interface's busiest rate, messages a second; None: [`PEAK`], said as such.
    pub peak: Option<u64>,
    pub json: bool,
}

/// A busy feed's peak, for "what share" when no `--peak` is given (as `ductus bench` says it).
pub const PEAK: f64 = 150.0;
/// The lanes an ordering key gives when `partitions` isn't set (`defaults::partitions`).
const PARTITIONS: usize = 64;

pub async fn run(args: Args) -> Result<()> {
    let env = Environment::load(&args.root, &args.env)?;
    let interfaces = interface_dirs(&args.root, &env)?;
    let iface = interfaces.iter().find(|i| names_interface(&args.root, &args.interface, i)).with_context(|| {
        let names: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
        format!("`{}` isn't an interface (there are: {})", args.interface, names.join(", "))
    })?;
    let mocks = mocks(&args.mock, &args.rows)?;
    if args.real {
        if let Some(why) = load_test::production_refused(&env.name) {
            bail!("{why}");
        }
        // Said before anything runs, on stderr (`--json`'s output stays the document).
        eprintln!("{}", against(&args.root, &env, &iface.label(), &args)?);
        if !args.yes {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "`--real` runs against `{}`'s real databases and APIs: add --yes to run without asking",
                    env.name
                );
            }
            eprint!("Run it against `{}`? Type yes: ", env.name);
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if answer.trim() != "yes" {
                bail!("not run");
            }
        }
    }
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
        stages: true,
        real: args.real,
    };
    let load = tokio::task::spawn_blocking(move || load_test::run(options))
        .await
        .map_err(|e| anyhow!("the load test failed: {e}"))??;
    let machine = super::report::machine();
    let verdict = verdict(&load, args.peak.map(|p| p as f64), machine.cores);
    if args.json {
        let document = serde_json::json!({
            "machine": machine,
            "mocks": if args.real { serde_json::Value::Null } else { mocked(&mocks) },
            "peak_per_second": args.peak.map_or(PEAK, |p| p as f64),
            "load": load,
            "verdict": verdict,
        });
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        print!("{}", text(&load, args.files.is_empty()));
        for line in &verdict {
            println!("  → {line}");
        }
    }
    Ok(())
}

/// What `--real` will reach, said before it runs: the environment, its resources this interface
/// can use, and that writes are made.
fn against(root: &std::path::Path, env: &Environment, interface: &str, args: &Args) -> Result<String> {
    let resources = ductus_core::Resources::load(root, env)
        .with_context(|| format!("loading resources.yaml for `{}` (`--real` uses its resources)", env.name))?;
    let mut names: Vec<String> = resources.databases.keys().map(|n| format!("database `{n}`")).collect();
    names.extend(resources.apis.keys().map(|n| format!("API `{n}`")));
    let what = match names.len() {
        0 => "no databases or APIs in its resources.yaml (fetch() to URLs is real too)".to_string(),
        _ => names.join(", "),
    };
    let how_long = match args.count {
        Some(n) => format!("{} messages", whole(n as f64)),
        None => format!("{} s", args.seconds),
    };
    Ok(format!(
        "`{interface}` against environment `{}`'s real systems: {what}. For a second's warm-up and {how_long}, as fast as the code runs: its reads and its writes (db().execute, POST, PUT...) are made.",
        env.name
    ))
}

/// What limits it, and what it means at the peak given (or [`PEAK`]): one sentence each.
pub fn verdict(load: &Load, peak: Option<f64>, cores: usize) -> Vec<String> {
    let mut out = Vec::new();
    if load.messages == 0 {
        return out;
    }
    let per_thread = load.per_thread_per_second.max(1e-9);
    // A message's time on its thread, everything included (the bench runs one at a time there).
    let wall = 1000.0 / per_thread;
    if load.lookups_ms >= wall / 2.0 {
        let most = load
            .lookups
            .iter()
            .max_by(|a, b| (a.calls as f64 * a.waited_ms).total_cmp(&(b.calls as f64 * b.waited_ms)));
        let most = most.map_or_else(String::new, |l| format!(", most on {}", lookup_name(l)));
        let alone = 1000.0 / (wall - load.lookups_ms).max(1e-3);
        let mut line = format!(
            "The lookups are the limit: a message waits {} of its {} on them{most}; without the waits this code would do about {} msg/s a thread.",
            fine(load.lookups_ms),
            fine(wall),
            super::report::about(alone)
        );
        if load.respond {
            line += &format!(
                " The engine answers requests side by side while they wait, so with enough callers at once it answers about {}/s; each caller still waits {}.",
                super::report::about(alone),
                fine(wall)
            );
        } else if load.threads == 1 && !load.keyed {
            line += " Without an ordering key messages go one at a time, so each wait holds up the next message.";
        }
        out.push(line);
    } else {
        let sum = |f: fn(&load_test::Stages) -> f64| load.stages.iter().map(f).sum::<f64>();
        let parts = [
            ("its key functions", load.key_ms.unwrap_or(0.0)),
            ("reading the message (decode)", sum(|s| s.decode_ms)),
            ("the functions' own code", sum(|s| s.code_ms)),
            ("serializing the results", sum(|s| s.serialize_ms)),
        ];
        let (what, ms) = parts.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).expect("four parts");
        out.push(format!(
            "The code is the limit: {} msg/s a thread; of each message's {}, the most goes to {what} ({}).",
            whole(per_thread),
            fine(wall),
            fine(ms)
        ));
    }

    let (rate, at) = match peak {
        Some(p) => (p, format!("At your peak of {}/s", whole(p))),
        None => (PEAK, format!("At {}/s (a busy feed's peak; give yours with `--peak`)", whole(PEAK))),
    };
    let cores_busy = load.cpu_ms.map(|cpu| rate * cpu / 1000.0);
    if load.respond {
        // Requests wait side by side: what one engine answers is its code's pace, not one
        // caller's.
        let capacity = 1000.0 / (wall - load.lookups_ms).max(1e-3);
        let mut line = match rate <= capacity {
            true => {
                format!("{at} it uses {} of what one engine answers ({}/s)", share(rate / capacity), whole(capacity))
            }
            false => {
                let needed = (rate / capacity).ceil() as usize;
                format!(
                    "{at} it needs about {needed} engines at this speed ({}/s each): one thread answers a respond-mode interface and no engine claims it, so run {needed} behind a load balancer",
                    whole(capacity)
                )
            }
        };
        match cores_busy {
            Some(busy) if busy >= 1.0 => line += &format!(", its code keeping {busy:.1} cores busy"),
            Some(busy) => line += &format!(", and {} of one core", share(busy)),
            None => {}
        }
        out.push(line + ".");
        return out;
    }
    if rate <= load.per_second {
        let mut line = format!(
            "{at} it uses {} of what it does here ({}/s)",
            share(rate / load.per_second),
            whole(load.per_second)
        );
        if let Some(busy) = cores_busy {
            line += &format!(", and {} of one core", share(busy));
        }
        out.push(line + ".");
    } else {
        let needed = (rate / per_thread).ceil() as usize;
        let workers = format!("`isolation: dedicated` and `workers: {needed}`");
        let config = match (load.keyed, needed > load.lanes) {
            (false, _) => {
                let partitions = if needed > PARTITIONS { format!(", `partitions: {needed}`") } else { String::new() };
                format!(
                    "give it an ordering key (`ordering: {{ key: ... }}`: messages with different keys then go side by side){partitions}, {workers}"
                )
            }
            (true, true) => format!("`partitions: {needed}`, {workers}"),
            (true, false) => workers,
        };
        let mut line =
            format!("{at} it needs about {needed} threads at this speed ({} msg/s each): {config}", whole(per_thread));
        if let Some(busy) = cores_busy
            && busy > cores as f64
        {
            line += &format!(". Its code alone would keep {busy:.1} cores busy, and this machine has {cores}");
        }
        out.push(format!("{line}; check it with `--threads {needed}`."));
    }
    out
}

fn lookup_name(l: &load_test::Lookup) -> String {
    match l.kind {
        "db" => format!("database `{}`", l.name),
        "api" => format!("API `{}`", l.name),
        _ => "fetch() to URLs".to_string(),
    }
}

/// A short time to the digits that matter: `4.2 µs`, `38 µs`, `5.06 ms`.
fn fine(ms: f64) -> String {
    match ms {
        _ if ms < 0.01 => format!("{:.1} µs", ms * 1000.0),
        _ if ms < 1.0 => format!("{:.0} µs", ms * 1000.0),
        _ => took(ms),
    }
}

/// A fraction as a share: `0.4%`, `12%`, `<0.1%`.
fn share(x: f64) -> String {
    let p = x * 100.0;
    match p {
        _ if p < 0.05 => "<0.1%".into(),
        _ if p < 1.0 => format!("{p:.1}%"),
        _ => format!("{p:.0}%"),
    }
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
    let (one, many) = if load.respond { ("request", "requests") } else { ("message", "messages") };
    let cycled = match load.inputs {
        1 => format!("1 {one} from {from}, again and again"),
        n => format!("{n} {many} from {from}, in turn"),
    };
    let resources = match load.real {
        true => format!("`{}`'s real databases and APIs, nothing else sent", load.env),
        false => "databases and APIs mocked, nothing sent".to_string(),
    };
    let mut out = format!(
        "{}: {:.1} s on {threads} ({}), {cycled}; {resources}\n",
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
    let mut used = match load.cpu_ms {
        Some(cpu) => format!("  CPU: {} a message on its thread", fine(cpu)),
        None => "  CPU: not read on this system".to_string(),
    };
    used += &format!("; JavaScript heap: {} a thread after the warm-up\n", super::report::megabytes(load.heap_mb));
    out += &used;
    if let Some(key) = load.key_ms {
        out += &format!("  key functions: {} a message (they run while the sender waits for its ACK)\n", fine(key));
    }
    for d in &load.decisions {
        let mut parts = Vec::new();
        if load.respond {
            for (status, n) in &d.statuses {
                parts.push(format!("{} answered {status}", percent(*n, load.messages)));
            }
        }
        for (n, what) in
            [(d.sent, "sent"), (d.filtered, "filtered"), (d.quarantined, "quarantined"), (d.retried, "retried")]
        {
            if n > 0 && !load.respond {
                parts.push(format!("{} {what}", percent(n, load.messages)));
            }
        }
        let stages = match load.stages.iter().find(|s| s.destination == d.destination) {
            Some(s) if s.timed > 0 => {
                let lookups =
                    if s.lookups_ms > 0.0 { format!(", lookups {}", fine(s.lookups_ms)) } else { String::new() };
                format!(
                    "; decode {}, code {}{lookups}, serialize {}",
                    fine(s.decode_ms),
                    fine(s.code_ms),
                    fine(s.serialize_ms)
                )
            }
            Some(_) => "; not timed (no message could be read as its format says)".to_string(),
            None => String::new(),
        };
        out += &format!("  {}: {}{stages}\n", d.destination, parts.join(", "));
        if let Some(why) = &d.first_reason {
            out += &format!("      first: {}\n", why.lines().next().unwrap_or(""));
        }
    }
    if load.lookups.is_empty() {
        out += "  no database or API calls\n";
    }
    for l in &load.lookups {
        let per = l.calls as f64 / load.messages.max(1) as f64;
        let what = lookup_name(l);
        let reads = l.calls - l.writes;
        let Some(latency) = l.latency_ms else {
            // Real: what it took, its writes made, its failures counted.
            let mut line = format!("  {what}: {per:.2} calls per message, {} on average", took(l.waited_ms));
            if l.writes > 0 {
                line += &format!(", {} of them writes (made)", percent(l.writes, l.calls));
            }
            if l.failed > 0 {
                line += &format!(", {} failed", percent(l.failed, l.calls));
            }
            out += &(line + "\n");
            continue;
        };
        let rows = match (l.given_rows, l.kind) {
            _ if reads == 0 => "writes, answered as done (nothing is written)".to_string(),
            (true, _) => "with the rows given".to_string(),
            (false, "db") => format!("answering no rows (give some: `--rows {}=rows.json`)", l.name),
            (false, _) => format!("answering 200 `{{}}` (give a body: `--rows {}=body.json`)", l.name),
        };
        // Waits well past the mock's latency: more threads than this machine has cores.
        let waited = if l.waited_ms > latency * 1.2 + 0.5 {
            format!(" (waited {} on average: the threads outnumber the cores)", took(l.waited_ms))
        } else {
            String::new()
        };
        out += &format!("  {what}: {per:.2} calls per message, each mocked at {}{waited}, {rows}\n", took(latency));
    }
    let off_path = load.decisions.iter().any(|d| {
        let errors: u64 = d.statuses.range(400..).map(|(_, n)| n).sum();
        d.quarantined + d.retried + errors > 0
    });
    if off_path && load.lookups.iter().any(|l| !l.given_rows && l.latency_ms.is_some() && l.calls > l.writes) {
        let how = if load.respond { "answered with an error" } else { "quarantined or retried" };
        out += &format!(
            "  → some {many} were {how} with lookups answering nothing: the numbers are for that path. Give the lookups rows (`--rows`) to measure the usual one.\n"
        );
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
    use super::{fine, percent, share, verdict};
    use ductus_runtime::load_test::{Load, Lookup, Stages};

    /// One thread doing `rate` a second, `lookups_ms` of each message waiting on `provider_db`.
    fn load(rate: f64, lookups_ms: f64, keyed: bool) -> Load {
        let wall = 1000.0 / rate;
        let code = wall - lookups_ms;
        Load {
            interface: "orders".into(),
            env: "dev".into(),
            real: false,
            respond: false,
            threads: 1,
            threads_why: String::new(),
            inputs: 1,
            messages: 1000,
            seconds: 1000.0 / rate,
            per_second: rate,
            per_thread_per_second: rate,
            p50_ms: wall,
            p95_ms: wall,
            p99_ms: wall,
            max_ms: wall,
            key_ms: None,
            stages: vec![Stages {
                destination: "lab".into(),
                decode_ms: code * 0.2,
                code_ms: code * 0.7,
                lookups_ms,
                serialize_ms: code * 0.1,
                timed: 1000,
            }],
            lookups_ms,
            cpu_ms: Some(code),
            heap_mb: 4.0,
            keyed,
            lanes: if keyed { 64 } else { 1 },
            decisions: Vec::new(),
            lookups: vec![Lookup {
                kind: "db",
                name: "provider_db".into(),
                calls: 1000,
                writes: 0,
                latency_ms: Some(lookups_ms),
                waited_ms: lookups_ms,
                failed: 0,
                given_rows: true,
            }],
        }
    }

    #[test]
    fn the_verdict_names_the_limit_and_what_the_peak_takes() {
        // 5 ms lookups: 190/s, the waits the limit; 150/s fits.
        let said = verdict(&load(190.0, 5.0, false), None, 8);
        assert_eq!(
            said[0],
            "The lookups are the limit: a message waits 5.00 ms of its 5.26 ms on them, most on database `provider_db`; without the waits this code would do about 3,800 msg/s a thread. Without an ordering key messages go one at a time, so each wait holds up the next message."
        );
        assert_eq!(
            said[1],
            "At 150/s (a busy feed's peak; give yours with `--peak`) it uses 79% of what it does here (190/s), and 4% of one core."
        );
        // A peak above it: the threads, and the key it needs for them.
        let said = verdict(&load(190.0, 5.0, false), Some(1000.0), 8);
        assert_eq!(
            said[1],
            "At your peak of 1,000/s it needs about 6 threads at this speed (190 msg/s each): give it an ordering key (`ordering: { key: ... }`: messages with different keys then go side by side), `isolation: dedicated` and `workers: 6`; check it with `--threads 6`."
        );
        // Pure code: the code is the limit; a keyed interface needs only workers, until its lanes run out.
        let said = verdict(&load(20_000.0, 0.0, true), Some(100_000.0), 2);
        assert_eq!(
            said[0],
            "The code is the limit: 20,000 msg/s a thread; of each message's 50 µs, the most goes to the functions' own code (35 µs)."
        );
        assert_eq!(
            said[1],
            "At your peak of 100,000/s it needs about 5 threads at this speed (20,000 msg/s each): `isolation: dedicated` and `workers: 5`. Its code alone would keep 5.0 cores busy, and this machine has 2; check it with `--threads 5`."
        );
        let said = verdict(&load(1000.0, 0.0, true), Some(100_000.0), 64);
        assert!(said[1].contains("`partitions: 100`, `isolation: dedicated` and `workers: 100`"), "{}", said[1]);
    }

    /// Respond mode: requests wait side by side, so one engine answers at its code's pace, and
    /// more than that takes more engines (not workers).
    #[test]
    fn a_responder_s_verdict_counts_requests_side_by_side() {
        let responder = Load { respond: true, ..load(190.0, 5.0, false) };
        let said = verdict(&responder, None, 8);
        assert!(
            said[0].ends_with(
                "The engine answers requests side by side while they wait, so with enough callers at once it answers about 3,800/s; each caller still waits 5.26 ms."
            ),
            "{}",
            said[0]
        );
        assert_eq!(
            said[1],
            "At 150/s (a busy feed's peak; give yours with `--peak`) it uses 4% of what one engine answers (3,800/s), and 4% of one core."
        );
        let said = verdict(&responder, Some(10_000.0), 8);
        assert!(said[1].contains("it needs about 3 engines at this speed (3,800/s each)"), "{}", said[1]);
        assert!(said[1].contains("run 3 behind a load balancer, its code keeping 2.6 cores busy."), "{}", said[1]);
    }

    #[test]
    fn short_times_and_shares_keep_their_digits() {
        assert_eq!([fine(0.0042), fine(0.038), fine(5.061)], ["4.2 µs", "38 µs", "5.06 ms"]);
        assert_eq!([share(0.0004), share(0.004), share(0.12)], ["<0.1%", "0.4%", "12%"]);
    }

    #[test]
    fn shares_round_and_never_say_none_or_all_when_it_isnt() {
        assert_eq!([percent(1, 2), percent(1, 3), percent(2, 3)], ["50%", "33%", "67%"]);
        assert_eq!([percent(1, 1000), percent(999, 1000), percent(7, 7)], ["<1%", ">99%", "100%"]);
    }
}
