//! When schedule sources run: `every: 15m` or `cron: "0 2 * * *"` (server local time).
//!
//! The engine remembers the last run (`anchor`). The next run is the first time after it. If the
//! engine was down through one or more runs, it runs once on start, not once per missed run.

use crate::config::Schedule;
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local, Utc};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Timing {
    Every(Duration),
    Cron(Box<croner::Cron>),
}

impl Timing {
    pub fn new(schedule: &Schedule) -> Result<Self> {
        match (schedule.every, &schedule.cron) {
            (Some(every), None) => {
                if every < Duration::from_secs(1) {
                    bail!("schedule `every` must be at least 1s");
                }
                Ok(Self::Every(every))
            }
            (None, Some(expr)) => expr.parse::<croner::Cron>().map(|c| Self::Cron(Box::new(c))).map_err(|e| {
                anyhow!("invalid cron `{expr}`: {e} (five fields: minute hour day-of-month month day-of-week)")
            }),
            _ => bail!("a schedule needs one of `every` (e.g. 15m) or `cron` (e.g. \"0 2 * * *\")"),
        }
    }

    /// The first run time strictly after `t`.
    pub fn next_after(&self, t: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match self {
            Self::Every(every) => Ok(t + *every),
            Self::Cron(cron) => cron
                .find_next_occurrence(&t.with_timezone(&Local), false)
                .map(|next| next.with_timezone(&Utc))
                .map_err(|e| anyhow!("no next run time for `{}`: {e}", cron.as_str())),
        }
    }

    /// The next run, given the last one (`anchor`). With no history, `every` runs right away and
    /// `cron` waits for its next time. A run that was missed while the engine was down is due now.
    pub fn next_due(&self, anchor: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match (anchor, self) {
            (Some(anchor), _) => self.next_after(anchor),
            (None, Self::Every(_)) => Ok(now),
            (None, Self::Cron(_)) => self.next_after(now),
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

    fn timing(every: Option<&str>, cron: Option<&str>) -> Result<Timing> {
        Timing::new(&Schedule {
            every: every.map(|e| crate::config::parse_duration(e).unwrap()),
            cron: cron.map(str::to_string),
        })
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
    }
}
