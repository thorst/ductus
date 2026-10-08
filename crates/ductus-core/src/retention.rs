//! How long an interface's messages are kept: one of a few fixed windows.
//!
//! Fixed windows, not any duration: each is a tier of partitions in the database, and old
//! messages go by dropping whole partitions (no row-by-row deletes on the busiest tables). They
//! also settle the "29 or 31 days?" debates.

use anyhow::{Result, bail};
use serde::Deserialize;

/// Days messages are kept: 7, 21, 45 or 90.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Retention(u16);

impl Retention {
    /// The windows, shortest first (the store's tiers: `ductus_store::TIERS`). Chosen for the
    /// partitions they cost (about window / 7 each; the 7d tier one per day): longer history
    /// belongs to the source systems or an archive.
    pub const DAYS: [u16; 4] = [7, 21, 45, 90];

    /// When neither the interface nor ductus.yaml says: long enough to look into last month's
    /// problems, short enough not to fill a disk by surprise.
    pub const DEFAULT: Retention = Retention(45);

    pub fn days(self) -> u16 {
        self.0
    }

    /// From `45d` (or `45`).
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let days: Option<u16> = trimmed.strip_suffix('d').unwrap_or(trimmed).trim().parse().ok();
        match days {
            Some(d) if Self::DAYS.contains(&d) => Ok(Self(d)),
            _ => bail!(
                "retention `{trimmed}`: use one of 7d, 21d, 45d, 90d (fixed windows: old messages go by \
                 dropping whole partitions)"
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
        write!(f, "{}d", self.0)
    }
}

impl<'de> Deserialize<'de> for Retention {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        // `45d` is a string in YAML; a bare `45` is a number.
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
        assert_eq!(Retention::parse("45d").unwrap().days(), 45);
        assert_eq!(Retention::parse(" 21d ").unwrap().to_string(), "21d");
        assert_eq!(Retention::parse("7").unwrap(), Retention(7));
        for bad in ["30d", "365d", "1y", "", "45 days", "-7d"] {
            let err = Retention::parse(bad).unwrap_err().to_string();
            assert!(err.contains("7d, 21d, 45d, 90d"), "{bad}: {err}");
        }
        assert_eq!(Retention::default().days(), 45);
        assert_eq!(Retention::from_days(90), Some(Retention(90)));
        assert_eq!(Retention::from_days(91), None);
    }

    #[test]
    fn reads_from_yaml() {
        let r: Retention = serde_yaml_ng::from_str("90d").unwrap();
        assert_eq!(r.days(), 90);
        let r: Retention = serde_yaml_ng::from_str("21").unwrap();
        assert_eq!(r.days(), 21);
        assert!(serde_yaml_ng::from_str::<Retention>("31d").is_err());
    }
}
