//! The time zone that wall-clock settings are read in: a schedule's `cron`, `monitor` hours and
//! deadlines, the hour-of-week baselines, and alerts' quiet hours.
//!
//! [`Zone::Server`] (the default) is the server's own zone, as the operating system says (on
//! Windows, the service account's). A named zone (`timezone: America/Chicago`, an IANA name)
//! comes from the zone database built into the binary, so Windows, Linux and a container with no
//! zone files all read it the same way; it changes with a Ductus release, not with the server's
//! updates.
//!
//! `Zone` is a chrono [`TimeZone`], so croner finds a cron's times in it directly, daylight
//! saving rules included: a time skipped in spring runs at the first minute after the gap, a
//! fixed time that repeats in autumn runs once.

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike, Utc};
use serde::Deserialize;
use std::fmt;

/// Where wall-clock times are read: the server's zone, or a named one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zone {
    /// The server's own zone (the default: nothing changes for a site that sets none).
    #[default]
    Server,
    /// An IANA zone from the database inside the binary.
    Named(chrono_tz::Tz),
}

impl Zone {
    /// An IANA name as it's spelled in the database (`America/Chicago`, `Europe/Amsterdam`,
    /// `UTC`). A name in the wrong case is refused with the right spelling.
    pub fn parse(name: &str) -> Result<Self> {
        let name = name.trim();
        if let Ok(tz) = name.parse::<chrono_tz::Tz>() {
            return Ok(Self::Named(tz));
        }
        let hint = match chrono_tz::TZ_VARIANTS.iter().find(|tz| tz.name().eq_ignore_ascii_case(name)) {
            Some(tz) => format!("did you mean `{}`?", tz.name()),
            None => "give an IANA name such as `America/Chicago`, `Europe/Amsterdam` or `UTC` \
                     (the list: https://en.wikipedia.org/wiki/List_of_tz_database_time_zones)"
                .into(),
        };
        Err(anyhow!("unknown time zone `{name}`: {hint}"))
    }

    /// Every name `parse` takes (the configuration's JSON Schema lists them for editors).
    pub fn names() -> impl Iterator<Item = &'static str> {
        chrono_tz::TZ_VARIANTS.iter().map(|tz| tz.name())
    }

    /// `t` on this zone's clock.
    pub fn at(self, t: DateTime<Utc>) -> DateTime<Zone> {
        t.with_timezone(&self)
    }

    /// The moment this zone's clock shows `time`. A time the clock skips (spring forward) is the
    /// first minute after the gap; a time it shows twice (fall back) is the first of the two.
    pub fn moment(self, time: NaiveDateTime) -> DateTime<Utc> {
        match self.from_local_datetime(&time) {
            LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => return t.with_timezone(&Utc),
            LocalResult::None => {}
        }
        // In a gap: walk to its end, a minute at a time (gaps are an hour, rarely a day).
        let mut next = time.with_second(0).unwrap_or(time);
        for _ in 0..2 * 24 * 60 {
            next += chrono::Duration::minutes(1);
            if let Some(t) = self.from_local_datetime(&next).earliest() {
                return t.with_timezone(&Utc);
            }
        }
        time.and_utc()
    }

    /// Words for messages: `America/Chicago`, or `the server's time zone`.
    pub fn describe(self) -> String {
        match self {
            Zone::Server => "the server's time zone".into(),
            Zone::Named(tz) => tz.name().into(),
        }
    }

    fn offset(self, fixed: FixedOffset) -> ZoneOffset {
        ZoneOffset { zone: self, fixed }
    }
}

impl fmt::Display for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Zone::Server => f.write_str("server"),
            Zone::Named(tz) => f.write_str(tz.name()),
        }
    }
}

impl<'de> Deserialize<'de> for Zone {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A [`Zone`]'s offset at one moment. It carries its zone, so a `DateTime<Zone>` stays in it
/// through arithmetic (chrono rebuilds the zone from the offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneOffset {
    zone: Zone,
    fixed: FixedOffset,
}

impl Offset for ZoneOffset {
    fn fix(&self) -> FixedOffset {
        self.fixed
    }
}

impl fmt::Display for ZoneOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.fixed, f)
    }
}

impl TimeZone for Zone {
    type Offset = ZoneOffset;

    fn from_offset(offset: &ZoneOffset) -> Self {
        offset.zone
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<ZoneOffset> {
        match self {
            Zone::Server => chrono::Local.offset_from_local_date(local).map(|o| self.offset(o)),
            Zone::Named(tz) => tz.offset_from_local_date(local).map(|o| self.offset(o.fix())),
        }
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<ZoneOffset> {
        match self {
            Zone::Server => chrono::Local.offset_from_local_datetime(local).map(|o| self.offset(o)),
            Zone::Named(tz) => tz.offset_from_local_datetime(local).map(|o| self.offset(o.fix())),
        }
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> ZoneOffset {
        match self {
            Zone::Server => self.offset(chrono::Local.offset_from_utc_date(utc)),
            Zone::Named(tz) => self.offset(tz.offset_from_utc_date(utc).fix()),
        }
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> ZoneOffset {
        match self {
            Zone::Server => self.offset(chrono::Local.offset_from_utc_datetime(utc)),
            Zone::Named(tz) => self.offset(tz.offset_from_utc_datetime(utc).fix()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chicago() -> Zone {
        Zone::parse("America/Chicago").unwrap()
    }

    fn naive(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn names_are_iana_names() {
        assert_eq!(chicago().to_string(), "America/Chicago");
        assert_eq!(Zone::parse(" UTC ").unwrap().to_string(), "UTC");
        let wrong_case = Zone::parse("america/chicago").unwrap_err().to_string();
        assert!(wrong_case.contains("did you mean `America/Chicago`?"), "{wrong_case}");
        let unknown = Zone::parse("Central").unwrap_err().to_string();
        assert!(unknown.contains("unknown time zone `Central`: give an IANA name"), "{unknown}");
        assert!(Zone::names().any(|n| n == "Europe/Amsterdam"));
        let yaml: Zone = serde_yaml_ng::from_str("Asia/Kolkata").unwrap();
        assert_eq!(yaml.describe(), "Asia/Kolkata");
        assert_eq!(Zone::Server.describe(), "the server's time zone");
    }

    #[test]
    fn a_named_zone_keeps_its_rules() {
        // Standard time (UTC-6), summer time (UTC-5): the offset follows the date.
        assert_eq!(chicago().at(utc("2026-01-15T14:00:00Z")).to_rfc3339(), "2026-01-15T08:00:00-06:00");
        assert_eq!(chicago().at(utc("2026-07-15T14:00:00Z")).to_rfc3339(), "2026-07-15T09:00:00-05:00");
        // Arithmetic across the change stays in the zone (the offset carries it).
        let before = chicago().at(utc("2026-03-08T07:00:00Z")); // 01:00 CST
        assert_eq!((before + chrono::Duration::hours(2)).to_rfc3339(), "2026-03-08T04:00:00-05:00");
    }

    #[test]
    fn moments_across_daylight_saving() {
        // 2026-03-08 02:00-03:00 doesn't happen in Chicago: 02:30 is read as 03:00 CDT.
        assert_eq!(chicago().moment(naive("2026-03-08 02:30")), utc("2026-03-08T08:00:00Z"));
        // 2026-11-01 01:00-02:00 happens twice: the first (CDT).
        assert_eq!(chicago().moment(naive("2026-11-01 01:30")), utc("2026-11-01T06:30:00Z"));
        assert_eq!(chicago().moment(naive("2026-11-01 09:00")), utc("2026-11-01T15:00:00Z"));
    }
}
