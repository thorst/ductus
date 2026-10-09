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
}
