//! `monitor:` in interface.yaml: what operations expects of an interface, in the terms its teams
//! already use. Nothing is expected by default: these are operations decisions.
//!
//! ```yaml
//! monitor:
//!   active_within: 30m          # something received in the last 30 minutes...
//!   between: "06:00-22:00"      # ...during these hours (the feed sleeps at night)
//!   active_by: "Mon-Fri 09:00"  # something received by 09:00 on weekdays (or "09:00", or cron)
//!   from: "18:00"               # counted from 18:00 the evening before (default: midnight)
//!   behind: { messages: 500, age: 10m }   # either one trips it; `behind: 500` or `behind: 10m`
//! ```
//!
//! A destination can set its own `behind` (`monitor: { behind: 10m }` on the destination).
//! Times are the server's local time, like schedules.

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration as Span, Local, NaiveTime, TimeZone, Utc};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    /// Something must be received at least this often, e.g. `30m`.
    #[serde(default, deserialize_with = "crate::config::duration_opt")]
    pub active_within: Option<Duration>,
    /// `active_within` applies only between these times: `"06:00-22:00"` (or overnight,
    /// `"22:00-06:00"`). The quiet time starts counting when the hours start, so a feed that
    /// sleeps at night isn't "silent" at 06:01.
    #[serde(default)]
    pub between: Option<Hours>,
    /// Something must be received by then: `"09:00"` (every day), `"Mon 09:00"`, `"Mon-Fri
    /// 09:00"`, or a cron expression. Only receiving counts, not delivering.
    #[serde(default)]
    pub active_by: Option<At>,
    /// With `active_by`: what counts is received after the last `from` before the deadline
    /// (`"18:00"`: the evening before; `"Fri 18:00"`). Default: midnight of the deadline's day,
    /// so yesterday's late file can't hide today's missing one.
    #[serde(default)]
    pub from: Option<At>,
    /// When a destination's queue is behind: by messages waiting, by the age of the oldest
    /// waiting one, or both (either trips it). Destinations can set their own.
    #[serde(default)]
    pub behind: Option<Behind>,
}

/// A destination's own expectations (`monitor:` on the destination).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationMonitor {
    #[serde(default)]
    pub behind: Option<Behind>,
}

impl Monitor {
    pub fn validate(&self) -> Result<()> {
        if self.between.is_some() && self.active_within.is_none() {
            bail!("monitor.between goes with `active_within` (the hours it applies to)");
        }
        if self.from.is_some() && self.active_by.is_none() {
            bail!("monitor.from goes with `active_by` (where its window starts)");
        }
        if self.active_within.is_some_and(|d| d < Duration::from_secs(60)) {
            bail!("monitor.active_within must be at least 1m");
        }
        Ok(())
    }

    /// Nothing is expected.
    pub fn is_empty(&self) -> bool {
        self.active_within.is_none() && self.active_by.is_none() && self.behind.is_none()
    }

    /// With `active_within`: how long the interface has been quiet, if longer than allowed now.
    /// `last` is when something was last received (or when watching began, if never). Outside
    /// `between`, nothing is expected.
    pub fn silent(&self, last: DateTime<Utc>, now: DateTime<Utc>) -> Option<Span> {
        let within = Span::from_std(self.active_within?).ok()?;
        let counted_from = match &self.between {
            None => last,
            Some(hours) => last.max(hours.started(now)?),
        };
        (now - counted_from > within).then(|| now - last)
    }

    /// With `active_by`: the latest deadline up to `now` that passed with nothing received in
    /// its window, as `(window start, deadline)`. Deadlines before `watched_since` (when the
    /// engine first ran the interface) don't count: nobody could have sent to it.
    pub fn missed(
        &self,
        last_received: Option<DateTime<Utc>>,
        watched_since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let deadline = self.active_by.as_ref()?.last_at_or_before(now)?;
        let start = self.window_start(deadline)?;
        if start < watched_since || last_received.is_some_and(|r| r >= start) {
            return None;
        }
        Some((start, deadline))
    }

    /// Where the window that ends at `deadline` starts.
    fn window_start(&self, deadline: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.from {
            Some(from) => from.last_at_or_before(deadline - Span::seconds(1)),
            None => {
                let day = deadline.with_timezone(&Local).date_naive();
                let midnight = Local.from_local_datetime(&day.and_time(NaiveTime::MIN)).earliest()?;
                Some(midnight.with_timezone(&Utc))
            }
        }
    }
}

/// Hours of the day (local): `"06:00-22:00"`, or across midnight, `"22:00-06:00"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hours {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl Hours {
    pub fn parse(text: &str) -> Result<Self> {
        let problem = || anyhow!("hours `{text}`: write them as \"06:00-22:00\"");
        let (start, end) = text.split_once('-').ok_or_else(problem)?;
        let time = |t: &str| NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|_| problem());
        let hours = Self { start: time(start)?, end: time(end)? };
        if hours.start == hours.end {
            bail!("hours `{text}` start and end at the same time (leave `between` out for all day)");
        }
        Ok(hours)
    }

    fn contains(&self, t: NaiveTime) -> bool {
        if self.start < self.end { self.start <= t && t < self.end } else { t >= self.start || t < self.end }
    }

    /// When the current stretch of hours started, if `now` is inside them.
    fn started(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = now.with_timezone(&Local);
        if !self.contains(local.time()) {
            return None;
        }
        let mut day = local.date_naive();
        if local.time() < self.start {
            day = day.pred_opt()?; // overnight hours, after midnight: they started yesterday
        }
        Some(Local.from_local_datetime(&day.and_time(self.start)).earliest()?.with_timezone(&Utc))
    }
}

impl std::fmt::Display for Hours {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.start.format("%H:%M"), self.end.format("%H:%M"))
    }
}

impl<'de> Deserialize<'de> for Hours {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Moments that repeat, in local time: `"09:00"` (every day), `"Mon 09:00"`, `"Mon-Fri
/// 09:00"`, `"Mon,Thu 09:00"`, or cron (`"0 9 * * 1-5"`).
#[derive(Debug, Clone)]
pub struct At {
    /// As written.
    pub text: String,
    cron: Box<croner::Cron>,
}

impl At {
    pub fn parse(text: &str) -> Result<Self> {
        let written = text.trim();
        let parts: Vec<&str> = written.split_whitespace().collect();
        let time = |t: &str| NaiveTime::parse_from_str(t, "%H:%M").ok();
        let expression = match parts.as_slice() {
            [t] => time(t).map(|t| format!("{} {} * * *", t.format("%-M"), t.format("%-H"))),
            [days, t] => time(t).map(|t| format!("{} {} * * {days}", t.format("%-M"), t.format("%-H"))),
            _ => None,
        }
        .unwrap_or_else(|| written.to_string());
        let cron = expression.parse::<croner::Cron>().map_err(|e| {
            anyhow!("`{written}`: {e} (write \"09:00\", \"Mon 09:00\", \"Mon-Fri 09:00\", or a five-field cron)")
        })?;
        Ok(Self { text: written.to_string(), cron: Box::new(cron) })
    }

    /// The latest of these moments at or before `t`.
    pub fn last_at_or_before(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = t.with_timezone(&Local);
        self.cron.find_previous_occurrence(&local, true).ok().map(|at| at.with_timezone(&Utc))
    }
}

impl<'de> Deserialize<'de> for At {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// When a queue counts as behind: `500` (messages waiting), `10m` (the oldest has waited that
/// long), or `{ messages: 500, age: 10m }` (either one). Count alone misses an overnight stall
/// when little arrives; age alone misses a daytime pile-up that still moves.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Behind {
    pub messages: Option<u64>,
    pub age: Option<Duration>,
}

impl Behind {
    /// Which limit a queue of `waiting` messages, the oldest `age` old, is past (the count is
    /// reported first).
    pub fn tripped(&self, waiting: u64, age: Option<Duration>) -> Option<Tripped> {
        if let Some(limit) = self.messages
            && waiting >= limit
            && waiting > 0
        {
            return Some(Tripped::Messages(limit));
        }
        match (self.age, age) {
            (Some(limit), Some(age)) if age >= limit => Some(Tripped::Age(limit)),
            _ => None,
        }
    }
}

/// Which `behind` limit a queue is past.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tripped {
    Messages(u64),
    Age(Duration),
}

impl<'de> Deserialize<'de> for Behind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Both {
            #[serde(default)]
            messages: Option<u64>,
            #[serde(default, deserialize_with = "crate::config::duration_opt")]
            age: Option<Duration>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Messages(u64),
            Text(String),
            Both(Both),
        }
        let behind = match Written::deserialize(d)? {
            Written::Messages(n) => Behind { messages: Some(n), age: None },
            Written::Text(t) => match t.trim().parse::<u64>() {
                Ok(n) => Behind { messages: Some(n), age: None },
                Err(_) => Behind {
                    messages: None,
                    age: Some(crate::config::parse_duration(&t).map_err(serde::de::Error::custom)?),
                },
            },
            Written::Both(b) => Behind { messages: b.messages, age: b.age },
        };
        match behind {
            Behind { messages: None, age: None } => {
                Err(serde::de::Error::custom("`behind` needs `messages` (e.g. 500), `age` (e.g. 10m), or both"))
            }
            Behind { messages: Some(0), .. } => Err(serde::de::Error::custom("`behind` messages must be at least 1")),
            Behind { age: Some(a), .. } if a < Duration::from_secs(1) => {
                Err(serde::de::Error::custom("`behind` age must be at least 1s"))
            }
            b => Ok(b),
        }
    }
}

/// Hours in a week: what "normal" is kept for (Monday 07:00 compares with past Mondays at 07:00).
pub const HOURS_OF_WEEK: usize = 168;

/// The hour of the week of `t` in local time: Monday 00:00-01:00 is 0, Sunday 23:00 is 167.
pub fn hour_of_week(t: DateTime<Utc>) -> usize {
    use chrono::{Datelike, Timelike};
    let local = t.with_timezone(&Local);
    local.weekday().num_days_from_monday() as usize * 24 + local.hour() as usize
}

/// What's normal for a destination in each hour of the week: messages arriving per minute, and
/// how long a send takes. Learned from the hours the engine watched; recent weeks weigh more
/// (each hour moves its value `LEARN` of the way), so a feed that grows or a receiver that got
/// faster becomes the new normal within a few weeks.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    pub inflow: Vec<Option<f32>>,
    pub send_ms: Vec<Option<f32>>,
}

impl Default for Baseline {
    fn default() -> Self {
        Self { inflow: vec![None; HOURS_OF_WEEK], send_ms: vec![None; HOURS_OF_WEEK] }
    }
}

impl Baseline {
    const LEARN: f32 = 0.3;

    /// From stored values (any length that isn't 168 starts over).
    pub fn from_stored(inflow: Option<Vec<Option<f32>>>, send_ms: Option<Vec<Option<f32>>>) -> Self {
        let slots = |v: Option<Vec<Option<f32>>>| v.filter(|v| v.len() == HOURS_OF_WEEK);
        let empty = Self::default();
        Self { inflow: slots(inflow).unwrap_or(empty.inflow), send_ms: slots(send_ms).unwrap_or(empty.send_ms) }
    }

    /// Learn from one hour watched: messages per minute arriving, and the mean send (if any).
    pub fn learn(&mut self, hour: usize, inflow_per_min: f64, send_ms: Option<f64>) {
        let blend = |slot: &mut Option<f32>, x: f64| {
            let x = x as f32;
            *slot = Some(slot.map_or(x, |old| old + (x - old) * Self::LEARN));
        };
        blend(&mut self.inflow[hour % HOURS_OF_WEEK], inflow_per_min);
        if let Some(ms) = send_ms {
            blend(&mut self.send_ms[hour % HOURS_OF_WEEK], ms);
        }
    }

    /// Messages per minute normally arriving in this hour of the week (unknown until it was watched).
    pub fn inflow(&self, hour: usize) -> Option<f64> {
        self.inflow[hour % HOURS_OF_WEEK].map(f64::from)
    }

    /// A send's normal time in this hour; until that hour was watched, the mean over the hours
    /// that were (a receiver's speed depends less on the hour than its traffic does).
    pub fn send_ms(&self, hour: usize) -> Option<f64> {
        if let Some(ms) = self.send_ms[hour % HOURS_OF_WEEK] {
            return Some(ms.into());
        }
        let known: Vec<f64> = self.send_ms.iter().flatten().map(|&v| v.into()).collect();
        (!known.is_empty()).then(|| known.iter().sum::<f64>() / known.len() as f64)
    }
}

/// A destination over the last few minutes (the engine samples its counts every few seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recent {
    /// How long the samples cover.
    pub minutes: f64,
    /// Messages received for it (the interface's).
    pub received: u64,
    /// Messages it finished (sent, filtered or quarantined): what shrinks its queue.
    pub done: u64,
    /// Mean time of its successful sends.
    pub send_ms: Option<f64>,
    /// Since it last finished a message (or since the engine started watching).
    pub since_progress: Duration,
}

impl Recent {
    fn per_minute(&self, n: u64) -> f64 {
        n as f64 / self.minutes.max(1.0 / 60.0)
    }
}

/// What's known about one destination now.
#[derive(Debug, Clone, Default)]
pub struct Facts<'a> {
    pub waiting: u64,
    /// How long the oldest waiting message has waited.
    pub oldest: Option<Duration>,
    pub quarantined: u64,
    pub last_sent: Option<DateTime<Utc>>,
    pub last_error: Option<(DateTime<Utc>, &'a str)>,
    /// Paused on purpose: how to say it ("paused by ann at 14:02 (no reason yet)").
    pub paused: Option<String>,
    /// Its interface was stopped (its code ran away): why.
    pub stopped: Option<&'a str>,
    pub recent: Option<Recent>,
    /// Its `behind` limit, if operations set one.
    pub behind: Option<Behind>,
    /// Normal for this hour of the week.
    pub normal_inflow: Option<f64>,
    pub normal_send_ms: Option<f64>,
}

/// A destination's state, best to worst. The numbers are Prometheus's
/// (`ductus_destination_state`): never change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Ok = 0,
    /// Past its `behind` limit.
    Behind = 1,
    /// Has quarantined messages: someone has to look.
    Error = 2,
    /// Messages wait and nothing is finished.
    Stalled = 3,
    /// Paused on purpose (said instead of stalled).
    Paused = 4,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Behind => "behind",
            State::Error => "error",
            State::Stalled => "stalled",
            State::Paused => "paused",
        }
    }
}

/// Why a queue grows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cause {
    /// More is arriving than normal for this hour (sending is normal).
    Burst { per_min: f64, normal: f64 },
    /// Each send takes longer than normal.
    SlowReceiver { ms: f64, normal: f64 },
}

/// Where a backlog is heading at the current rates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trend {
    CatchingUp {
        eta: Duration,
    },
    /// Arriving as fast as or faster than it's finished.
    FallingBehind {
        in_per_min: f64,
        out_per_min: f64,
    },
}

/// A destination's state, why, and how to say it.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnosis {
    pub state: State,
    /// Which `behind` limit is past.
    pub tripped: Option<Tripped>,
    pub causes: Vec<Cause>,
    pub trend: Option<Trend>,
    /// In operations' words, e.g. "behind 1,240 (oldest 12 min), catching up ~6 min".
    pub summary: String,
}

/// A backlog whose oldest message waited less than this isn't diagnosed (a queue that flows
/// always has a few messages in it), and a destination is stalled once it finished nothing for
/// this long with messages waiting.
pub const STALL_AFTER: Duration = Duration::from_secs(60);

/// A receiver is slow when a send takes over twice its normal time and at least this much more
/// (2 ms → 5 ms is noise, not a slow receiver).
const SLOW_BY_MS: f64 = 5.0;
/// A burst: over twice the normal inflow, and at least this many more per minute.
const BURST_BY_PER_MIN: f64 = 5.0;

/// Diagnose a destination from what's known about it now.
pub fn diagnose(facts: &Facts, now: DateTime<Utc>) -> Diagnosis {
    let backlog = facts.waiting > 0 && facts.oldest.is_some_and(|age| age >= STALL_AFTER);
    let tripped = facts.behind.and_then(|b| b.tripped(facts.waiting, facts.oldest));
    let mut causes = Vec::new();
    let mut trend = None;
    let mut parts = Vec::new();
    let mut state = State::Ok;

    let stalled = backlog && facts.recent.as_ref().is_some_and(|r| r.since_progress >= STALL_AFTER);
    if let Some(paused) = &facts.paused {
        state = State::Paused;
        parts.push(format!("{paused}, {} waiting", count(facts.waiting)));
    } else if let Some(reason) = facts.stopped {
        state = State::Stalled;
        parts.push(format!("stalled: interface stopped ({reason}), {} waiting", count(facts.waiting)));
    } else if stalled {
        state = State::Stalled;
        let idle = facts.recent.map(|r| r.since_progress).unwrap_or_default();
        let since = now - Span::from_std(idle).unwrap_or_default();
        let why = match facts.last_error {
            Some((at, error)) if at >= since => format!("retrying: {}", first_line(error)),
            _ => "no error: a blocked lane or a receiver that doesn't answer (`ductus hotspots` shows each lane's first message)".to_string(),
        };
        parts.push(format!("stalled: nothing sent for {}, {} waiting; {why}", human(idle), count(facts.waiting)));
    }

    if facts.quarantined > 0 {
        state = state.max(State::Error);
        let last = facts.last_error.map(|(_, e)| format!(" (last error: {})", first_line(e))).unwrap_or_default();
        parts.push(format!("error: {} quarantined{last}", count(facts.quarantined)));
    }

    if state < State::Stalled && (backlog || tripped.is_some()) {
        if tripped.is_some() {
            state = state.max(State::Behind);
        }
        let oldest = facts.oldest.map(|a| format!(" (oldest {})", human(a))).unwrap_or_default();
        // Without a limit past, it's information, not a problem.
        let mut text = match tripped {
            Some(_) => format!("behind {}{oldest}", count(facts.waiting)),
            None => format!("{} waiting{oldest}", count(facts.waiting)),
        };
        if let Some(r) = facts.recent {
            let (inflow, out) = (r.per_minute(r.received), r.per_minute(r.done));
            if out > inflow {
                let eta = Duration::from_secs_f64(facts.waiting as f64 / (out - inflow) * 60.0);
                trend = Some(Trend::CatchingUp { eta });
                text += &format!(", catching up ~{}", human(eta.max(Duration::from_secs(60))));
            } else if out > 0.0 {
                trend = Some(Trend::FallingBehind { in_per_min: inflow, out_per_min: out });
                text += &format!(", not catching up: {}/min in, {}/min out", rate(inflow), rate(out));
            }
            if let (Some(ms), Some(normal)) = (r.send_ms, facts.normal_send_ms)
                && ms > 2.0 * normal
                && ms - normal >= SLOW_BY_MS
            {
                causes.push(Cause::SlowReceiver { ms, normal });
                text += &format!("; slow receiver: {} ms/msg (normal {})", rate(ms), rate(normal));
            }
            if let Some(normal) = facts.normal_inflow
                && inflow > 2.0 * normal
                && inflow - normal >= BURST_BY_PER_MIN
            {
                causes.push(Cause::Burst { per_min: inflow, normal });
                text += &format!("; burst: {}/min received (normal {})", rate(inflow), rate(normal));
            }
        }
        parts.push(text);
    }

    if parts.is_empty() || state == State::Ok {
        let ok = match facts.last_sent {
            Some(at) => format!("ok, sent {} ago", human((now - at).to_std().unwrap_or_default())),
            None => "ok, nothing sent yet".to_string(),
        };
        parts.insert(0, ok);
    }
    Diagnosis { state, tripped, causes, trend, summary: parts.join("; ") }
}

/// An interface's own state: its expectations of what it receives. The numbers are
/// Prometheus's (`ductus_interface_state`): never change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InterfaceState {
    Ok = 0,
    /// Nothing received within `active_within`.
    Silent = 1,
    /// Nothing received by `active_by`.
    Missed = 2,
    /// Its code ran away; nothing is processed until it's fixed and the engine restarted.
    Stopped = 3,
}

impl InterfaceState {
    pub fn label(self) -> &'static str {
        match self {
            InterfaceState::Ok => "ok",
            InterfaceState::Silent => "silent",
            InterfaceState::Missed => "missed",
            InterfaceState::Stopped => "stopped",
        }
    }
}

/// An interface's state and how to say it ("silent: nothing received for 47 min").
pub fn interface_state(
    monitor: &Monitor,
    last_received: Option<DateTime<Utc>>,
    watched_since: DateTime<Utc>,
    stopped: Option<&str>,
    now: DateTime<Utc>,
) -> (InterfaceState, String) {
    let mut state = InterfaceState::Ok;
    let mut parts = Vec::new();
    if let Some(reason) = stopped {
        state = InterfaceState::Stopped;
        parts.push(format!("stopped: {reason}"));
    }
    if let Some((start, deadline)) = monitor.missed(last_received, watched_since, now) {
        state = state.max(InterfaceState::Missed);
        let at = |t: DateTime<Utc>| t.with_timezone(&Local).format("%a %H:%M").to_string();
        parts.push(format!("missed {}: nothing received since {}", at(deadline), at(start)));
    }
    if let Some(quiet) = monitor.silent(last_received.unwrap_or(watched_since), now) {
        state = state.max(InterfaceState::Silent);
        parts.push(format!("silent: nothing received for {}", human(quiet.to_std().unwrap_or_default())));
    }
    if parts.is_empty() {
        parts.push(match last_received {
            Some(at) => format!("ok, received {} ago", human((now - at).to_std().unwrap_or_default())),
            None => "ok, nothing received yet".to_string(),
        });
    }
    (state, parts.join("; "))
}

/// A duration the way people say it: "4s", "47 min", "3 h 10 min", "5 days".
pub fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{} min", s / 60),
        3600..172_800 => match (s / 3600, s % 3600 / 60) {
            (h, 0) => format!("{h} h"),
            (h, m) => format!("{h} h {m} min"),
        },
        _ => format!("{} days", s / 86_400),
    }
}

/// 1240 → "1,240".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A rate or a time: whole above 10, one decimal below ("180", "2.5").
fn rate(x: f64) -> String {
    if x >= 10.0 { count(x.round() as u64) } else { format!("{:.1}", x) }
}

pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > 160 { format!("{}…", line.chars().take(160).collect::<String>()) } else { line.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(text: &str) -> DateTime<Utc> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
        Local.from_local_datetime(&naive).earliest().unwrap().with_timezone(&Utc)
    }

    fn monitor(yaml: &str) -> Monitor {
        let m: Monitor = serde_yaml_ng::from_str(yaml).unwrap();
        m.validate().unwrap();
        m
    }

    #[test]
    fn reads_every_form() {
        let m = monitor("{ active_within: 30m, between: '06:00-22:00', behind: { messages: 500, age: 10m } }");
        assert_eq!(m.active_within, Some(Duration::from_secs(1800)));
        assert_eq!(m.between.unwrap().to_string(), "06:00-22:00");
        assert_eq!(m.behind, Some(Behind { messages: Some(500), age: Some(Duration::from_secs(600)) }));
        assert_eq!(monitor("{ behind: 500 }").behind.unwrap().messages, Some(500));
        assert_eq!(monitor("{ behind: '500' }").behind.unwrap().messages, Some(500));
        assert_eq!(monitor("{ behind: 10m }").behind.unwrap().age, Some(Duration::from_secs(600)));
        assert!(monitor("{}").is_empty());
        for at in ["09:00", "Mon 09:00", "mon-fri 9:30", "Mon,Thu 09:00", "0 9 * * 1-5"] {
            At::parse(at).unwrap_or_else(|e| panic!("{at}: {e}"));
        }

        let err = |yaml: &str| match serde_yaml_ng::from_str::<Monitor>(yaml) {
            Ok(m) => m.validate().unwrap_err().to_string(),
            Err(e) => e.to_string(),
        };
        assert!(err("{ behind: {} }").contains("needs `messages`"));
        assert!(err("{ behind: 0 }").contains("at least 1"));
        assert!(err("{ behind: soon }").contains("invalid duration"));
        assert!(err("{ between: '06:00-22:00' }").contains("goes with `active_within`"));
        assert!(err("{ active_by: '09:00', from: '25:00' }").contains("Mon 09:00"));
        assert!(err("{ from: '18:00' }").contains("goes with `active_by`"));
        assert!(err("{ active_within: 30s }").contains("at least 1m"));
        assert!(err("{ active_within: 30m, between: '06:00-06:00' }").contains("same time"));
        assert!(err("{ active_within: 30m, between: '6am' }").contains("06:00-22:00"));
        assert!(err("{ bogus: 1 }").contains("bogus"));
    }

    #[test]
    fn silent_counts_only_inside_the_hours() {
        let m = monitor("{ active_within: 30m, between: '06:00-22:00' }");
        let last = local("2026-10-07 21:50");
        // At night nothing is expected; in the morning the clock starts at 06:00.
        assert_eq!(m.silent(last, local("2026-10-08 03:00")), None);
        assert_eq!(m.silent(last, local("2026-10-08 06:20")), None);
        let silent = m.silent(last, local("2026-10-08 06:31")).unwrap();
        assert_eq!(silent.num_minutes(), 8 * 60 + 41, "reports the real quiet time");
        // All day: just the gap.
        let always = monitor("{ active_within: 30m }");
        assert_eq!(always.silent(last, local("2026-10-07 22:15")), None);
        assert!(always.silent(last, local("2026-10-07 22:21")).is_some());
        // Overnight hours.
        let night = monitor("{ active_within: 1h, between: '22:00-06:00' }");
        assert_eq!(night.silent(local("2026-10-07 12:00"), local("2026-10-07 22:59")), None);
        assert!(night.silent(local("2026-10-07 12:00"), local("2026-10-08 01:00")).is_some());
        assert_eq!(night.silent(local("2026-10-07 12:00"), local("2026-10-08 07:00")), None);
    }

    #[test]
    fn missed_deadlines() {
        // 2026-10-08 is a Thursday.
        let m = monitor("{ active_by: '09:00' }");
        let watched = local("2026-10-01 00:00");
        let now = local("2026-10-08 09:30");
        // Yesterday's file doesn't count for today; a file after midnight does.
        let missed = m.missed(Some(local("2026-10-07 23:00")), watched, now).unwrap();
        assert_eq!(missed, (local("2026-10-08 00:00"), local("2026-10-08 09:00")));
        assert_eq!(m.missed(Some(local("2026-10-08 06:00")), watched, now), None);
        assert_eq!(m.missed(Some(local("2026-10-08 06:00")), watched, local("2026-10-08 08:59")), None);
        // Late is still received: the missed deadline clears once it comes.
        assert_eq!(m.missed(Some(local("2026-10-08 09:20")), watched, now), None);
        // Before this deadline, the latest one is yesterday's.
        assert_eq!(
            m.missed(Some(local("2026-10-06 23:00")), watched, local("2026-10-08 08:00")),
            Some((local("2026-10-07 00:00"), local("2026-10-07 09:00")))
        );
        // Nothing expected before the interface was watched.
        assert_eq!(m.missed(None, local("2026-10-08 07:00"), now), None);

        // `from` widens the window: a file sent the evening before.
        let evening = monitor("{ active_by: '09:00', from: '18:00' }");
        assert_eq!(evening.missed(Some(local("2026-10-07 19:00")), watched, now), None);
        assert_eq!(
            evening.missed(Some(local("2026-10-07 17:00")), watched, now),
            Some((local("2026-10-07 18:00"), local("2026-10-08 09:00")))
        );

        // Weekly: Monday's deadline is the latest until next Monday.
        let weekly = monitor("{ active_by: 'Mon 09:00', from: 'Fri 18:00' }");
        assert_eq!(weekly.missed(None, watched, now), Some((local("2026-10-02 18:00"), local("2026-10-05 09:00"))));
        assert_eq!(weekly.missed(Some(local("2026-10-03 10:00")), watched, now), None);
    }

    #[test]
    fn behind_by_count_or_age() {
        let both = Behind { messages: Some(500), age: Some(Duration::from_secs(600)) };
        assert_eq!(both.tripped(10, Some(Duration::from_secs(60))), None);
        assert_eq!(both.tripped(500, Some(Duration::from_secs(60))), Some(Tripped::Messages(500)));
        // An overnight stall: few messages, but old.
        assert_eq!(both.tripped(3, Some(Duration::from_secs(900))), Some(Tripped::Age(Duration::from_secs(600))));
        let age = Behind { messages: None, age: Some(Duration::from_secs(600)) };
        assert_eq!(age.tripped(0, None), None);
    }

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    /// Five minutes of traffic: `received` and `done` per minute, sends taking `send_ms`.
    fn recent(received: u64, done: u64, send_ms: f64) -> Recent {
        Recent {
            minutes: 5.0,
            received: received * 5,
            done: done * 5,
            send_ms: Some(send_ms),
            since_progress: Duration::from_secs(1),
        }
    }

    #[test]
    fn a_flowing_queue_is_ok() {
        let now = Utc::now();
        let facts = Facts {
            waiting: 12,
            oldest: Some(Duration::from_millis(300)),
            last_sent: Some(now - Span::seconds(2)),
            recent: Some(recent(100, 100, 12.0)),
            behind: Some(Behind { messages: Some(500), age: Some(mins(10)) }),
            ..Default::default()
        };
        let d = diagnose(&facts, now);
        assert_eq!((d.state, d.summary.as_str()), (State::Ok, "ok, sent 2s ago"));
        assert_eq!(diagnose(&Facts::default(), now).summary, "ok, nothing sent yet");
    }

    #[test]
    fn behind_and_why() {
        let now = Utc::now();
        let base = Facts {
            waiting: 1240,
            oldest: Some(mins(12)),
            last_sent: Some(now),
            behind: Some(Behind { messages: Some(500), age: None }),
            normal_inflow: Some(80.0),
            normal_send_ms: Some(12.0),
            ..Default::default()
        };
        // A burst, sending normally and catching up: 400/min in, 600/min out → 1,240 in ~6 min.
        let burst = diagnose(&Facts { recent: Some(recent(400, 600, 12.0)), ..base.clone() }, now);
        assert_eq!(burst.state, State::Behind);
        assert_eq!(burst.tripped, Some(Tripped::Messages(500)));
        assert_eq!(burst.causes, vec![Cause::Burst { per_min: 400.0, normal: 80.0 }]);
        assert_eq!(
            burst.summary,
            "behind 1,240 (oldest 12 min), catching up ~6 min; burst: 400/min received (normal 80)"
        );
        // A slow receiver, normal inflow, losing ground.
        let slow = diagnose(&Facts { recent: Some(recent(80, 60, 180.0)), ..base.clone() }, now);
        assert_eq!(slow.causes, vec![Cause::SlowReceiver { ms: 180.0, normal: 12.0 }]);
        assert!(matches!(slow.trend, Some(Trend::FallingBehind { .. })));
        assert_eq!(
            slow.summary,
            "behind 1,240 (oldest 12 min), not catching up: 80/min in, 60/min out; slow receiver: 180 ms/msg (normal 12)"
        );
        // Small differences are not causes.
        let noise =
            diagnose(&Facts { recent: Some(recent(84, 100, 4.0)), normal_send_ms: Some(1.5), ..base.clone() }, now);
        assert!(noise.causes.is_empty(), "{:?}", noise.causes);
        // No limit set: the backlog is shown, the state stays ok.
        let info = diagnose(&Facts { behind: None, recent: Some(recent(80, 200, 12.0)), ..base }, now);
        assert_eq!(info.state, State::Ok);
        assert_eq!(info.summary, "ok, sent 0s ago; 1,240 waiting (oldest 12 min), catching up ~10 min");
    }

    #[test]
    fn stalled_says_why_and_paused_says_who() {
        let now = Utc::now();
        let idle = Recent { since_progress: mins(3), ..recent(10, 0, 0.0) };
        let facts = Facts {
            waiting: 40,
            oldest: Some(mins(4)),
            last_sent: Some(now - Span::minutes(3)),
            last_error: Some((now - Span::seconds(5), "connection refused (lab.local:5000)\nmore detail")),
            recent: Some(idle),
            ..Default::default()
        };
        let d = diagnose(&facts, now);
        assert_eq!(d.state, State::Stalled);
        assert_eq!(
            d.summary,
            "stalled: nothing sent for 3 min, 40 waiting; retrying: connection refused (lab.local:5000)"
        );
        // An error older than the stall isn't the reason.
        let old_error = Facts { last_error: Some((now - Span::minutes(30), "old")), ..facts.clone() };
        assert!(diagnose(&old_error, now).summary.contains("no error: a blocked lane"));
        // Quarantined messages are an error at once, also while stalled.
        let both = diagnose(&Facts { quarantined: 2, ..facts.clone() }, now);
        assert_eq!(both.state, State::Stalled);
        assert!(both.summary.ends_with("; error: 2 quarantined (last error: connection refused (lab.local:5000))"));
        let error = diagnose(&Facts { quarantined: 2, waiting: 0, oldest: None, ..facts.clone() }, now);
        assert_eq!(error.state, State::Error);
        // Paused is said instead of stalled.
        let paused = Facts { paused: Some("paused by ann at 14:02 (no reason yet)".into()), ..facts.clone() };
        let d = diagnose(&paused, now);
        assert_eq!(
            (d.state, d.summary.as_str()),
            (State::Paused, "paused by ann at 14:02 (no reason yet), 40 waiting")
        );
        // A stopped interface stalls every destination.
        let stopped = diagnose(&Facts { stopped: Some("ran past its timeout 3 times"), recent: None, ..facts }, now);
        assert_eq!(stopped.state, State::Stalled);
        assert!(stopped.summary.starts_with("stalled: interface stopped (ran past its timeout 3 times)"));
    }

    #[test]
    fn interface_states() {
        let m = monitor("{ active_within: 30m, active_by: '09:00' }");
        let now = local("2026-10-08 09:47");
        let watched = local("2026-10-01 00:00");
        let (state, text) = interface_state(&m, Some(local("2026-10-07 23:00")), watched, None, now);
        assert_eq!(state, InterfaceState::Missed);
        assert_eq!(
            text,
            "missed Thu 09:00: nothing received since Thu 00:00; silent: nothing received for 10 h 47 min"
        );
        let (state, text) = interface_state(&m, Some(local("2026-10-08 09:46")), watched, None, now);
        assert_eq!((state, text.as_str()), (InterfaceState::Ok, "ok, received 1 min ago"));
        let (state, _) = interface_state(&m, Some(local("2026-10-08 09:46")), watched, Some("heap"), now);
        assert_eq!(state, InterfaceState::Stopped);
        // Never received: quiet since it was first watched.
        let (_, text) = interface_state(&m, None, local("2026-10-08 09:00"), None, now);
        assert_eq!(text, "silent: nothing received for 47 min");
    }

    #[test]
    fn the_baseline_learns_each_hour_of_the_week() {
        let mut b = Baseline::default();
        assert_eq!((b.inflow(7), b.send_ms(7)), (None, None));
        b.learn(7, 100.0, Some(10.0));
        assert_eq!((b.inflow(7), b.send_ms(7)), (Some(100.0), Some(10.0)));
        // Recent weeks weigh more, but one odd hour doesn't replace normal.
        b.learn(7, 200.0, None);
        assert!((b.inflow(7).unwrap() - 130.0).abs() < 1e-3);
        // Another hour: no inflow known; send time falls back to the hours known.
        b.learn(8, 50.0, Some(20.0));
        assert_eq!(b.inflow(9), None);
        assert_eq!(b.send_ms(9), Some(15.0));
        // Stored values of the wrong shape start over.
        assert_eq!(Baseline::from_stored(Some(vec![Some(1.0)]), None), Baseline::default());
        // 2026-10-05 is a Monday.
        assert_eq!(hour_of_week(local("2026-10-05 00:30")), 0);
        assert_eq!(hour_of_week(local("2026-10-05 07:59")), 7);
        assert_eq!(hour_of_week(local("2026-10-11 23:00")), 167);
    }

    #[test]
    fn words_and_numbers() {
        assert_eq!(human(Duration::from_secs(4)), "4s");
        assert_eq!(human(mins(47)), "47 min");
        assert_eq!(human(mins(190)), "3 h 10 min");
        assert_eq!(human(mins(120)), "2 h");
        assert_eq!(human(mins(60 * 24 * 5)), "5 days");
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1240), "1,240");
        assert_eq!(count(90_000_000), "90,000,000");
        assert_eq!(rate(2.54), "2.5");
    }
}
