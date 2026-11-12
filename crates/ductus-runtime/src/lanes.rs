//! Lanes, and changing them without stranding or reordering anything (0.7.4.4.4).
//!
//! A message's lane is worked out once, at intake, from its ordering key: `first + hash(key) %
//! lanes` of the interface's newest **generation** (`ductus_store` lanes.rs, migration 0033). A
//! change of `ordering` (the lane count, or the key) adds a generation numbered after the last one:
//! new messages are stored (and ACKed) in its lanes at once, and nothing is refused or held at the
//! sender.
//!
//! Workers read the lanes of every generation still listed. Order holds per key because a lane is
//! **held** (not read) for a destination while an older lane that may hold the same keys still has
//! messages for it. Which older lanes those are is arithmetic: a key in lane `i` of `a` lanes lands
//! in lane `j` of `b` lanes only if `i ≡ j (mod gcd(a, b))`, so 64 → 32 holds new lane `j` behind
//! old lanes `j` and `j + 32`, and to or from one lane (the key added or removed), or a changed key,
//! holds every new lane behind every old one. An older lane on the worker's own slot doesn't hold
//! it while that lane is read: a slot reads its lanes oldest message first, one at a time, which
//! keeps their order anyway (so an interface in the shared pool, one worker per destination, holds
//! nothing). One stuck message holds the lanes that share its keys, as it held its own before.
//!
//! **Nothing polls.** A read returns the oldest pending deliveries of the lanes read, up to a
//! limit; a shorter read says the lanes it found nothing in are empty. Old lanes never fill again
//! (intake, a resend and an edited copy all go to the newest lanes), so an empty old lane stays
//! empty: the worker marks it **drained** for its destination, which opens the lanes it held (their
//! workers are woken). When every old lane is drained for every destination, the old generations
//! are deleted from the database (`interface.lanes_settled`): the steady state has one generation,
//! and an interface whose lanes never change has no state here at all.
//!
//! At start (or before a takeover receives) `start` compares each interface's `ordering` with its
//! newest generation and adds one where it differs (`interface.lanes_changed`). An older binary
//! that ran the interface since (it writes the lane count to `interface_applied`, where this one
//! writes the end of its lanes) or an interface with nothing recorded gets its lanes **adopted**:
//! one generation covering every lane anything waits in, whose lanes don't follow any key (every
//! new lane waits for all of it), and the new one after it.

use anyhow::Result;
use ductus_core::{InterfaceConfig, partition_of};
use ductus_store::{Audit, By, LaneGeneration, LaneWrites, NewGeneration, Store};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use tokio::sync::watch;

/// One generation of an interface's lanes: `first .. first + lanes`.
#[derive(Debug, Clone, PartialEq)]
pub struct Generation {
    pub first: i32,
    pub lanes: i32,
    /// `ordering.key` (`""` without one); None: not known (recorded by an older version).
    pub key: Option<String>,
    /// Its lanes don't follow any key (adopted): every newer lane waits for all of it.
    pub mixed: bool,
}

impl Generation {
    pub fn end(&self) -> i32 {
        self.first + self.lanes
    }

    pub fn contains(&self, lane: i32) -> bool {
        (self.first..self.end()).contains(&lane)
    }

    fn of(row: &LaneGeneration) -> Self {
        Self { first: row.first_lane, lanes: row.lanes, key: row.key.clone(), mixed: row.mixed }
    }

    /// The modulus older lanes share keys with this one's by, None: all of them (see the module).
    fn shares_with(&self, older: &Generation) -> Option<i32> {
        let keys_differ = matches!((&older.key, &self.key), (Some(a), Some(b)) if a != b);
        (!older.mixed && !keys_differ).then(|| gcd(older.lanes, self.lanes))
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// An interface's lane generations, oldest first; never empty, the last is where new messages go.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub generations: Vec<Generation>,
}

impl Layout {
    /// One generation of `lanes` lanes from 0.
    pub fn single(lanes: i32, key: &str) -> Self {
        Self { generations: vec![Generation { first: 0, lanes, key: Some(key.to_string()), mixed: false }] }
    }

    pub fn newest(&self) -> &Generation {
        self.generations.last().expect("a layout has a generation")
    }

    /// Lanes are changing: older generations still listed.
    pub fn changing(&self) -> bool {
        self.generations.len() > 1
    }

    /// The lane a message with ordering key `key` (`""` without one) is stored in.
    pub fn lane_of(&self, key: &str) -> i32 {
        let newest = self.newest();
        newest.first + partition_of(key, newest.lanes as u32)
    }

    /// Which generation `lane` is in.
    pub fn generation_of(&self, lane: i32) -> Option<usize> {
        self.generations.iter().position(|g| g.contains(lane))
    }

    /// The lanes a worker of `slot` (of `slots`) owns, every generation's: lane `l` is slot
    /// `l % slots`'s, as `Wake::lane` wakes it.
    pub fn lanes_of_slot(&self, slot: u32, slots: u32) -> Vec<i32> {
        let slots = slots.max(1) as i32;
        self.generations.iter().flat_map(|g| g.first..g.end()).filter(|l| l % slots == slot as i32).collect()
    }

    /// The older lanes that may hold the same keys as `lane` (none for the oldest generation's).
    pub fn peers(&self, lane: i32) -> Vec<i32> {
        let Some(k) = self.generation_of(lane) else { return Vec::new() };
        let newer = &self.generations[k];
        let at = lane - newer.first;
        self.generations[..k]
            .iter()
            .flat_map(|older| {
                let modulus = newer.shares_with(older);
                (older.first..older.end()).filter(move |p| modulus.is_none_or(|m| (p - older.first) % m == at % m))
            })
            .collect()
    }

    /// `lane` as people configured it: "lane 8 of 32", or "old lane 40 of 64" (numbered from 0
    /// within its generation). None: in no generation listed.
    pub fn describe(&self, lane: i32) -> Option<String> {
        let k = self.generation_of(lane)?;
        let g = &self.generations[k];
        let old = if k + 1 < self.generations.len() { "old lane" } else { "lane" };
        Some(format!("{old} {} of {}", lane - g.first, g.lanes))
    }
}

/// An interface's lanes while it runs: the layout, and while it is changing, which old lanes are
/// drained for each destination. Shared by its intake, its workers and the API.
#[derive(Debug)]
pub struct Lanes {
    pub layout: Layout,
    slots: u32,
    changing: Option<Changing>,
}

#[derive(Debug)]
struct Changing {
    /// The old lanes: `first .. end` (every generation but the newest; they're contiguous).
    first: i32,
    end: i32,
    /// `[destination][lane - first]`.
    drained: Box<[AtomicBool]>,
    remaining: AtomicUsize,
    /// Bumped for a destination when one of its old lanes drains: its held workers look again.
    changed: Box<[watch::Sender<u64>]>,
    settled: watch::Sender<bool>,
}

/// What a worker reads now (`Lanes::readable`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readable {
    pub lanes: Vec<i32>,
    /// Lanes of its slot held behind older ones (they open when those drain).
    pub held: Vec<i32>,
}

impl Lanes {
    pub fn new(layout: Layout, slots: u32, destinations: usize) -> Self {
        let changing = layout.changing().then(|| {
            let (first, end) = (layout.generations[0].first, layout.newest().first);
            let old = (end - first) as usize;
            Changing {
                first,
                end,
                drained: (0..old * destinations).map(|_| AtomicBool::new(false)).collect(),
                remaining: AtomicUsize::new(old * destinations),
                changed: (0..destinations).map(|_| watch::Sender::new(0)).collect(),
                settled: watch::Sender::new(old * destinations == 0),
            }
        });
        Self { layout, slots: slots.max(1), changing }
    }

    pub fn lane_of(&self, key: &str) -> i32 {
        self.layout.lane_of(key)
    }

    /// Every lane of `slot`, held or not.
    pub fn of_slot(&self, slot: u32) -> Vec<i32> {
        self.layout.lanes_of_slot(slot, self.slots)
    }

    pub fn is_changing(&self) -> bool {
        self.changing.is_some()
    }

    fn is_drained(&self, c: &Changing, destination: usize, lane: i32) -> bool {
        let old = (c.end - c.first) as usize;
        c.drained[destination * old + (lane - c.first) as usize].load(Relaxed)
    }

    /// Whether `lane` has been drained for `destination` (false for the newest lanes).
    pub fn drained(&self, destination: usize, lane: i32) -> bool {
        match &self.changing {
            Some(c) if (c.first..c.end).contains(&lane) => self.is_drained(c, destination, lane),
            _ => false,
        }
    }

    /// The lanes `slot`'s worker for `destination` reads now: its lanes but drained old ones (empty
    /// for good) and held ones (see the module).
    pub fn readable(&self, slot: u32, destination: usize) -> Readable {
        let mut readable = Readable::default();
        for (lane, holders) in self.assess(slot, destination) {
            if !holders.is_empty() {
                readable.held.push(lane);
            } else if !self.drained(destination, lane) {
                readable.lanes.push(lane);
            }
        }
        readable
    }

    /// Each lane of `slot` with what holds it for `destination` (empty: open): its older peers
    /// that aren't drained, but those of the same slot that are read themselves.
    fn assess(&self, slot: u32, destination: usize) -> Vec<(i32, Vec<i32>)> {
        let mine = self.of_slot(slot);
        let Some(c) = &self.changing else { return mine.into_iter().map(|l| (l, Vec::new())).collect() };
        let mut open: HashMap<i32, bool> = HashMap::new();
        // Oldest first: an older lane's openness is known before a newer one asks.
        mine.into_iter()
            .map(|lane| {
                let holders: Vec<i32> = self
                    .layout
                    .peers(lane)
                    .into_iter()
                    .filter(|&p| {
                        !self.is_drained(c, destination, p)
                            && !(p % self.slots as i32 == slot as i32 && open.get(&p).copied().unwrap_or(false))
                    })
                    .collect();
                open.insert(lane, holders.is_empty());
                (lane, holders)
            })
            .collect()
    }

    /// A read of `read` for `destination` came back short with messages in `found`: the old lanes
    /// read that it found nothing in are empty for good. Marks them; true if any was new.
    pub fn read_short(&self, destination: usize, read: &[i32], found: &HashSet<i32>) -> bool {
        let Some(c) = &self.changing else { return false };
        let lanes: Vec<i32> =
            read.iter().copied().filter(|l| (c.first..c.end).contains(l) && !found.contains(l)).collect();
        self.mark(c, destination, &lanes)
    }

    /// Nothing waited for the interface when the engine took it: `slot`'s old lanes are empty
    /// for `destination`.
    pub fn nothing_waiting(&self, slot: u32, destination: usize) {
        let Some(c) = &self.changing else { return };
        let lanes: Vec<i32> = self.of_slot(slot).into_iter().filter(|l| (c.first..c.end).contains(l)).collect();
        self.mark(c, destination, &lanes);
    }

    fn mark(&self, c: &Changing, destination: usize, lanes: &[i32]) -> bool {
        let old = (c.end - c.first) as usize;
        let mut marked = 0;
        for &lane in lanes {
            if !c.drained[destination * old + (lane - c.first) as usize].swap(true, Relaxed) {
                marked += 1;
            }
        }
        if marked == 0 {
            return false;
        }
        c.changed[destination].send_modify(|n| *n += 1);
        if c.remaining.fetch_sub(marked, Relaxed) == marked {
            c.settled.send_replace(true);
        }
        true
    }

    /// Changes when an old lane drains for `destination`; None when nothing is changing (never).
    pub fn watch(&self, destination: usize) -> Option<watch::Receiver<u64>> {
        self.changing.as_ref().map(|c| c.changed[destination].subscribe())
    }

    /// Resolves once every old lane is drained for every destination (never while not changing).
    pub async fn settled(&self) {
        match &self.changing {
            Some(c) => {
                let _ = c.settled.subscribe().wait_for(|s| *s).await;
            }
            None => std::future::pending().await,
        }
    }

    /// For `destination`, each held lane and the older lanes holding it, by lane (for the API).
    pub fn holding(&self, destination: usize) -> Vec<(i32, Vec<i32>)> {
        if self.changing.is_none() {
            return Vec::new();
        }
        let mut held: Vec<(i32, Vec<i32>)> = (0..self.slots)
            .flat_map(|slot| self.assess(slot, destination))
            .filter(|(_, holders)| !holders.is_empty())
            .collect();
        held.sort();
        held
    }
}

/// What an interface's `ordering` asks for now.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub interface: String,
    /// `ordering.partitions`, or 1 without `ordering.key`.
    pub lanes: i32,
    /// `ordering.key`, `""` without one.
    pub key: String,
}

impl Wanted {
    /// None for respond mode (no queue, no lanes).
    pub fn of(config: &InterfaceConfig) -> Option<Self> {
        (!config.responds()).then(|| Self {
            interface: config.name.clone(),
            lanes: config.partition_count() as i32,
            key: config.ordering.key.clone().unwrap_or_default(),
        })
    }
}

/// A change of an interface's lanes, as `start` found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub interface: String,
    /// The lanes it ran with (None: not recorded).
    pub before: Option<i32>,
    pub now: i32,
    /// The key changed too (an unknown key never counts as changed).
    pub key_changed: bool,
    /// An older binary ran it since, or nothing was recorded: its waiting lanes were adopted.
    pub adopted: bool,
    pub first_lane: i32,
}

/// What `start` decided: each interface's layout, what to write and the changes to tell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decided {
    pub layouts: HashMap<String, Layout>,
    pub writes: LaneWrites,
    pub changes: Vec<Change>,
}

/// The interfaces whose recorded lanes can't be trusted (none recorded, or an older binary ran
/// them since): what waits in them must be read first (`Store::highest_waiting_lanes`).
pub fn to_adopt(
    wanted: &[Wanted],
    generations: &HashMap<String, Vec<Generation>>,
    applied: &HashMap<String, i32>,
) -> Vec<String> {
    wanted
        .iter()
        .filter(|w| {
            let end = generations.get(&w.interface).and_then(|g| g.last()).map(Generation::end);
            end.is_none() || applied.get(&w.interface) != end.as_ref()
        })
        .map(|w| w.interface.clone())
        .collect()
}

/// Each interface's layout from its recorded generations and what its `ordering` asks for (see
/// the module). `highest`: the highest lane anything waits in, for the interfaces `to_adopt` named.
pub fn decide(
    wanted: &[Wanted],
    generations: &HashMap<String, Vec<Generation>>,
    applied: &HashMap<String, i32>,
    highest: &HashMap<String, i32>,
) -> Decided {
    let adopt: HashSet<String> = to_adopt(wanted, generations, applied).into_iter().collect();
    let mut d = Decided::default();
    for w in wanted {
        let new = |first: i32| Generation { first, lanes: w.lanes, key: Some(w.key.clone()), mixed: false };
        let layout = if adopt.contains(&w.interface) {
            d.writes.replaced.push(w.interface.clone());
            let before = applied.get(&w.interface).copied();
            let layout = match highest.get(&w.interface) {
                Some(&h) => {
                    let old = Generation { first: 0, lanes: h + 1, key: None, mixed: true };
                    Layout { generations: vec![old, new(h + 1)] }
                }
                None => Layout { generations: vec![new(0)] },
            };
            if highest.contains_key(&w.interface) || before.is_some_and(|b| b != w.lanes) {
                d.changes.push(Change {
                    interface: w.interface.clone(),
                    before,
                    now: w.lanes,
                    key_changed: false,
                    adopted: true,
                    first_lane: layout.newest().first,
                });
            }
            layout
        } else {
            let mut gens = generations[&w.interface].clone();
            let newest = gens.last().expect("checked by to_adopt").clone();
            let key_changed = newest.key.as_ref().is_some_and(|k| *k != w.key);
            if newest.lanes == w.lanes && !key_changed {
                if newest.key.is_none() {
                    d.writes.keys.push((w.interface.clone(), newest.first, w.key.clone()));
                    gens.last_mut().unwrap().key = Some(w.key.clone());
                }
            } else {
                d.changes.push(Change {
                    interface: w.interface.clone(),
                    before: Some(newest.lanes),
                    now: w.lanes,
                    key_changed,
                    adopted: false,
                    first_lane: newest.end(),
                });
                gens.push(new(newest.end()));
            }
            Layout { generations: gens }
        };
        let old: Vec<&Generation> = generations.get(&w.interface).map(|g| g.iter().collect()).unwrap_or_default();
        for g in &layout.generations {
            let recorded = !d.writes.replaced.contains(&w.interface) && old.iter().any(|o| o.first == g.first);
            if !recorded {
                d.writes.added.push(NewGeneration {
                    interface: w.interface.clone(),
                    first_lane: g.first,
                    lanes: g.lanes,
                    key: g.key.clone().unwrap_or_default(),
                    mixed: g.mixed,
                });
            }
        }
        let end = layout.newest().end();
        if applied.get(&w.interface) != Some(&end) {
            d.writes.applied.push((w.interface.clone(), end));
        }
        d.layouts.insert(w.interface.clone(), layout);
    }
    d
}

/// Read what's recorded and decide (writing nothing): the start's reading, and `validate`'s.
pub async fn read(store: &Store, wanted: &[Wanted]) -> Result<Decided> {
    let names: Vec<String> = wanted.iter().map(|w| w.interface.clone()).collect();
    let (rows, applied) = store.lanes_recorded(&names).await?;
    let mut generations: HashMap<String, Vec<Generation>> = HashMap::new();
    for row in rows {
        generations.entry(row.interface.clone()).or_default().push(Generation::of(&row));
    }
    let applied: HashMap<String, i32> = applied.into_iter().collect();
    let adopt = to_adopt(wanted, &generations, &applied);
    let highest: HashMap<String, i32> = if adopt.is_empty() {
        HashMap::new()
    } else {
        store.highest_waiting_lanes(&adopt).await?.into_iter().collect()
    };
    Ok(decide(wanted, &generations, &applied, &highest))
}

/// At start, after the claim (a takeover: before it receives): each interface's lanes, a
/// generation added where `ordering` changed, written; told (log and audit) beside the start, which
/// doesn't need it (1,001 changes' audit rows written one by one took ~3.7 s).
pub async fn start(store: &Store, wanted: &[Wanted]) -> Result<HashMap<String, Layout>> {
    let decided = read(store, wanted).await?;
    store.write_lanes(&decided.writes).await?;
    if !decided.changes.is_empty() {
        tokio::spawn(tell(store.clone(), decided.changes));
    }
    Ok(decided.layouts)
}

/// Log and audit the lane changes a start made, with how many messages wait.
async fn tell(store: Store, changes: Vec<Change>) {
    let names: Vec<String> = changes.iter().map(|c| c.interface.clone()).collect();
    let waiting: HashMap<String, i64> = match store.waiting_by_interface(&names).await {
        Ok(w) => w.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "couldn't count what waits in the old lanes");
            HashMap::new()
        }
    };
    for c in &changes {
        let waiting = waiting.get(&c.interface).copied().unwrap_or(0);
        tracing::info!(
            interface = %c.interface,
            before = %c.before.map_or_else(|| "not recorded".to_string(), |b| b.to_string()),
            now = c.now,
            key_changed = c.key_changed,
            waiting,
            "lanes changed: new messages go to the new lanes; the {waiting} waiting in the old ones are sent first, and a key's new messages wait behind its older ones"
        );
        let audit = Audit::new(By::engine(), "interface.lanes_changed").interface(&c.interface).detail(json!({
            "before": c.before, "now": c.now, "key_changed": c.key_changed, "adopted": c.adopted,
            "waiting": waiting, "first_lane": c.first_lane,
        }));
        if let Err(e) = store.record(&audit).await {
            tracing::error!(error = %format!("{e:#}"), interface = %c.interface, "could not write the audit log (lanes changed)");
        }
    }
}

/// A handover failed while this engine still had its interfaces (`interfaces`: each with its
/// lanes): take back the lanes the new engine added where it stored nothing (`Store::
/// take_back_lanes`). Returns what to tell whoever asked about those it did store messages in
/// (they wait for the next start or handover); none when there are none.
pub async fn take_back(store: &Store, interfaces: &[(String, &Lanes)]) -> Option<String> {
    let ends: Vec<(String, i32)> = interfaces.iter().map(|(n, l)| (n.clone(), l.layout.newest().end())).collect();
    if ends.is_empty() {
        return None;
    }
    match store.take_back_lanes(&ends).await {
        Ok(kept) if kept.is_empty() => None,
        Ok(kept) => {
            let lines: Vec<String> = kept
                .iter()
                .map(|(i, n)| format!("  {i}: {n} message{} stored by the new engine", if *n == 1 { "" } else { "s" }))
                .collect();
            Some(format!(
                "the new engine stored messages in lanes this one doesn't read (`ordering` changed):\n{}\nThey wait until the next start or handover, which sends them (docs: nuances, ordering).",
                lines.join("\n")
            ))
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "couldn't take back the lanes the failed handover added");
            None
        }
    }
}

/// Once `lanes` settles (every old lane drained for every destination), delete its old
/// generations and tell it (`interface.lanes_settled`). Runs only for an interface whose lanes are
/// changing, until it settles or the engine stops.
pub fn retire_when_settled(
    store: Store,
    interface: String,
    lanes: Arc<Lanes>,
    stop: tokio_util::sync::CancellationToken,
) {
    if !lanes.is_changing() {
        return;
    }
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        tokio::select! {
            _ = lanes.settled() => {}
            _ = stop.cancelled() => return,
        }
        let newest = lanes.layout.newest().clone();
        match store.retire_lanes(&interface, newest.first).await {
            Ok(_) => {
                let took = started.elapsed();
                tracing::info!(%interface, lanes = newest.lanes, ?took, "lanes settled: nothing waits in the old lanes any more");
                let audit = Audit::new(By::engine(), "interface.lanes_settled").interface(&interface).detail(json!({
                    "lanes": newest.lanes, "first_lane": newest.first, "seconds": took.as_secs_f64(),
                }));
                if let Err(e) = store.record(&audit).await {
                    tracing::error!(error = %format!("{e:#}"), %interface, "could not write the audit log (lanes settled)");
                }
            }
            // Harmless: the next start finds them empty and settles them then.
            Err(e) => tracing::warn!(error = %format!("{e:#}"), %interface, "could not delete the old lanes"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(first: i32, lanes: i32) -> Generation {
        Generation { first, lanes, key: Some("PID-3.1".into()), mixed: false }
    }

    fn layout(gens: &[(i32, i32)]) -> Layout {
        Layout { generations: gens.iter().map(|&(f, n)| generation(f, n)).collect() }
    }

    #[test]
    fn the_store_and_intake_hash_keys_alike() {
        for key in ["", "MRN1", "12345^^^HOSP^MR", "ü"] {
            for lanes in [1, 7, 32, 64] {
                assert_eq!(
                    ductus_store::lane_of(key, 100, lanes),
                    100 + partition_of(key, lanes as u32),
                    "{key} {lanes}"
                );
            }
        }
    }

    #[test]
    fn new_lanes_wait_only_for_old_lanes_sharing_their_keys() {
        // 64 → 32: new lane j (64 + j) shares keys with old lanes j and j + 32.
        let l = layout(&[(0, 64), (64, 32)]);
        assert_eq!(l.peers(64 + 5), vec![5, 37]);
        assert!(l.peers(5).is_empty(), "the oldest lanes wait for nothing");
        // 8 → 64: new lane j shares keys with old lane j % 8.
        let l = layout(&[(0, 8), (8, 64)]);
        assert_eq!(l.peers(8 + 13), vec![5]);
        // To one lane (the key removed): everything.
        let l = Layout {
            generations: vec![
                generation(0, 4),
                Generation { first: 4, lanes: 1, key: Some(String::new()), mixed: false },
            ],
        };
        assert_eq!(l.peers(4), vec![0, 1, 2, 3]);
        // A changed key shares no hashing: every old lane.
        let l = Layout {
            generations: vec![
                generation(0, 4),
                Generation { first: 4, lanes: 4, key: Some("PID-18".into()), mixed: false },
            ],
        };
        assert_eq!(l.peers(5), vec![0, 1, 2, 3]);
        // A key not recorded counts as the same; an adopted generation as unrelated.
        let l =
            Layout { generations: vec![Generation { first: 0, lanes: 4, key: None, mixed: false }, generation(4, 2)] };
        assert_eq!(l.peers(5), vec![1, 3]);
        let l =
            Layout { generations: vec![Generation { first: 0, lanes: 3, key: None, mixed: true }, generation(3, 2)] };
        assert_eq!(l.peers(3), vec![0, 1, 2]);
        // Every older generation counts, not only the one before.
        let l = layout(&[(0, 4), (4, 2), (6, 4)]);
        assert_eq!(l.peers(6 + 3), vec![3, 5]);
    }

    #[test]
    fn lanes_are_numbered_after_the_last_generation_and_told_as_configured() {
        let l = layout(&[(0, 64), (64, 32)]);
        assert_eq!(l.lane_of(""), 64 + partition_of("", 32));
        assert_eq!(l.lanes_of_slot(1, 4), (0..96).filter(|l| l % 4 == 1).collect::<Vec<_>>());
        assert_eq!(l.describe(40).as_deref(), Some("old lane 40 of 64"));
        assert_eq!(l.describe(72).as_deref(), Some("lane 8 of 32"));
        assert_eq!(l.describe(96), None);
    }

    #[test]
    fn a_held_lane_opens_when_the_old_lanes_sharing_its_keys_drain() {
        // 4 → 2 lanes, 2 workers, 2 destinations. New lane 4 + j shares keys with old j and j + 2.
        let lanes = Lanes::new(layout(&[(0, 4), (4, 2)]), 2, 2);
        // Slot 0 has old lanes 0, 2 and new lane 4 (its peers 0 and 2, both its own and read):
        // nothing held. Slot 1: old 1, 3 and new 5 (peers 1, 3: its own): nothing held either.
        assert_eq!(lanes.readable(0, 0), Readable { lanes: vec![0, 2, 4], held: vec![] });
        assert_eq!(lanes.readable(1, 0), Readable { lanes: vec![1, 3, 5], held: vec![] });

        // 4 → 3 lanes, 2 workers: gcd 1, every new lane waits for every old lane.
        let lanes = Lanes::new(layout(&[(0, 4), (4, 3)]), 2, 2);
        // Slot 0: old 0, 2; new 4, 6 (held: old 1 and 3 are slot 1's).
        assert_eq!(lanes.readable(0, 0), Readable { lanes: vec![0, 2], held: vec![4, 6] });
        let rx = lanes.watch(0).unwrap();
        // Slot 1's read for destination 0 came back short with something in lane 3 only.
        assert!(lanes.read_short(0, &[1, 3], &HashSet::from([3])));
        assert!(rx.has_changed().unwrap());
        assert!(lanes.drained(0, 1) && !lanes.drained(0, 3));
        assert_eq!(lanes.readable(0, 0).held, vec![4, 6], "lane 3 still holds them");
        // Slot 1 empties lane 3; slot 0's own old lanes don't hold its new ones while it reads them.
        assert!(lanes.read_short(0, &[3, 5], &HashSet::new()));
        assert_eq!(lanes.readable(0, 0), Readable { lanes: vec![0, 2, 4, 6], held: vec![] });
        // Only for that destination.
        assert_eq!(lanes.readable(0, 1).held, vec![4, 6]);
        assert_eq!(lanes.holding(1), vec![(4, vec![1, 3]), (5, vec![0, 2]), (6, vec![1, 3])]);
        // A short read never marks a new lane; drained old lanes are no longer read.
        assert!(!lanes.read_short(0, &[5], &HashSet::new()));
        assert!(lanes.read_short(0, &[0, 2, 4, 6], &HashSet::from([4])));
        assert_eq!(lanes.readable(0, 0), Readable { lanes: vec![4, 6], held: vec![] });
    }

    #[tokio::test]
    async fn settled_once_every_old_lane_drained_for_every_destination() {
        let lanes = Lanes::new(layout(&[(0, 2), (2, 1)]), 1, 2);
        assert!(lanes.is_changing());
        lanes.nothing_waiting(0, 0);
        let settled = tokio::time::timeout(std::time::Duration::from_millis(20), lanes.settled());
        assert!(settled.await.is_err(), "destination 1 still has old lanes");
        lanes.nothing_waiting(0, 1);
        tokio::time::timeout(std::time::Duration::from_secs(1), lanes.settled()).await.unwrap();
        // Unchanged lanes keep no state.
        let same = Lanes::new(Layout::single(64, "PID-3.1"), 4, 2);
        assert!(!same.is_changing() && same.watch(0).is_none());
        assert_eq!(same.readable(3, 1).lanes, (0..64).filter(|l| l % 4 == 3).collect::<Vec<_>>());
    }

    fn wanted(name: &str, lanes: i32, key: &str) -> Wanted {
        Wanted { interface: name.into(), lanes, key: key.into() }
    }

    #[test]
    fn a_start_adds_a_generation_where_ordering_changed() {
        let gens: HashMap<String, Vec<Generation>> = [
            ("same".to_string(), vec![generation(0, 64)]),
            ("lower".to_string(), vec![generation(0, 64)]),
            ("rekeyed".to_string(), vec![generation(0, 8)]),
            ("unknown-key".to_string(), vec![Generation { first: 0, lanes: 8, key: None, mixed: false }]),
            ("changing".to_string(), vec![generation(0, 64), generation(64, 32)]),
        ]
        .into();
        let applied: HashMap<String, i32> =
            [("same", 64), ("lower", 64), ("rekeyed", 8), ("unknown-key", 8), ("changing", 96)]
                .map(|(n, l)| (n.to_string(), l))
                .into();
        let w = [
            wanted("same", 64, "PID-3.1"),
            wanted("lower", 32, "PID-3.1"),
            wanted("rekeyed", 8, "PID-18"),
            wanted("unknown-key", 8, "PID-3.1"),
            wanted("changing", 1, ""),
        ];
        assert!(to_adopt(&w, &gens, &applied).is_empty());
        let d = decide(&w, &gens, &applied, &HashMap::new());
        assert_eq!(d.layouts["same"], layout(&[(0, 64)]));
        assert_eq!(d.layouts["lower"], layout(&[(0, 64), (64, 32)]));
        assert_eq!(d.layouts["changing"].generations.len(), 3);
        assert_eq!(
            d.layouts["changing"].newest(),
            &Generation { first: 96, lanes: 1, key: Some(String::new()), mixed: false }
        );
        let changed: Vec<(&str, Option<i32>, i32, bool)> =
            d.changes.iter().map(|c| (c.interface.as_str(), c.before, c.now, c.key_changed)).collect();
        assert_eq!(
            changed,
            [("lower", Some(64), 32, false), ("rekeyed", Some(8), 8, true), ("changing", Some(32), 1, true)]
        );
        // Written: the new generations, the unknown key, the new ends.
        let added: Vec<(&str, i32, i32)> =
            d.writes.added.iter().map(|g| (g.interface.as_str(), g.first_lane, g.lanes)).collect();
        assert_eq!(added, [("lower", 64, 32), ("rekeyed", 8, 8), ("changing", 96, 1)]);
        assert_eq!(d.writes.keys, [("unknown-key".to_string(), 0, "PID-3.1".to_string())]);
        assert_eq!(
            d.writes.applied,
            [("lower".to_string(), 96), ("rekeyed".to_string(), 16), ("changing".to_string(), 97)]
        );
        assert!(d.writes.replaced.is_empty());
    }

    #[test]
    fn lanes_an_older_binary_ran_or_nothing_recorded_are_adopted() {
        // `old` was run by an older binary since (it wrote 16 where this one wrote 96); `new` was
        // never run; `fresh` neither, with nothing waiting.
        let gens: HashMap<String, Vec<Generation>> =
            [("old".to_string(), vec![generation(0, 64), generation(64, 32)])].into();
        let applied: HashMap<String, i32> = [("old".to_string(), 16)].into();
        let w = [wanted("old", 16, "PID-3.1"), wanted("new", 8, "PID-3.1"), wanted("fresh", 4, "PID-3.1")];
        assert_eq!(to_adopt(&w, &gens, &applied), ["old", "new", "fresh"]);
        let highest: HashMap<String, i32> = [("old".to_string(), 70), ("new".to_string(), 3)].into();
        let d = decide(&w, &gens, &applied, &highest);
        let mixed = |lanes| Generation { first: 0, lanes, key: None, mixed: true };
        assert_eq!(d.layouts["old"].generations, vec![mixed(71), Generation { first: 71, ..generation(0, 16) }]);
        assert_eq!(d.layouts["new"].generations, vec![mixed(4), Generation { first: 4, ..generation(0, 8) }]);
        assert_eq!(d.layouts["fresh"], layout(&[(0, 4)]));
        assert_eq!(d.writes.replaced, ["old", "new", "fresh"]);
        let changed: Vec<(&str, Option<i32>, bool)> =
            d.changes.iter().map(|c| (c.interface.as_str(), c.before, c.adopted)).collect();
        assert_eq!(
            changed,
            [("old", Some(16), true), ("new", None, true)],
            "a new interface with nothing waiting changes nothing"
        );
    }
}
