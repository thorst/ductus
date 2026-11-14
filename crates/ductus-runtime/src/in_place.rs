//! What the engine keeps of each running interface to change it while it runs (a deploy in
//! place, 0.7.5): the parts its workers share and the threads its slots run on. A start builds
//! them once (`Kept::insert`); `Kept::replace` gives an interface new code and settings by
//! building new jobs from them and sending each thread its slots' (`worker::Command::Replace`).
//!
//! What it keeps across a change: the interface's wake-ups, health, counts, lanes and gate, and
//! each destination's id, counts, pause switch and (with the same rate limit) its allowance. What
//! it rebuilds: the config and each destination's outlet (TLS, API, SFTP server, route), from the
//! certificates and resources the engine started with (a change to those is a handover:
//! `changes::plan`).

use crate::Wake;
use crate::activity::Pause;
use crate::health::Health;
use crate::ingest::Intake;
use crate::lanes::Lanes;
use crate::limiter::Limiter;
use crate::stats::{DestinationStats, InterfaceStats};
use crate::worker::{Command, Job, Outlet};
use anyhow::{Context, Result, anyhow, bail};
use ductus_core::config::{Destination, DestinationConfig};
use ductus_core::{InterfaceConfig, Resources};
use ductus_script::CodeFile;
use ductus_store::DestinationId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc::UnboundedSender, oneshot};

/// Every running interface's parts, and what their jobs share.
pub struct Kept {
    /// One lock per interface: deploys of different interfaces go at once, two of the same one
    /// in turn.
    interfaces: HashMap<String, Mutex<Interface>>,
    shared: Shared,
}

/// What every interface's jobs and outlets are built with.
pub(crate) struct Shared {
    pub ownership: Arc<crate::ownership::Ownership>,
    pub waiting: Arc<crate::worker::Waiting>,
    pub decided: crate::deployment::Decided,
    pub certificates: Arc<crate::tls::Certificates>,
    pub resources: Resources,
    /// Every queued interface's intake (routes store into their target's).
    pub intakes: HashMap<String, Arc<Intake>>,
}

/// One interface's parts, shared by its slots' jobs.
pub(crate) struct Interface {
    pub config: Arc<InterfaceConfig>,
    pub wake: Arc<Wake>,
    pub health: Arc<Health>,
    pub stats: Arc<InterfaceStats>,
    pub lanes: Arc<Lanes>,
    /// Its destinations' outlets, in `config.destinations()` order.
    pub outlets: Vec<Outlet>,
    /// Where its slots run: each thread's commands and the slots on it.
    pub threads: Vec<Placed>,
}

/// Some of an interface's slots, on one worker thread.
pub(crate) struct Placed {
    pub commands: UnboundedSender<Command>,
    pub slots: Vec<u32>,
}

/// What one destination keeps when its settings change.
pub(crate) struct KeptDestination {
    pub id: DestinationId,
    pub stats: Arc<DestinationStats>,
    pub pause: Arc<Pause>,
    /// Its allowance so far, kept when its rate limit is the same.
    pub limiter: Option<Arc<Limiter>>,
}

impl Shared {
    /// `config`'s destination `name`'s outlet. TLS settings are built from the engine's
    /// certificates (destinations naming the same CA and certificate share them).
    pub fn outlet(
        &self,
        config: &InterfaceConfig,
        name: &str,
        d: &DestinationConfig,
        kept: KeptDestination,
    ) -> Result<Outlet> {
        let tls = match &d.connector {
            Destination::Mllp { tls: Some(t), .. } => Some(self.certificates.client(t)),
            Destination::Http { api: Some(api), .. } => {
                self.resources.apis.get(api).and_then(|a| self.certificates.named(&a.tls()).transpose())
            }
            Destination::Http { ca, certificate, .. } => {
                self.certificates.named(&ductus_core::tls::ConnectTls::new(ca, certificate)).transpose()
            }
            _ => None,
        };
        let tls = tls.transpose().with_context(|| format!("{}: TLS", config.label_destination(name)))?;
        let route = match d.route() {
            Some(target) => Some(self.intakes.get(target).cloned().with_context(|| {
                format!("{}: routes to `{target}`, which isn't a queued interface here", config.label_destination(name))
            })?),
            None => None,
        };
        Ok(Outlet {
            id: kept.id,
            tls,
            stats: kept.stats,
            pause: kept.pause,
            limiter: kept.limiter.or_else(|| {
                config.rate_limit_for(d).map(|r| Arc::new(Limiter::new(r.clone(), std::time::Instant::now())))
            }),
            server: match &d.connector {
                Destination::Sftp { server, .. } => self.resources.sftp.get(server).cloned(),
                _ => None,
            },
            api: match &d.connector {
                Destination::Http { api: Some(api), .. } => self.resources.apis.get(api).cloned(),
                _ => None,
            },
            route,
        })
    }
}

impl Interface {
    /// Slot `slot`'s job (a respond-mode interface's first gets `requests`).
    pub fn job(&self, shared: &Shared, slot: u32, requests: Option<crate::worker::Requests>) -> Job {
        Job {
            config: self.config.clone(),
            slot,
            wake: self.wake.clone(),
            requests,
            health: self.health.clone(),
            outlets: self.outlets.clone(),
            stats: self.stats.clone(),
            ownership: shared.ownership.clone(),
            waiting: shared.waiting.clone(),
            decided: shared.decided.clone(),
            lanes: self.lanes.clone(),
        }
    }

    /// Every slot's job, by the thread it runs on.
    fn jobs(&self, shared: &Shared) -> Vec<(UnboundedSender<Command>, Vec<Job>)> {
        let jobs = |p: &Placed| p.slots.iter().map(|&slot| self.job(shared, slot, None)).collect();
        self.threads.iter().map(|p| (p.commands.clone(), jobs(p))).collect()
    }

    /// The outlets for `config`, whose destinations are this interface's (same names, same
    /// order): each keeps its id, counts and pause, and its allowance if its rate limit is the
    /// same.
    fn outlets_for(&self, shared: &Shared, config: &InterfaceConfig) -> Result<Vec<Outlet>> {
        let old: Vec<_> = self.config.destinations().collect();
        let new: Vec<_> = config.destinations().collect();
        if old.iter().map(|(n, _)| n).ne(new.iter().map(|(n, _)| n)) {
            // TODO(0.7.5.1 item 3): destinations added or removed.
            bail!("`{}`: its destinations changed: that isn't applied in place yet", config.name);
        }
        new.into_iter()
            .zip(old)
            .zip(&self.outlets)
            .map(|(((name, d), (_, was)), outlet)| {
                let same_limit = config.rate_limit_for(d) == self.config.rate_limit_for(was);
                let kept = KeptDestination {
                    id: outlet.id,
                    stats: outlet.stats.clone(),
                    pause: outlet.pause.clone(),
                    limiter: if same_limit { outlet.limiter.clone() } else { None },
                };
                shared.outlet(config, name, d, kept)
            })
            .collect()
    }
}

impl Kept {
    /// What a start built: `interfaces` with the threads their slots run on.
    pub(crate) fn new(shared: Shared, interfaces: impl IntoIterator<Item = Interface>) -> Self {
        let interfaces = interfaces.into_iter().map(|i| (i.config.name.clone(), Mutex::new(i))).collect();
        Self { interfaces, shared }
    }

    /// Whether `name` runs here.
    pub fn contains(&self, name: &str) -> bool {
        self.interfaces.contains_key(name)
    }

    /// Run `config`'s interface on its new code and settings from its next message (see
    /// `worker::Command::Replace`), on every thread its slots run on. Its destinations must be
    /// the same names (`changes::plan` decides what goes in place). Answers the files its new
    /// code loaded. A thread that can't load the new code leaves the interface as it was: threads
    /// that already switched are given the old code back (the same files: V8's own modules, so
    /// nothing loads twice) before the error is returned.
    pub async fn replace(&self, config: InterfaceConfig) -> Result<Vec<CodeFile>> {
        let name = config.name.clone();
        let mut interface =
            self.interfaces.get(&name).with_context(|| format!("interface `{name}` isn't running here"))?.lock().await;
        let config = Arc::new(config);
        let outlets = interface.outlets_for(&self.shared, &config)?;
        let old =
            (std::mem::replace(&mut interface.config, config), std::mem::replace(&mut interface.outlets, outlets));
        let mut switched = Vec::new();
        let mut files = Vec::new();
        let mut failed = None;
        for (commands, jobs) in interface.jobs(&self.shared) {
            match send(&commands, &name, jobs).await {
                Ok(f) => {
                    files = f;
                    switched.push(commands);
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        let Some(error) = failed else { return Ok(files) };
        (interface.config, interface.outlets) = old;
        let back = interface.jobs(&self.shared);
        for (commands, jobs) in back.into_iter().filter(|(c, _)| switched.iter().any(|s| s.same_channel(c))) {
            if let Err(e) = send(&commands, &name, jobs).await {
                // Its old code loaded before: only a runaway during the load gets here.
                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't give a thread its old code back: it runs the new code there");
            }
        }
        Err(error)
    }
}

/// Ask one thread to run `jobs` as `interface` and wait for its answer.
async fn send(commands: &UnboundedSender<Command>, interface: &str, jobs: Vec<Job>) -> Result<Vec<CodeFile>> {
    let (answer, answered) = oneshot::channel();
    commands
        .send(Command::Replace { interface: interface.to_string(), jobs, answer })
        .map_err(|_| anyhow!("`{interface}`: its worker thread has stopped"))?;
    answered.await.map_err(|_| anyhow!("`{interface}`: its worker thread stopped before answering"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn config(yaml: &str) -> InterfaceConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    const ONE: &str = "name: adt\nsource: { mllp: { port: 6661 } }\ndestinations:\n  lab: { file: { dir: lab }, rate_limit: { per_minute: 60 } }\n  ris: { file: { dir: ris }, rate_limit: { per_minute: 60 } }\n";

    /// A thread that answers each `Replace` with `answer(n)` (n: how many it got before), and
    /// tells the test which interface config each one carried.
    fn thread(
        answer: impl Fn(usize) -> Result<Vec<CodeFile>> + Send + 'static,
    ) -> (UnboundedSender<Command>, mpsc::UnboundedReceiver<Arc<InterfaceConfig>>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (seen, configs) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut n = 0;
            while let Some(Command::Replace { jobs, answer: reply, .. }) = rx.recv().await {
                let _ = seen.send(jobs[0].config.clone());
                let _ = reply.send(answer(n));
                n += 1;
            }
        });
        (tx, configs)
    }

    /// `adt` (ONE) on two threads, slot 0 on each.
    fn kept(threads: [UnboundedSender<Command>; 2]) -> Kept {
        let config = Arc::new(config(ONE));
        let shared = Shared {
            ownership: Arc::new(crate::ownership::Ownership::new(true)),
            waiting: Default::default(),
            decided: crate::deployment::Decided::channel().1,
            certificates: Default::default(),
            resources: Default::default(),
            intakes: HashMap::new(),
        };
        let stats = Arc::new(InterfaceStats::new("adt", ["lab".to_string(), "ris".to_string()]));
        let outlets = config
            .destinations()
            .zip(&stats.destinations)
            .enumerate()
            .map(|(i, ((name, d), s))| {
                let kept = KeptDestination {
                    id: i as DestinationId + 1,
                    stats: s.clone(),
                    pause: Default::default(),
                    limiter: None,
                };
                shared.outlet(&config, name, d, kept).unwrap()
            })
            .collect();
        let interface = Interface {
            wake: Arc::new(Wake::new(1, 2)),
            health: Default::default(),
            stats,
            lanes: Arc::new(Lanes::new(crate::lanes::Layout::single(1, "x"), 1, 2)),
            outlets,
            threads: threads.into_iter().map(|commands| Placed { commands, slots: vec![0] }).collect(),
            config,
        };
        Kept::new(shared, [interface])
    }

    async fn running(kept: &Kept) -> (Arc<InterfaceConfig>, Vec<Outlet>) {
        let i = kept.interfaces["adt"].lock().await;
        (i.config.clone(), i.outlets.clone())
    }

    /// Every thread the interface runs on gets its new config, each destination keeping its id,
    /// counts and pause, and its allowance only while its rate limit is the same.
    #[tokio::test]
    async fn every_thread_gets_the_new_config_and_destinations_keep_their_parts() {
        let ((a, mut seen_a), (b, mut seen_b)) = (thread(|_| Ok(Vec::new())), thread(|_| Ok(Vec::new())));
        let kept = kept([a, b]);
        let (_, before) = running(&kept).await;
        let two = ONE.replace(
            "ris: { file: { dir: ris }, rate_limit: { per_minute: 60 } }",
            "ris: { file: { dir: ris2 }, rate_limit: { per_minute: 30 } }",
        );
        kept.replace(config(&two)).await.unwrap();
        for seen in [&mut seen_a, &mut seen_b] {
            assert!(
                seen.recv().await.unwrap().destinations().any(|(_, d)| d.rate_limit.as_ref().unwrap().per_minute == 30)
            );
        }
        let (now, after) = running(&kept).await;
        assert!(now.destinations().any(|(_, d)| d.rate_limit.as_ref().unwrap().per_minute == 30));
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(b.id, a.id);
            assert!(Arc::ptr_eq(&b.stats, &a.stats) && Arc::ptr_eq(&b.pause, &a.pause));
        }
        let same = |o: &Outlet| o.limiter.clone().unwrap();
        assert!(Arc::ptr_eq(&same(&before[0]), &same(&after[0])), "lab's limit didn't change: its allowance is kept");
        assert!(!Arc::ptr_eq(&same(&before[1]), &same(&after[1])), "ris's did: a new allowance");
    }

    /// A thread that can't load the new code leaves the interface as it was: a thread that had
    /// switched is given the old config back.
    #[tokio::test]
    async fn a_thread_that_fails_puts_the_others_back() {
        let (a, mut seen_a) = thread(|_| Ok(Vec::new()));
        let (b, _) = thread(|_| Err(anyhow!("loading `adt`'s new code (its running code goes on): SyntaxError")));
        let kept = kept([a, b]);
        let (old, _) = running(&kept).await;
        let two = ONE.replace("dir: lab", "dir: lab2");
        let error = kept.replace(config(&two)).await.unwrap_err();
        assert!(format!("{error:#}").contains("SyntaxError"));
        assert!(!Arc::ptr_eq(&seen_a.recv().await.unwrap(), &old), "the first thread switched");
        assert!(Arc::ptr_eq(&seen_a.recv().await.unwrap(), &old), "and was given the old config back");
        let (now, _) = running(&kept).await;
        assert!(Arc::ptr_eq(&now, &old), "the old config is what runs");
    }

    /// Destinations added or removed aren't applied here yet: refused, nothing sent.
    #[tokio::test]
    async fn changed_destination_names_are_refused() {
        let ((a, mut seen_a), (b, _)) = (thread(|_| Ok(Vec::new())), thread(|_| Ok(Vec::new())));
        let kept = kept([a, b]);
        let error = kept.replace(config(&ONE.replace("ris:", "pacs:"))).await.unwrap_err();
        assert!(format!("{error:#}").contains("its destinations changed"), "{error:#}");
        assert!(seen_a.try_recv().is_err());
        let error = kept.replace(config(&ONE.replace("adt", "nope"))).await.unwrap_err();
        assert!(format!("{error:#}").contains("interface `nope` isn't running here"), "{error:#}");
    }
}
