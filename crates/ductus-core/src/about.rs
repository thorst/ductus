//! Which systems an interface connects: `about:` in interface.yaml, and `application` /
//! `vendor` on destinations and resources.yaml entries. It's what `ductus impact` reads to
//! answer "which interfaces does this vendor breach or VPN change affect?".
//!
//! ```yaml
//! about:
//!   description: ADT from Epic to the lab
//!   owner: Integration team
//!   from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
//!   to: { application: Sunquest Lab, vendor: Sunquest }
//! ```

use anyhow::{Result, bail};
use serde::Deserialize;
use std::net::IpAddr;

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct About {
    /// What the interface does, in a sentence someone on call understands: shown in the web UI,
    /// `ductus impact` and alerts. Example: `description: ADT from Epic to the Sunquest lab`.
    #[serde(default)]
    pub description: Option<String>,
    /// Who to ask about the interface: a person or a team, best written as an email address
    /// (`lab-it@hospital.org`), which matches their account and survives a change of name. An
    /// owner that names an account shows as that person in the web UI (and their "Mine" list);
    /// alert rules with `to: owner` mail them. `ductus owners` lists who owns what. Default:
    /// none. Example: `owner: lab-it@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// The system that sends (for an HTTP source in `respond` mode: the one that calls).
    /// Example: `from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }`.
    #[serde(default)]
    pub from: Origin,
    /// The system messages go to. A destination's own `application` and `vendor` take
    /// precedence for that destination. Example: `to: { application: Sunquest Lab, vendor:
    /// Sunquest }`.
    #[serde(default)]
    pub to: System,
}

/// The sending side: its system, and where its connections come from.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Origin {
    /// The sending application, as people name it. `ductus impact Epic` finds every interface
    /// that names it here or as a receiver. Example: `application: Epic`.
    #[serde(default)]
    pub application: Option<String>,
    /// The sending application's vendor: `ductus impact` by vendor answers "what does this
    /// vendor's breach or VPN change touch?". Example: `vendor: Epic Systems`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Where the sender's connections come from: addresses, subnets (`10.20.30.0/24`) or host
    /// names. The engine can't learn these from the configuration, so a network change (a new
    /// subnet, a firewall move) can only be traced to interfaces that say. `ductus validate`
    /// warns when an `allow:` list doesn't cover them. Default: none. Example: `hosts:
    /// [10.20.30.0/24, epic-bridge01.hospital.org]`.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl Origin {
    pub fn system(&self) -> System {
        System { application: self.application.clone(), vendor: self.vendor.clone() }
    }
}

/// An application and its vendor; either may be unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct System {
    /// The application, as people name it. `ductus impact` finds interfaces by it. Example:
    /// `application: Sunquest Lab`.
    #[serde(default)]
    pub application: Option<String>,
    /// The application's vendor. `ductus impact` finds interfaces by it. Example: `vendor:
    /// Sunquest`.
    #[serde(default)]
    pub vendor: Option<String>,
}

/// A value that's set and not blank (`application: ""`, as `ductus new` writes it, is unset).
pub fn given(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl System {
    pub fn new(application: &Option<String>, vendor: &Option<String>) -> Self {
        Self { application: given(application).map(Into::into), vendor: given(vendor).map(Into::into) }
    }

    /// Application or vendor is given.
    pub fn is_known(&self) -> bool {
        given(&self.application).is_some() || given(&self.vendor).is_some()
    }

    /// This, or `fallback` if this is unknown.
    pub fn or(self, fallback: &System) -> System {
        if self.is_known() { self } else { fallback.clone() }
    }

    /// `Sunquest Lab (Sunquest)`, `Sunquest Lab`, `(Sunquest)`, or empty.
    pub fn label(&self) -> String {
        match (given(&self.application), given(&self.vendor)) {
            (Some(a), Some(v)) if a.eq_ignore_ascii_case(v) => a.to_string(),
            (Some(a), Some(v)) => format!("{a} ({v})"),
            (Some(a), None) => a.to_string(),
            (None, Some(v)) => format!("({v})"),
            (None, None) => String::new(),
        }
    }
}

/// An IP address or subnet: `10.20.30.40`, `10.20.30.0/24`, `fd00::/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    pub address: IpAddr,
    pub prefix: u8,
}

impl Network {
    /// `None` when `text` isn't an address or subnet (a host name, say); an error when it looks
    /// like a subnet but isn't one.
    pub fn parse(text: &str) -> Result<Option<Self>> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text, None),
        };
        let Ok(address) = address.parse::<IpAddr>() else {
            if prefix.is_some() {
                bail!("`{text}` isn't a subnet (write it like 10.20.30.0/24)");
            }
            return Ok(None);
        };
        let max = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => match p.parse::<u8>() {
                Ok(p) if p <= max => p,
                _ => bail!("`{text}`: the prefix after / must be 0 to {max}"),
            },
        };
        Ok(Some(Self { address, prefix }))
    }

    pub fn from_ip(address: IpAddr) -> Self {
        Self { address, prefix: if address.is_ipv4() { 32 } else { 128 } }
    }

    /// Every address of `other` is in this one.
    pub fn contains(&self, other: &Network) -> bool {
        self.prefix <= other.prefix && self.overlaps(other)
    }

    /// Some address is in both (one contains the other, or they're the same).
    pub fn overlaps(&self, other: &Network) -> bool {
        let bits = self.prefix.min(other.prefix);
        match (self.address, other.address) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - u32::from(bits)) };
                u32::from(a) & mask == u32::from(b) & mask
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let mask = if bits == 0 { 0 } else { u128::MAX << (128 - u32::from(bits)) };
                u128::from(a) & mask == u128::from(b) & mask
            }
            _ => false,
        }
    }
}

/// What `ductus impact --host` looks for: an address, a subnet or a host name.
#[derive(Debug, Clone)]
pub struct HostQuery {
    text: String,
    network: Option<Network>,
}

impl HostQuery {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(Self { text: text.trim().to_string(), network: Network::parse(text)? })
    }

    /// Whether a configured host (address, subnet or name) is affected. Names match names
    /// (ignoring case); addresses and subnets match when they overlap. `resolve` looks a name
    /// up (in DNS, or not at all) so a name can match an address and the other way round.
    pub fn matches(&self, configured: &str, resolve: &dyn Fn(&str) -> Vec<IpAddr>) -> bool {
        let configured = configured.trim();
        if configured.eq_ignore_ascii_case(&self.text) {
            return true;
        }
        // Host names are case-insensitive.
        let lookup = |name: &str| resolve(&name.to_ascii_lowercase()).into_iter().map(Network::from_ip).collect();
        let theirs: Vec<Network> = match Network::parse(configured) {
            Ok(Some(n)) => vec![n],
            Ok(None) => lookup(configured),
            Err(_) => return false,
        };
        let ours: Vec<Network> = match self.network {
            Some(n) => vec![n],
            None => lookup(&self.text),
        };
        ours.iter().any(|a| theirs.iter().any(|b| a.overlaps(b)))
    }
}

impl std::fmt::Display for HostQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_dns(_: &str) -> Vec<IpAddr> {
        Vec::new()
    }

    #[test]
    fn matches_addresses_subnets_and_names() {
        let ip = HostQuery::parse("10.20.30.40").unwrap();
        assert!(ip.matches("10.20.30.40", &no_dns));
        assert!(ip.matches("10.20.30.0/24", &no_dns), "an address in a configured subnet");
        assert!(ip.matches("10.0.0.0/8", &no_dns));
        assert!(!ip.matches("10.20.31.0/24", &no_dns));
        assert!(!ip.matches("epic.local", &no_dns), "names need a lookup to match addresses");
        let dns = |name: &str| if name == "epic.local" { vec!["10.20.30.40".parse().unwrap()] } else { vec![] };
        assert!(ip.matches("epic.local", &dns));

        let subnet = HostQuery::parse("10.20.30.0/24").unwrap();
        assert!(subnet.matches("10.20.30.99", &no_dns));
        assert!(subnet.matches("10.20.0.0/16", &no_dns), "overlapping subnets");
        assert!(!subnet.matches("10.20.29.255", &no_dns));
        assert!(!subnet.matches("fd00::1", &no_dns));

        let name = HostQuery::parse("EPIC.local").unwrap();
        assert!(name.matches("epic.local", &no_dns), "names ignore case");
        assert!(name.matches("10.20.30.0/24", &dns), "a name's addresses match subnets");

        let v6 = HostQuery::parse("fd00::1").unwrap();
        assert!(v6.matches("fd00::/8", &no_dns) && !v6.matches("fe80::/10", &no_dns));
        assert!(HostQuery::parse("0.0.0.0/0").unwrap().matches("192.168.1.1", &no_dns));

        assert!(HostQuery::parse("10.1.2.0/33").is_err());
        assert!(HostQuery::parse("lab/24").is_err());
    }

    #[test]
    fn systems_and_labels() {
        let s = System::new(&Some("Sunquest Lab".into()), &Some(" ".into()));
        assert_eq!((s.label(), s.is_known()), ("Sunquest Lab".to_string(), true));
        assert_eq!(System::new(&Some("Epic".into()), &Some("epic".into())).label(), "Epic");
        assert_eq!(System::new(&None, &Some("Epic Systems".into())).label(), "(Epic Systems)");
        let unknown = System::new(&Some(String::new()), &None);
        assert!(!unknown.is_known());
        assert_eq!(unknown.or(&s), s);
    }
}
