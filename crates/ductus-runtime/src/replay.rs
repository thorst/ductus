//! `ductus replay` (0.6.6.4): a period's originals run again through this version's code, every
//! destination, sending nothing; what each destination's code decides (sent and what, filtered,
//! quarantined) is compared with what was recorded, with kept outputs, and with another version run
//! beside it (`report.rs`). The evidence a change board asks for before an upgrade or a code change.
//!
//! Both versions run alike: one call at a time, interfaces by name, messages in arrival order,
//! destinations in their config's order; the clock pinned to each message's arrival and
//! `Math.random` seeded per message (`Isolate::pin_clock`). Nothing leaves: no destination is
//! connected, routes aren't stored, Postgres sessions are read-only, and writes (`db().execute`,
//! `fetch()` other than GET or HEAD) aren't made: the message is reported "not comparable".

mod report;

pub use report::{Other, OtherSide, OutputKind, Report};

use crate::services::{EngineServices, SharedServices};
use crate::worker::{self, Outcome};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::{BoxFuture, FetchRequest, FetchResponse, Isolate, Services};
use ductus_store::{MessageRef, Recorded, Store};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Messages read per statement.
const PAGE: i64 = 500;

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub database_url: String,
    /// The period: messages received in `[from, to)`.
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Only these interfaces (all when empty).
    pub interfaces: Vec<String>,
    /// At most this many messages per interface, the first ones in the period.
    pub limit: Option<u64>,
}

/// What a destination's code decided for one message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Sent {
        output: String,
    },
    Filtered {
        reason: Option<String>,
    },
    Quarantined {
        reason: String,
    },
    /// Couldn't be decided as the engine would: a write the replay doesn't make, a temporary
    /// failure (a lookup's database down).
    NotComparable {
        why: String,
    },
}

/// One message through one destination's code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub interface: String,
    pub id: i64,
    pub destination: String,
    #[serde(flatten)]
    pub decision: Decision,
    /// The code looked something up (`db().query`, a GET): a difference from the record or a kept
    /// output may be the data, not the code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub looked_up: bool,
    /// What the destination recorded then (read by the side that compares; never emitted).
    #[serde(skip)]
    pub recorded: Option<Recorded>,
}

impl Row {
    /// What rows are compared by, in the order both sides produce them.
    pub fn key(&self) -> (&str, i64, &str) {
        (&self.interface, self.id, &self.destination)
    }
}

/// What a replay produces, in order; `ductus replay --emit` writes each as a JSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// First: who replays.
    Start {
        version: String,
    },
    /// Before an interface's rows. `skipped`: why it isn't replayed. `outputs`: each destination's
    /// output format (how differences are shown).
    Interface {
        interface: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skipped: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        outputs: BTreeMap<String, OutputKind>,
    },
    Row(Row),
    /// After an interface's rows: how many messages, how long.
    Done {
        interface: String,
        messages: u64,
        seconds: f64,
    },
}

/// Start replaying; the events arrive on the receiver (an error ends them). Also returns the
/// interfaces it replays, by name. Must be called on a multi-threaded tokio runtime: database
/// calls run on it, the code on a thread of its own.
pub async fn start(options: Options) -> Result<(Vec<String>, mpsc::Receiver<Result<Event>>)> {
    let env = Environment::load(&options.root, &options.env)?;
    let mut configs = crate::load_interfaces(&options.root, &env)?;
    if !options.interfaces.is_empty() {
        for name in &options.interfaces {
            if !configs.iter().any(|c| &c.name == name) {
                bail!("no interface named `{name}` in {}", options.root.join("interfaces").display());
            }
        }
        configs.retain(|c| options.interfaces.contains(&c.name));
    }
    let resources = ductus_core::Resources::load(&options.root, &env)?;
    let missing = resources.missing_references(&configs);
    if !missing.is_empty() {
        bail!("{}", missing.join("\n"));
    }
    let heap_mb = ductus_core::settings::Settings::load(&options.root)?.isolates.heap_mb;
    let certificates = crate::tls::Certificates::load(&resources.certificates)?;
    let shared = Arc::new(SharedServices::read_only(env, &resources, &certificates)?);
    let store = Store::connect_read_only(&options.database_url).await?;
    let names = configs.iter().map(|c| c.name.clone()).collect();

    let (events, received) = mpsc::channel(1024);
    let io = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("replay".into())
        .spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().expect("building replay runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let replay = Replay { store, io, events: events.clone(), options };
                if let Err(e) = replay.run(configs, shared, heap_mb).await {
                    let _ = events.send(Err(e)).await;
                }
            });
        })
        .context("starting the replay thread")?;
    Ok((names, received))
}

struct Replay {
    store: Store,
    /// The main runtime: database calls run there (see `Worker::db`).
    io: tokio::runtime::Handle,
    events: mpsc::Sender<Result<Event>>,
    options: Options,
}

impl Replay {
    async fn send(&self, event: Event) -> Result<()> {
        self.events.send(Ok(event)).await.map_err(|_| anyhow!("the replay's reader stopped"))
    }

    async fn db<T: Send + 'static>(&self, call: impl Future<Output = Result<T>> + Send + 'static) -> Result<T> {
        self.io.spawn(call).await.context("database task failed")?
    }

    async fn run(self, configs: Vec<InterfaceConfig>, shared: Arc<SharedServices>, heap_mb: usize) -> Result<()> {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let services = ReplayServices { inner: EngineServices::new(shared, Default::default()), calls: calls.clone() };
        let mut isolate = Isolate::new(Some(Rc::new(services)), heap_mb);
        isolate.pin_clock()?;
        // Top-level code of the interfaces' modules runs as the period starts.
        isolate.set_clock(self.options.from.timestamp_millis() as f64, 0)?;
        self.send(Event::Start { version: env!("CARGO_PKG_VERSION").into() }).await?;
        for config in &configs {
            let started = std::time::Instant::now();
            let outputs = config
                .destinations()
                .map(|(name, d)| (name.to_string(), OutputKind::of(config.output_for(d))))
                .collect::<BTreeMap<_, _>>();
            let interface = config.name.clone();
            if config.responds() {
                let skipped = Some("it answers HTTP requests and keeps none: nothing to replay".to_string());
                self.send(Event::Interface { interface, skipped, outputs: BTreeMap::new() }).await?;
                continue;
            }
            let functions = match config.script_path() {
                None => None,
                Some(path) => {
                    let functions = config.functions();
                    let functions: Vec<(&str, &str)> =
                        functions.iter().map(|(n, f)| (n.as_str(), f.as_str())).collect();
                    match isolate.add_functions(&path, &functions, config.timeout).await {
                        Ok((_, ids)) => Some(ids),
                        Err(e) => {
                            let skipped = Some(format!("its code doesn't load: {e:#}"));
                            self.send(Event::Interface { interface, skipped, outputs }).await?;
                            continue;
                        }
                    }
                }
            };
            self.send(Event::Interface { interface: interface.clone(), skipped: None, outputs }).await?;
            let destinations: Vec<&str> = config.destinations().map(|(name, _)| name).collect();
            let (mut after, mut count) = (0i64, 0u64);
            loop {
                let take = match self.options.limit {
                    Some(limit) => PAGE.min(limit.saturating_sub(count) as i64),
                    None => PAGE,
                };
                if take == 0 {
                    break;
                }
                let (store, name, from, to) =
                    (self.store.clone(), interface.clone(), self.options.from, self.options.to);
                let page = self
                    .db(async move {
                        let page = store.originals(&name, from, to, after, take).await?;
                        let recorded = store.recorded(&name, &page).await?;
                        Ok((page, recorded))
                    })
                    .await?;
                let (page, recorded) = page;
                let Some(last) = page.last() else { break };
                after = last.id;
                count += page.len() as u64;
                let mut recorded: HashMap<(i64, String), Recorded> =
                    recorded.into_iter().map(|r| ((r.id, r.destination.clone()), r)).collect();
                for message in page {
                    let input = String::from_utf8_lossy(&message.payload);
                    let reference = MessageRef { id: message.id, received_at: message.received_at };
                    let mut rows = Vec::with_capacity(destinations.len());
                    // In the config's order, as the engine's workers are made: module state a
                    // function keeps sees the same sequence in both versions.
                    for (i, destination) in destinations.iter().enumerate() {
                        let (decision, looked_up) = match &functions {
                            None => (Decision::Sent { output: input.to_string() }, false),
                            Some(ids) => {
                                isolate
                                    .set_clock(message.received_at.timestamp_millis() as f64, seed(message.id, i))?;
                                *calls.borrow_mut() = Calls::default();
                                let meta = worker::meta(reference, destination, message.origin);
                                let result = isolate.call_function(ids[i], &input, Some(&meta)).await;
                                decide(&input, result, std::mem::take(&mut *calls.borrow_mut()))
                            }
                        };
                        rows.push(Row {
                            interface: interface.clone(),
                            id: message.id,
                            destination: destination.to_string(),
                            decision,
                            looked_up,
                            recorded: recorded.remove(&(message.id, destination.to_string())),
                        });
                    }
                    rows.sort_by(|a, b| a.destination.cmp(&b.destination));
                    for row in rows {
                        self.send(Event::Row(row)).await?;
                    }
                }
            }
            let seconds = started.elapsed().as_secs_f64();
            self.send(Event::Done { interface, messages: count, seconds }).await?;
        }
        Ok(())
    }
}

/// `Math.random`'s seed for one message and destination: the same in every version.
fn seed(id: i64, destination: usize) -> u32 {
    let mixed =
        (id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (destination as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    (mixed ^ (mixed >> 32)) as u32
}

/// What the code's result stands for, as the engine decides (`Outcome::of`); a write the replay
/// refused makes it not comparable, even when the code caught the error and went on.
fn decide(input: &str, result: Result<Option<String>>, calls: Calls) -> (Decision, bool) {
    let looked_up = calls.lookups > 0;
    if let Some(write) = calls.write {
        return (Decision::NotComparable { why: format!("writes {write}: not made in a replay") }, looked_up);
    }
    let decision = match Outcome::of(input, Some(result)) {
        Outcome::Deliver(output) => Decision::Sent { output },
        Outcome::Filter(reason) => Decision::Filtered { reason },
        Outcome::Quarantine(reason) => Decision::Quarantined { reason },
        Outcome::Runaway(runaway) => Decision::Quarantined { reason: format!("transform {runaway}") },
        Outcome::Retry(error) => Decision::NotComparable { why: format!("a temporary failure: {error}") },
    };
    (decision, looked_up)
}

/// What one call of interface code asked of the engine.
#[derive(Default)]
struct Calls {
    /// The first write it tried (refused), in words.
    write: Option<String>,
    lookups: u32,
}

/// The engine's services, except that nothing is written: `db().execute` and `fetch()` other than
/// GET or HEAD fail (and mark the call), lookups are counted.
struct ReplayServices {
    inner: EngineServices,
    calls: Rc<RefCell<Calls>>,
}

impl ReplayServices {
    fn refuse<T: 'static>(&self, write: String) -> BoxFuture<T> {
        let error = anyhow!("not made in a replay: {write}");
        self.calls.borrow_mut().write.get_or_insert(write);
        Box::pin(async move { Err(error) })
    }
}

impl Services for ReplayServices {
    fn db_query(&self, resource: &str, sql: &str, params: Vec<serde_json::Value>) -> BoxFuture<Vec<serde_json::Value>> {
        self.calls.borrow_mut().lookups += 1;
        self.inner.db_query(resource, sql, params)
    }

    fn db_execute(&self, resource: &str, _sql: &str, _params: Vec<serde_json::Value>) -> BoxFuture<u64> {
        self.refuse(format!("to database `{resource}`"))
    }

    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse> {
        let method = request.method.to_ascii_uppercase();
        if method == "GET" || method == "HEAD" {
            self.calls.borrow_mut().lookups += 1;
            return self.inner.fetch(request);
        }
        let to = match &request.api {
            Some(api) => format!("api `{api}`"),
            None => reqwest::Url::parse(&request.url)
                .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
                .unwrap_or_else(|_| request.url.clone()),
        };
        self.refuse(format!("({method} to {to})"))
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.inner.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.secret(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_differ_by_message_and_destination_and_never_by_run() {
        assert_eq!(seed(42, 0), seed(42, 0));
        assert_ne!(seed(42, 0), seed(43, 0));
        assert_ne!(seed(42, 0), seed(42, 1));
    }

    #[test]
    fn a_refused_write_makes_the_call_not_comparable_even_when_the_code_went_on() {
        let calls = Calls { write: Some("to database `billing`".into()), lookups: 1 };
        let (decision, looked_up) = decide("in", Ok(Some("out".into())), calls);
        assert_eq!(
            decision,
            Decision::NotComparable { why: "writes to database `billing`: not made in a replay".into() }
        );
        assert!(looked_up);
        let (decision, _) = decide("in", Ok(None), Calls::default());
        assert_eq!(decision, Decision::Filtered { reason: None });
        let error = anyhow!("{} database unavailable: refused", ductus_script::TRANSIENT);
        let (decision, _) = decide("in", Err(error), Calls::default());
        assert!(
            matches!(decision, Decision::NotComparable { why } if why.starts_with("a temporary failure: transform failed: database unavailable"))
        );
    }

    #[test]
    fn events_go_through_json_as_emitted() {
        let row = Row {
            interface: "adt".into(),
            id: 7,
            destination: "lab".into(),
            decision: Decision::Sent { output: "MSH|^~\\&|".into() },
            looked_up: false,
            recorded: None,
        };
        let line = serde_json::to_string(&Event::Row(row)).unwrap();
        assert_eq!(
            line,
            r#"{"event":"row","interface":"adt","id":7,"destination":"lab","decision":"sent","output":"MSH|^~\\&|"}"#
        );
        let Event::Row(back) = serde_json::from_str(&line).unwrap() else { panic!("a row") };
        assert_eq!(back.key(), ("adt", 7, "lab"));
        assert_eq!(back.decision, Decision::Sent { output: "MSH|^~\\&|".into() });
        let filtered: Event = serde_json::from_str(r#"{"event":"row","interface":"a","id":1,"destination":"d","decision":"filtered","reason":null,"looked_up":true}"#).unwrap();
        assert!(matches!(
            filtered,
            Event::Row(Row { decision: Decision::Filtered { reason: None }, looked_up: true, .. })
        ));
    }
}
