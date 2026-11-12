//! Hotspots: where a destination's backlog sits, lane by lane, and what's at the head of each.
//!
//! Messages wait in lanes (the ordering key's hash, `ordering.partitions` of them), and each
//! lane belongs to one worker per destination, which handles its lanes' messages one at a time
//! in arrival order. So a message that keeps failing holds up **every lane of its worker**, not
//! just its own; and a key (or a few) arriving faster than they're sent piles up in one lane on
//! one worker while the others sit idle. This answers "which message, which key, which worker"
//! from the lanes' counts and heads. Pure: the facts come from the store.

use crate::monitor::{count, first_line, human};
use chrono::{DateTime, Utc};

/// The first message waiting in a lane (the next one its worker sends).
#[derive(Debug, Clone, PartialEq)]
pub struct Head {
    pub id: i64,
    /// Its ordering key's value (e.g. the MRN), if the interface has one.
    pub key: Option<String>,
    pub received_at: DateTime<Utc>,
    /// Failed attempts so far: above zero, its worker is retrying it.
    pub attempts: i32,
    pub last_error: Option<String>,
    /// When it was last tried (only when it failed before).
    pub last_attempt: Option<DateTime<Utc>>,
}

/// One lane with messages waiting.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub lane: i32,
    pub waiting: u64,
    pub head: Head,
}

/// How an interface's lanes are numbered and spread over its workers (the runtime's
/// `lanes::Layout`, and `InterfaceConfig::slots`).
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Its lane generations, oldest first, as `(first lane, lanes)`: one, unless `ordering`
    /// changed and the old lanes still hold messages. The last is `ordering` as configured.
    pub generations: Vec<(i32, u32)>,
    pub workers: u32,
}

impl Layout {
    /// One generation of `lanes` lanes from 0.
    pub fn new(lanes: u32, workers: u32) -> Self {
        Self { generations: vec![(0, lanes)], workers }
    }

    /// The lanes configured (the newest generation's).
    pub fn lanes(&self) -> u32 {
        self.generations.last().map_or(1, |g| g.1)
    }

    fn generation(&self, lane: i32) -> Option<usize> {
        self.generations
            .iter()
            .position(|&(first, n)| lane >= first && i64::from(lane) < i64::from(first) + i64::from(n))
    }

    /// In a generation older than the newest (its messages go first).
    pub fn is_old(&self, lane: i32) -> bool {
        self.generation(lane).is_some_and(|g| g + 1 < self.generations.len())
    }

    /// The worker that owns a lane (lane `l` is worker `l % workers`'s), if any: a lane in no
    /// generation listed was made under another `ordering`, and this engine doesn't read it.
    pub fn worker_of(&self, lane: i32) -> Option<u32> {
        self.generation(lane)?;
        Some(lane as u32 % self.workers.max(1))
    }

    /// A lane as people configured it: "lane 8" (of the lanes configured) or "old lane 40" (of
    /// an earlier `ordering`), numbered from 0 in its generation.
    pub fn name(&self, lane: i32) -> String {
        match self.generation(lane) {
            Some(g) if g + 1 < self.generations.len() => format!("old lane {}", lane - self.generations[g].0),
            Some(g) => format!("lane {}", lane - self.generations[g].0),
            None => format!("lane {lane}"),
        }
    }
}

/// One worker's share of the backlog.
#[derive(Debug, Clone, PartialEq)]
pub struct Worker {
    pub worker: u32,
    pub lanes_waiting: usize,
    pub waiting: u64,
    /// The lane whose head it keeps retrying, if it does.
    pub blocked_by: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A message keeps failing at the head of `lane`: everything in `worker`'s lanes waits.
    Blocked { worker: u32, lane: i32, behind: u64 },
    /// Every worker with a backlog is retrying: more likely the receiver (or something the code
    /// calls) than one message.
    AllBlocked,
    /// One worker holds most of the backlog while the others have little.
    Uneven { worker: u32, share: f64 },
    /// One lane holds most of the backlog: a key (or a few) arriving faster than it's sent.
    HotLane { lane: i32, share: f64 },
    /// Messages in a lane this engine doesn't read: in no generation it listed (stored under
    /// another `ordering`, by an older version or a handover that failed).
    Unowned { lane: i32, waiting: u64 },
    /// `ordering` changed: `held` new lanes with messages wait for old lanes sharing their keys,
    /// which hold `old` messages (sent first).
    Changing { held: usize, old: u64 },
}

/// Something worth knowing about the backlog, and how to say it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    pub text: String,
}

/// A destination's backlog by lane and worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Hotspots {
    pub waiting: u64,
    pub lanes_waiting: usize,
    /// Workers with something waiting, first to last.
    pub workers: Vec<Worker>,
    pub findings: Vec<Finding>,
    /// In operations' words: "1,240 waiting in 3 of 64 lanes; worker 2 blocked by ...".
    pub summary: String,
}

/// A worker or lane is only called hot with at least this many waiting: below it, a queue that
/// flows always has a few messages somewhere.
pub const MIN_HOT: u64 = 100;
/// A lane is hot with at least this share of the backlog.
const HOT_SHARE: f64 = 0.5;

/// Where a destination's backlog sits. `lanes` are the lanes with messages waiting; `held`, while
/// `ordering` changes, the lanes held behind old ones and which old lanes hold each; a paused
/// destination retries nothing, so nothing counts as blocked. `last_sent`: when the destination
/// last delivered anything (a send after a failing message arrived means the receiver is up).
pub fn analyze(
    lanes: &[Lane],
    layout: &Layout,
    held: &[(i32, Vec<i32>)],
    paused: bool,
    last_sent: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Hotspots {
    let waiting: u64 = lanes.iter().map(|l| l.waiting).sum();
    let mut workers: Vec<Worker> = Vec::new();
    for lane in lanes {
        let Some(w) = layout.worker_of(lane.lane) else { continue };
        let at = match workers.iter().position(|x| x.worker == w) {
            Some(at) => at,
            None => {
                workers.push(Worker { worker: w, lanes_waiting: 0, waiting: 0, blocked_by: None });
                workers.len() - 1
            }
        };
        let worker = &mut workers[at];
        worker.lanes_waiting += 1;
        worker.waiting += lane.waiting;
        // A worker retries one message at a time; if two heads have failed attempts (a restart
        // with other settings), the older is the one it's on.
        if !paused && lane.head.attempts > 0 {
            let older = worker.blocked_by.and_then(|b| lanes.iter().find(|l| l.lane == b)).map(|l| l.head.id);
            if older.is_none_or(|id| lane.head.id < id) {
                worker.blocked_by = Some(lane.lane);
            }
        }
    }
    workers.sort_by_key(|w| w.worker);

    let mut findings = Vec::new();
    let lane = |n: i32| lanes.iter().find(|l| l.lane == n);
    let blocked: Vec<&Worker> = workers.iter().filter(|w| w.blocked_by.is_some()).collect();
    let all_blocked = workers.len() > 1 && blocked.len() == workers.len();
    if all_blocked {
        let error = blocked
            .iter()
            .filter_map(|w| lane(w.blocked_by?))
            .max_by_key(|l| l.head.last_attempt)
            .and_then(|l| l.head.last_error.as_deref())
            .map(|e| format!(" (last error: {})", first_line(e)))
            .unwrap_or_default();
        findings.push(Finding {
            kind: Kind::AllBlocked,
            text: format!(
                "every worker is retrying its first message: the receiver, or something the code calls, looks down{error}"
            ),
        });
    }
    for w in &blocked {
        let Some(l) = w.blocked_by.and_then(lane) else { continue };
        let head = &l.head;
        let key = head.key.as_deref().map(|k| format!("key {k}, ")).unwrap_or_default();
        let error = head.last_error.as_deref().map(|e| format!(": {}", first_line(e))).unwrap_or_default();
        let age = human((now - head.received_at).to_std().unwrap_or_default());
        let tries = format!("{} attempt{}", head.attempts, if head.attempts == 1 { "" } else { "s" });
        let what = format!("message {} ({key}{}, {tries}, waiting {age}){error}", head.id, layout.name(l.lane));
        let behind = w.waiting.saturating_sub(1);
        let mut text = match (layout.workers, all_blocked) {
            (1, _) => format!("blocked by {what}; all {} behind it wait (one worker: order)", count(behind)),
            // The error is said once, in the finding before.
            (_, true) => format!("worker {} on message {} ({key}{}, {tries})", w.worker, head.id, layout.name(l.lane)),
            _ => format!(
                "worker {} blocked by {what}; {} wait behind it in its {}",
                w.worker,
                count(behind),
                match w.lanes_waiting {
                    1 => "lane".to_string(),
                    n => format!("{n} lanes"),
                }
            ),
        };
        // The receiver took something after this message arrived: it's up, so the trouble is
        // this message (or what the code does with it), not the receiver.
        let others_sent = last_sent.filter(|s| *s > head.received_at);
        let others_flow = workers.iter().any(|o| o.worker != w.worker && o.blocked_by.is_none());
        if !all_blocked {
            match others_sent {
                Some(s) => {
                    text += &format!(
                        "; other messages were sent since it arrived (last {} ago): look at this message",
                        human((now - s).to_std().unwrap_or_default())
                    )
                }
                None if others_flow => text += "; the other workers are still sending: look at this message",
                None => {}
            }
        }
        findings.push(Finding { kind: Kind::Blocked { worker: w.worker, lane: l.lane, behind }, text });
    }

    // Hot workers and lanes, unless a failing message explains the pile.
    if layout.workers > 1 && waiting >= MIN_HOT {
        let threshold = HOT_SHARE.max(1.5 / f64::from(layout.workers));
        for w in workers.iter().filter(|w| w.blocked_by.is_none()) {
            let share = w.waiting as f64 / waiting as f64;
            if share >= threshold {
                findings.push(Finding {
                    kind: Kind::Uneven { worker: w.worker, share },
                    text: format!(
                        "worker {} has {}% of the backlog ({} of {} workers busy)",
                        w.worker,
                        (share * 100.0).round(),
                        workers.len(),
                        layout.workers
                    ),
                });
            }
        }
    }
    if layout.lanes() > 1 && waiting >= MIN_HOT {
        let hottest = lanes.iter().max_by_key(|l| (l.waiting, std::cmp::Reverse(l.lane)));
        if let Some(l) = hottest
            && let share = l.waiting as f64 / waiting as f64
            && share >= HOT_SHARE
            && !workers.iter().any(|w| w.blocked_by.is_some() && layout.worker_of(l.lane) == Some(w.worker))
        {
            let key = l.head.key.as_deref().map(|k| format!(", key {k} at its head")).unwrap_or_default();
            findings.push(Finding {
                kind: Kind::HotLane { lane: l.lane, share },
                text: format!(
                    "{} has {}% of the backlog ({}{key}): one key or a few arrive faster than they're sent",
                    layout.name(l.lane),
                    (share * 100.0).round(),
                    count(l.waiting)
                ),
            });
        }
    }
    for l in lanes.iter().filter(|l| layout.worker_of(l.lane).is_none()) {
        findings.push(Finding {
            kind: Kind::Unowned { lane: l.lane, waiting: l.waiting },
            text: format!(
                "lane {}: {} waiting that this engine doesn't read (stored under another `ordering`: by an older version, or by a handover that failed): the next start or handover takes them in",
                l.lane,
                count(l.waiting)
            ),
        });
    }
    let old: u64 = lanes.iter().filter(|l| layout.is_old(l.lane)).map(|l| l.waiting).sum();
    let waiting_held: Vec<&(i32, Vec<i32>)> =
        held.iter().filter(|(lane, _)| lanes.iter().any(|l| l.lane == *lane)).collect();
    if !waiting_held.is_empty() {
        let (lane, by) = waiting_held[0];
        let by: Vec<String> = by.iter().map(|&b| layout.name(b).trim_start_matches("old lane ").to_string()).collect();
        let more = match waiting_held.len() {
            1 => String::new(),
            n => format!(" and {} more", n - 1),
        };
        findings.push(Finding {
            kind: Kind::Changing { held: waiting_held.len(), old },
            text: format!(
                "`ordering` changed: the {} waiting in old lanes go first, and new lanes wait behind the old ones sharing their keys ({} waits for old lane{} {}{more})",
                count(old),
                layout.name(*lane),
                if by.len() == 1 { "" } else { "s" },
                by.join(", ")
            ),
        });
    }

    let new_lanes = lanes.iter().filter(|l| !layout.is_old(l.lane)).count();
    let old_lanes = lanes.len() - new_lanes;
    let old_where = format!("{} old lane{}", old_lanes, if old_lanes == 1 { "" } else { "s" });
    let mut parts = vec![match (waiting, old) {
        (0, _) => "nothing waiting".to_string(),
        (_, 0) => format!("{} waiting in {} of {} lanes", count(waiting), lanes.len(), layout.lanes()),
        (w, o) if w == o => format!("{} waiting, all in {old_where}", count(w)),
        (w, o) => format!(
            "{} waiting in {} of {} lanes and {} in {old_where}",
            count(w - o),
            new_lanes,
            layout.lanes(),
            count(o)
        ),
    }];
    if paused && waiting > 0 {
        parts[0] += " (paused)";
    }
    // With every worker blocked, the summary says so once; each worker's message is in the
    // findings (and the lane list).
    let said = |f: &&Finding| !(all_blocked && matches!(f.kind, Kind::Blocked { .. }));
    parts.extend(findings.iter().filter(said).map(|f| f.text.clone()));
    Hotspots { waiting, lanes_waiting: lanes.len(), workers, findings, summary: parts.join("; ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        "2026-10-09T12:00:00Z".parse().unwrap()
    }

    fn lane(lane: i32, waiting: u64, id: i64) -> Lane {
        Lane {
            lane,
            waiting,
            head: Head {
                id,
                key: Some(format!("MRN{lane}")),
                received_at: now() - TimeDelta::minutes(90),
                attempts: 0,
                last_error: None,
                last_attempt: None,
            },
        }
    }

    fn failing(mut l: Lane, attempts: i32, error: &str) -> Lane {
        l.head.attempts = attempts;
        l.head.last_error = Some(error.into());
        l.head.last_attempt = Some(now() - TimeDelta::seconds(30));
        l
    }

    fn kinds(h: &Hotspots) -> Vec<&Kind> {
        h.findings.iter().map(|f| &f.kind).collect()
    }

    #[test]
    fn lanes_belong_to_workers_round_robin_and_lanes_past_the_count_to_nobody() {
        let layout = Layout::new(8, 2);
        assert_eq!((layout.worker_of(0), layout.worker_of(3), layout.worker_of(6)), (Some(0), Some(1), Some(0)));
        assert_eq!((layout.worker_of(8), layout.worker_of(-1)), (None, None));
        assert_eq!(Layout::new(1, 4).worker_of(0), Some(0));
    }

    #[test]
    fn an_even_flowing_backlog_has_no_findings() {
        let lanes: Vec<Lane> = (0..8).map(|n| lane(n, 30, 100 + i64::from(n))).collect();
        let h = analyze(&lanes, &Layout::new(64, 4), &[], false, None, now());
        assert_eq!((h.waiting, h.lanes_waiting), (240, 8));
        assert!(h.findings.is_empty(), "{h:?}");
        assert_eq!(h.summary, "240 waiting in 8 of 64 lanes");
        assert_eq!(
            h.workers.iter().map(|w| (w.worker, w.waiting)).collect::<Vec<_>>(),
            [(0, 60), (1, 60), (2, 60), (3, 60)]
        );
    }

    #[test]
    fn one_failing_message_blocks_its_workers_lanes_and_the_others_still_send() {
        // Worker 1 owns lanes 1 and 5; lane 5's head keeps failing.
        let lanes = vec![lane(0, 3, 50), failing(lane(5, 400, 60), 14, "HTTP 503: busy\nstack"), lane(1, 300, 70)];
        let h = analyze(&lanes, &Layout::new(8, 4), &[], false, None, now());
        assert_eq!(kinds(&h), [&Kind::Blocked { worker: 1, lane: 5, behind: 699 }]);
        assert_eq!(
            h.findings[0].text,
            "worker 1 blocked by message 60 (key MRN5, lane 5, 14 attempts, waiting 1 h 30 min): HTTP 503: busy; \
             699 wait behind it in its 2 lanes; the other workers are still sending: look at this message"
        );
        // The pile on worker 1 is explained by the failure: not reported as uneven or hot.
        assert!(h.summary.starts_with("703 waiting in 3 of 8 lanes; worker 1 blocked"), "{}", h.summary);
    }

    #[test]
    fn every_worker_failing_points_at_the_receiver() {
        let lanes =
            vec![failing(lane(0, 10, 1), 3, "connection refused"), failing(lane(1, 10, 2), 2, "connection refused")];
        let h = analyze(&lanes, &Layout::new(2, 2), &[], false, None, now());
        assert_eq!(kinds(&h)[0], &Kind::AllBlocked);
        assert!(h.findings[0].text.contains("looks down (last error: connection refused)"), "{}", h.findings[0].text);
        assert_eq!(h.findings.len(), 3); // and each worker's message
        assert_eq!(h.findings[1].text, "worker 0 on message 1 (key MRN0, lane 0, 3 attempts)");
        assert!(!h.summary.contains("worker 0 on"), "said once: {}", h.summary);
    }

    #[test]
    fn with_one_worker_a_failing_message_holds_up_everything() {
        let lanes = vec![failing(lane(3, 50, 9), 1, "timeout"), lane(4, 70, 12)];
        let h = analyze(&lanes, &Layout::new(64, 1), &[], false, None, now());
        assert_eq!(kinds(&h), [&Kind::Blocked { worker: 0, lane: 3, behind: 119 }]);
        assert!(h.findings[0].text.starts_with("blocked by message 9 (key MRN3, lane 3, 1 attempt,"));
        assert!(h.findings[0].text.ends_with("all 119 behind it wait (one worker: order)"));
        // The receiver took other messages after this one arrived: it's this message.
        let sent = now() - TimeDelta::seconds(20);
        let h = analyze(&lanes, &Layout::new(64, 1), &[], false, Some(sent), now());
        assert!(
            h.findings[0].text.ends_with(
                "all 119 behind it wait (one worker: order); other messages were sent since it arrived (last 20s ago): look at this message"
            ),
            "{}",
            h.findings[0].text
        );
        // A send from before it arrived says nothing.
        let h = analyze(&lanes, &Layout::new(64, 1), &[], false, Some(now() - TimeDelta::hours(2)), now());
        assert!(h.findings[0].text.ends_with("(one worker: order)"));
    }

    #[test]
    fn a_blocked_worker_alone_with_a_backlog_names_no_culprit_without_evidence() {
        let lanes = vec![failing(lane(5, 1, 60), 2, "HTTP 503")];
        let h = analyze(&lanes, &Layout::new(8, 4), &[], false, None, now());
        assert_eq!(
            h.findings[0].text,
            "worker 1 blocked by message 60 (key MRN5, lane 5, 2 attempts, waiting 1 h 30 min): HTTP 503; 0 wait behind it in its lane"
        );
    }

    #[test]
    fn a_paused_destination_is_not_blocked() {
        let lanes = vec![failing(lane(0, 10, 1), 3, "connection refused")];
        let h = analyze(&lanes, &Layout::new(1, 1), &[], true, None, now());
        assert!(h.findings.is_empty());
        assert_eq!(h.summary, "10 waiting in 1 of 1 lanes (paused)");
    }

    #[test]
    fn a_hot_key_shows_as_a_hot_lane_and_an_uneven_worker() {
        let mut lanes = vec![lane(17, 900, 5)];
        lanes.extend((0..8).filter(|n| *n != 1).map(|n| lane(n, 10, 100 + i64::from(n))));
        let h = analyze(&lanes, &Layout::new(64, 4), &[], false, None, now());
        assert_eq!(h.waiting, 970);
        assert!(matches!(kinds(&h)[..], [Kind::Uneven { worker: 1, .. }, Kind::HotLane { lane: 17, .. }]), "{h:?}");
        assert_eq!(h.findings[0].text, "worker 1 has 94% of the backlog (4 of 4 workers busy)");
        assert_eq!(
            h.findings[1].text,
            "lane 17 has 93% of the backlog (900, key MRN17 at its head): one key or a few arrive faster than they're sent"
        );
    }

    #[test]
    fn small_backlogs_and_single_lanes_are_never_hot() {
        let h = analyze(&[lane(17, 90, 5), lane(2, 5, 6)], &Layout::new(64, 4), &[], false, None, now());
        assert!(h.findings.is_empty(), "{h:?}");
        // No ordering key: one lane, one worker; a big backlog there is just a backlog.
        let h = analyze(&[lane(0, 5000, 5)], &Layout::new(1, 1), &[], false, None, now());
        assert!(h.findings.is_empty(), "{h:?}");
    }

    #[test]
    fn lanes_nobody_reads_are_reported() {
        let h = analyze(&[lane(2, 5, 6), lane(70, 30, 1)], &Layout::new(64, 4), &[], false, None, now());
        assert_eq!(kinds(&h), [&Kind::Unowned { lane: 70, waiting: 30 }]);
        assert!(h.findings[0].text.contains("stored under another `ordering`"), "{}", h.findings[0].text);
        assert_eq!(h.workers.len(), 1);
    }

    #[test]
    fn while_ordering_changes_old_lanes_are_named_and_held_lanes_explained() {
        // 8 → 4 lanes, 2 workers: old lanes 0–7, new lanes 8–11 (lane 8 + j waits for old j, j + 4).
        let layout = Layout { generations: vec![(0, 8), (8, 4)], workers: 2 };
        assert_eq!((layout.lanes(), layout.name(5), layout.name(9)), (4, "old lane 5".into(), "lane 1".into()));
        assert_eq!((layout.worker_of(5), layout.worker_of(9), layout.worker_of(12)), (Some(1), Some(1), None));
        let lanes = vec![lane(5, 40, 10), failing(lane(1, 2, 11), 3, "HTTP 503"), lane(9, 7, 900)];
        let held = [(9, vec![1, 5])];
        let h = analyze(&lanes, &layout, &held, false, None, now());
        assert_eq!(kinds(&h).last(), Some(&&Kind::Changing { held: 1, old: 42 }));
        let changing = &h.findings.last().unwrap().text;
        assert_eq!(
            changing,
            "`ordering` changed: the 42 waiting in old lanes go first, and new lanes wait behind the old ones sharing their keys (lane 1 waits for old lanes 1, 5)"
        );
        assert!(h.summary.starts_with("7 waiting in 1 of 4 lanes and 42 in 2 old lanes;"), "{}", h.summary);
        assert!(h.findings[0].text.contains("(key MRN1, old lane 1, 3 attempts"), "{}", h.findings[0].text);
        // Only old lanes hold messages.
        let h = analyze(&[lane(5, 40, 10)], &layout, &[], false, None, now());
        assert_eq!(h.summary, "40 waiting, all in 1 old lane");
    }
}
