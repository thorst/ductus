//! How long an interface's messages are kept: 1, 3 or 7 weeks.
//!
//! Fixed windows, not any duration: each is a tier of partitions in the database, and old
//! messages go by dropping whole partitions (no row-by-row deletes on the busiest tables). Few,
//! short tiers keep the partition count low, which every query that can't prune pays for. Longer
//! history belongs to the source systems or an archive. Weeks also settle the "29 or 31 days?"
//! debates.

use anyhow::{Result, bail};
use serde::Deserialize;

/// Days messages are kept: 7, 21 or 49 (written `1w`, `3w`, `7w`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Retention(u16);

impl Retention {
    /// The windows in days, shortest first (the store's tiers: `ductus_store::TIERS`). Chosen
    /// for the partitions they cost: about window / 7 each, one per day for 1w (~24 in all).
    pub const DAYS: [u16; 3] = [7, 21, 49];

    /// When neither the interface nor ductus.yaml says: the longest, so nothing is lost by
    /// surprise; long enough to look into last month's problems.
    pub const DEFAULT: Retention = Retention(49);

    pub fn days(self) -> u16 {
        self.0
    }

    /// From `3w`, or the same in days (`21d`, `21`).
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let days: Option<u16> = match trimmed.strip_suffix('w') {
            Some(weeks) => weeks.trim().parse::<u16>().ok().and_then(|w| w.checked_mul(7)),
            None => trimmed.strip_suffix('d').unwrap_or(trimmed).trim().parse().ok(),
        };
        match days {
            Some(d) if Self::DAYS.contains(&d) => Ok(Self(d)),
            _ => bail!(
                "retention `{trimmed}`: use 1w, 3w or 7w (fixed windows, so old messages go by dropping whole \
                 partitions; longer history belongs to the source system or an archive)"
            ),
        }
    }

    /// From days as the store keeps them.
    pub fn from_days(days: u16) -> Option<Self> {
        Self::DAYS.contains(&days).then_some(Self(days))
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for Retention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}w", self.0 / 7)
    }
}

/// What happens to quarantined messages when their retention window has passed.
///
/// `keep` (the default): they move to the held area with anything still pending, and stay
/// until someone resolves them: a quarantined message is a question nobody has answered yet.
/// `purge`: they go with the regular messages when their partition expires (no second limit).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Quarantined {
    #[default]
    Keep,
    Purge,
}

impl<'de> Deserialize<'de> for Retention {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        // `3w` is a string in YAML; a bare `21` (days) is a number.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Text {
            Number(u64),
            Text(String),
        }
        let text = match Text::deserialize(d)? {
            Text::Number(n) => n.to_string(),
            Text::Text(t) => t,
        };
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_fixed_windows() {
        assert_eq!(Retention::parse("3w").unwrap().days(), 21);
        assert_eq!(Retention::parse(" 7w ").unwrap().to_string(), "7w");
        assert_eq!(Retention::parse("49d").unwrap(), Retention(49));
        assert_eq!(Retention::parse("7").unwrap(), Retention(7));
        for bad in ["45d", "90d", "2w", "1y", "", "3 weeks", "-1w", "9999w"] {
            let err = Retention::parse(bad).unwrap_err().to_string();
            assert!(err.contains("1w, 3w or 7w"), "{bad}: {err}");
        }
        assert_eq!(Retention::default().to_string(), "7w");
        assert_eq!(Retention::from_days(21), Some(Retention(21)));
        assert_eq!(Retention::from_days(90), None);
    }

    #[test]
    fn reads_from_yaml() {
        let r: Retention = serde_yaml_ng::from_str("1w").unwrap();
        assert_eq!(r.days(), 7);
        let r: Retention = serde_yaml_ng::from_str("21").unwrap();
        assert_eq!(r.days(), 21);
        assert!(serde_yaml_ng::from_str::<Retention>("31d").is_err());
    }

    #[test]
    fn quarantined_keep_or_purge() {
        assert_eq!(serde_yaml_ng::from_str::<Quarantined>("purge").unwrap(), Quarantined::Purge);
        assert_eq!(serde_yaml_ng::from_str::<Quarantined>("keep").unwrap(), Quarantined::Keep);
        assert_eq!(Quarantined::default(), Quarantined::Keep);
        assert!(serde_yaml_ng::from_str::<Quarantined>("delete").is_err());
    }
}
