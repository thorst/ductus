//! When schedule sources run: `every: 15m` or `cron: "0 2 * * *"` (in the schedule's
//! `timezone`, else ductus.yaml's, else the server's).
//!
//! The engine remembers the last run (`anchor`, a moment in UTC, so changing the zone loses no
//! run). The next run is the first time after it. If the engine was down through one or more
//! runs, it runs once on start, not once per missed run.
//!
//! Daylight saving is croner's, on the zone's rules: a fixed time the clock skips in spring
//! (02:30 on the night it jumps from 02:00 to 03:00) runs at the first minute after the gap
//! (03:00), and a fixed time it shows twice in autumn (01:30) runs once, the first time. A
//! pattern that repeats within the hour (`*/15 * * * *`) runs through both halves of a repeated
//! hour: it's a real hour of time.

use crate::config::Schedule;
use crate::zone::Zone;
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Timing {
    Every(Duration),
    Cron { cron: Box<croner::Cron>, zone: Zone },
}

impl Timing {
    /// `engine`: the zone a `cron` without its own `timezone` is read in (ductus.yaml's).
    pub fn new(schedule: &Schedule, engine: Zone) -> Result<Self> {
        match (schedule.every, &schedule.cron) {
            (Some(every), None) => {
                if every < Duration::from_secs(1) {
                    bail!("schedule `every` must be at least 1s");
                }
                if schedule.timezone.is_some() {
                    bail!("schedule `timezone` goes with `cron` (`every` counts elapsed time, the same in every zone)");
                }
                Ok(Self::Every(every))
            }
            (None, Some(expr)) => {
                let cron = expr.parse::<croner::Cron>().map_err(|e| {
                    anyhow!("invalid cron `{expr}`: {e} (five fields: minute hour day-of-month month day-of-week)")
                })?;
                Ok(Self::Cron { cron: Box::new(cron), zone: schedule.timezone.unwrap_or(engine) })
            }
            _ => bail!("a schedule needs one of `every` (e.g. 15m) or `cron` (e.g. \"0 2 * * *\")"),
        }
    }

    /// The zone run times are told in (`due` in a run's message): the cron's, or the engine's
    /// for `every`.
    pub fn zone(&self, engine: Zone) -> Zone {
        match self {
            Self::Every(_) => engine,
            Self::Cron { zone, .. } => *zone,
        }
    }

    /// The first run time strictly after `t`.
    pub fn next_after(&self, t: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match self {
            Self::Every(every) => Ok(t + *every),
            Self::Cron { cron, zone } => cron
                .find_next_occurrence(&zone.at(t), false)
                .map(|next| next.with_timezone(&Utc))
                .map_err(|e| anyhow!("no next run time for `{}` in {}: {e}", cron.as_str(), zone.describe())),
        }
    }

    /// The next run, given the last one (`anchor`). With no history, `every` runs right away and
    /// `cron` waits for its next time. A run that was missed while the engine was down is due now.
    pub fn next_due(&self, anchor: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match (anchor, self) {
            (Some(anchor), _) => self.next_after(anchor),
            (None, Self::Every(_)) => Ok(now),
            (None, Self::Cron { .. }) => self.next_after(now),
        }
    }

    /// The anchor to keep after running `due`. If later runs are already overdue (the engine
    /// was down, or a run came very late), skip them: the next run is then counted from now.
    pub fn anchor_after(&self, due: DateTime<Utc>, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        Ok(if self.next_after(due)? <= now { now } else { due })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn schedule(every: Option<&str>, cron: Option<&str>, timezone: Option<&str>) -> Schedule {
        Schedule {
            every: every.map(|e| crate::config::parse_duration(e).unwrap()),
            cron: cron.map(str::to_string),
            timezone: timezone.map(|z| Zone::parse(z).unwrap()),
        }
    }

    fn timing(every: Option<&str>, cron: Option<&str>) -> Result<Timing> {
        Timing::new(&schedule(every, cron, None), Zone::Server)
    }

    /// `cron` in `zone`, and its next `n` runs from `from`, as the zone's clock shows them.
    fn runs(cron: &str, zone: &str, from: &str, n: usize) -> Vec<String> {
        let t = Timing::new(&schedule(None, Some(cron), Some(zone)), Zone::Server).unwrap();
        let zone = t.zone(Zone::Server);
        let mut at_ = at(from);
        (0..n)
            .map(|_| {
                at_ = t.next_after(at_).unwrap();
                zone.at(at_).to_rfc3339()
            })
            .collect()
    }

    #[test]
    fn every_runs_now_then_on_the_interval() {
        let t = timing(Some("15m"), None).unwrap();
        let now = at("2026-10-06T10:00:00Z");
        assert_eq!(t.next_due(None, now).unwrap(), now);
        assert_eq!(t.next_due(Some(now), now).unwrap(), at("2026-10-06T10:15:00Z"));
    }

    #[test]
    fn missed_runs_run_once() {
        let t = timing(Some("15m"), None).unwrap();
        let last = at("2026-10-06T08:00:00Z");
        let now = at("2026-10-06T10:07:00Z"); // down for two hours
        let due = t.next_due(Some(last), now).unwrap();
        assert!(due <= now, "overdue: runs immediately");
        // After that one run, the next is counted from now, not from 08:15.
        let anchor = t.anchor_after(due, now).unwrap();
        assert_eq!(t.next_due(Some(anchor), now).unwrap(), at("2026-10-06T10:22:00Z"));
        // On time: the anchor is the due time itself, so the cadence doesn't drift.
        let on_time = at("2026-10-06T10:15:00Z");
        assert_eq!(t.anchor_after(on_time, at("2026-10-06T10:15:00.020Z")).unwrap(), on_time);
    }

    #[test]
    fn cron_waits_for_its_time() {
        let t = timing(None, Some("*/15 * * * *")).unwrap();
        let now = at("2026-10-06T10:07:30Z");
        assert_eq!(t.next_due(None, now).unwrap(), at("2026-10-06T10:15:00Z"));
        assert_eq!(t.next_after(at("2026-10-06T10:15:00Z")).unwrap(), at("2026-10-06T10:30:00Z"));
    }

    #[test]
    fn rejects_bad_schedules() {
        assert!(timing(None, None).is_err());
        assert!(timing(Some("1m"), Some("* * * * *")).is_err());
        assert!(timing(Some("500ms"), None).unwrap_err().to_string().contains("at least 1s"));
        let err = timing(None, Some("61 * * * *")).unwrap_err().to_string();
        assert!(err.contains("invalid cron"), "{err}");
        let every = Timing::new(&schedule(Some("1h"), None, Some("UTC")), Zone::Server).unwrap_err().to_string();
        assert!(every.contains("`timezone` goes with `cron`"), "{every}");
        let yaml = serde_yaml_ng::from_str::<Schedule>("{ cron: '0 6 * * *', timezone: Central }");
        assert!(yaml.unwrap_err().to_string().contains("unknown time zone `Central`"));
    }

    #[test]
    fn cron_reads_its_own_zone_or_the_engines() {
        let chicago = Zone::parse("America/Chicago").unwrap();
        // 06:00 every day in Chicago is 12:00 UTC in winter and 11:00 UTC in summer.
        let own = Timing::new(&schedule(None, Some("0 6 * * *"), Some("America/Chicago")), Zone::Server).unwrap();
        assert_eq!(own.next_after(at("2026-01-15T00:00:00Z")).unwrap(), at("2026-01-15T12:00:00Z"));
        assert_eq!(own.next_after(at("2026-07-15T00:00:00Z")).unwrap(), at("2026-07-15T11:00:00Z"));
        // No `timezone`: the engine's.
        let engine = Timing::new(&schedule(None, Some("0 6 * * *"), None), chicago).unwrap();
        assert_eq!(engine.next_after(at("2026-01-15T00:00:00Z")).unwrap(), at("2026-01-15T12:00:00Z"));
        assert_eq!(engine.zone(Zone::Server), chicago);
        // The schedule's own wins over the engine's.
        let utc = Timing::new(&schedule(None, Some("0 6 * * *"), Some("UTC")), chicago).unwrap();
        assert_eq!(utc.next_after(at("2026-01-15T00:00:00Z")).unwrap(), at("2026-01-15T06:00:00Z"));
        // `every` tells its times in the engine's zone.
        assert_eq!(timing(Some("1h"), None).unwrap().zone(chicago), chicago);
    }

    /// The next-run table across both changes of 2026 in Chicago (March 8: 02:00 → 03:00;
    /// November 1: 02:00 → 01:00).
    #[test]
    fn daylight_saving_in_spring() {
        // 02:30 doesn't happen on March 8: that day it runs at 03:00, the first minute after.
        assert_eq!(
            runs("30 2 * * *", "America/Chicago", "2026-03-06T12:00:00Z", 4),
            [
                "2026-03-07T02:30:00-06:00",
                "2026-03-08T03:00:00-05:00",
                "2026-03-09T02:30:00-05:00",
                "2026-03-10T02:30:00-05:00",
            ]
        );
        // Every 30 minutes: the skipped hour simply isn't there.
        assert_eq!(
            runs("*/30 * * * *", "America/Chicago", "2026-03-08T07:00:00Z", 3),
            ["2026-03-08T01:30:00-06:00", "2026-03-08T03:00:00-05:00", "2026-03-08T03:30:00-05:00"]
        );
        // Outside the gap, nothing moves.
        assert_eq!(runs("0 6 * * *", "America/Chicago", "2026-03-07T00:00:00Z", 2)[1], "2026-03-08T06:00:00-05:00");
    }

    #[test]
    fn daylight_saving_in_autumn() {
        // 01:30 happens twice on November 1: it runs once, the first time.
        assert_eq!(
            runs("30 1 * * *", "America/Chicago", "2026-10-31T12:00:00Z", 3),
            ["2026-11-01T01:30:00-05:00", "2026-11-02T01:30:00-06:00", "2026-11-03T01:30:00-06:00"]
        );
        // Every 30 minutes runs through both halves of the repeated hour: it's a real hour.
        assert_eq!(
            runs("*/30 * * * *", "America/Chicago", "2026-11-01T05:45:00Z", 5),
            [
                "2026-11-01T01:00:00-05:00",
                "2026-11-01T01:30:00-05:00",
                "2026-11-01T01:00:00-06:00",
                "2026-11-01T01:30:00-06:00",
                "2026-11-01T02:00:00-06:00",
            ]
        );
    }

    #[test]
    fn a_zone_on_the_other_side_of_the_world() {
        // Sydney changes the other way, in April and October; India has no daylight saving.
        assert_eq!(
            runs("30 2 * * *", "Australia/Sydney", "2026-10-02T00:00:00Z", 2),
            ["2026-10-03T02:30:00+10:00", "2026-10-04T03:00:00+11:00"]
        );
        assert_eq!(runs("0 9 * * 1", "Asia/Kolkata", "2026-11-18T00:00:00Z", 1), ["2026-11-23T09:00:00+05:30"]);
    }
}
