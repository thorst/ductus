//! Who may connect to a listener: `allow:` on MLLP and HTTP sources and on `api` in
//! ductus.yaml. A connection from anywhere else is closed before any TLS handshake or read.
//!
//! ```yaml
//! source: { mllp: { port: 6661, allow: [10.1.20.0/24, 10.1.30.15, epic-bridge01.hospital.org] } }
//! ```
//!
//! Entries are addresses, subnets or host names. Names are looked up forwards (name →
//! addresses) by the engine at start and every few minutes; never backwards, since a sender
//! controls the reverse record of its own address.

use crate::about::Network;
use anyhow::{Result, bail};
use serde::Deserialize;
use std::net::IpAddr;

/// An `allow:` list as written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Allow(pub Vec<String>);

/// An `allow:` list read: its addresses and subnets, and its host names (lower case).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub networks: Vec<Network>,
    pub names: Vec<String>,
}

impl Allow {
    /// The list read, or why it can't be.
    pub fn rules(&self) -> Result<Rules> {
        if self.0.is_empty() {
            bail!("`allow: []` lets no one connect; remove it to let anyone");
        }
        let mut rules = Rules::default();
        for entry in &self.0 {
            let entry = entry.trim();
            if entry.is_empty() {
                bail!("an `allow` entry is empty");
            }
            if let Some(network) = Network::parse(entry)? {
                rules.networks.push(network);
                continue;
            }
            if entry.contains('*') {
                bail!(
                    "`{entry}`: wildcards can't be checked (it would take a reverse lookup, which the sender controls); list the hosts or their subnet"
                );
            }
            if entry.contains('-') && entry.split('-').all(|part| part.trim().parse::<IpAddr>().is_ok()) {
                bail!("`{entry}`: ranges aren't supported; write the subnet (10.1.20.0/24) or the addresses");
            }
            let host_name = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_');
            if !entry.chars().all(host_name) || entry.starts_with(['-', '.']) {
                bail!("`{entry}` isn't an address, a subnet (10.1.20.0/24) or a host name");
            }
            let name = entry.to_ascii_lowercase();
            if !rules.names.contains(&name) {
                rules.names.push(name);
            }
        }
        Ok(rules)
    }

    /// The list for a table or a log line: `10.1.20.0/24, 10.1.30.15, epic-bridge01`.
    pub fn summary(&self) -> String {
        self.0.iter().map(|e| e.trim()).collect::<Vec<_>>().join(", ")
    }
}

/// The address a connection comes from as we compare it: an IPv4 sender on a `::` listener
/// shows as `::ffff:10.1.20.5`, which is 10.1.20.5.
pub fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

impl Rules {
    /// Whether `ip` may connect. `name_has` says whether a host name currently resolves to it.
    pub fn permits(&self, ip: IpAddr, name_has: impl Fn(&str, IpAddr) -> bool) -> bool {
        let ip = canonical(ip);
        let one = Network::from_ip(ip);
        self.networks.iter().any(|n| n.contains(&one)) || self.names.iter().any(|name| name_has(name, ip))
    }

    /// Whether every connection from `host` (an address, a subnet or a name, as in
    /// `about.from.hosts`) would be let in. `resolve` looks a name up (or returns nothing);
    /// `None` when it can't be told (a name that doesn't resolve).
    pub fn covers(&self, host: &str, resolve: &dyn Fn(&str) -> Vec<IpAddr>) -> Option<bool> {
        let host = host.trim();
        let theirs = match Network::parse(host) {
            Ok(Some(n)) => vec![n],
            Ok(None) if self.names.iter().any(|n| n.eq_ignore_ascii_case(host)) => return Some(true),
            Ok(None) => {
                let found: Vec<Network> =
                    resolve(&host.to_ascii_lowercase()).into_iter().map(Network::from_ip).collect();
                if found.is_empty() {
                    return None;
                }
                found
            }
            Err(_) => return None,
        };
        let ours: Vec<IpAddr> = self.names.iter().flat_map(|n| resolve(n)).map(canonical).collect();
        Some(theirs.iter().all(|t| {
            self.networks.iter().any(|n| n.contains(t))
                || (t.prefix == if t.address.is_ipv4() { 32 } else { 128 } && ours.contains(&t.address))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(entries: &[&str]) -> Allow {
        Allow(entries.iter().map(|e| e.to_string()).collect())
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn reads_addresses_subnets_and_names() {
        let rules = allow(&["10.1.20.0/24", " 10.1.30.15 ", "Epic-Bridge01.hospital.org", "fd00::/8"]).rules().unwrap();
        assert_eq!(rules.networks.len(), 3);
        assert_eq!(rules.names, ["epic-bridge01.hospital.org"], "names in lower case");
        assert_eq!(allow(&["a", "A"]).rules().unwrap().names, ["a"], "a name once");
    }

    #[test]
    fn refuses_what_it_cant_check() {
        let err = |entries: &[&str]| allow(entries).rules().unwrap_err().to_string();
        assert!(err(&[]).contains("lets no one connect"));
        assert!(err(&[" "]).contains("empty"));
        assert!(err(&["10.1.20.0/33"]).contains("prefix"));
        assert!(err(&["lab/24"]).contains("isn't a subnet"));
        assert!(err(&["*.hospital.org"]).contains("wildcards"));
        assert!(err(&["10.1.20.1-10.1.20.50"]).contains("ranges"));
        assert!(err(&["epic bridge"]).contains("isn't an address"));
        assert!(err(&["epic:6661"]).contains("isn't an address"), "no ports");
        assert!(err(&["http://epic"]).contains("isn't a subnet"));
        assert!(allow(&["epic-bridge01"]).rules().is_ok(), "a short name with a dash is a name");
    }

    #[test]
    fn permits_addresses_in_the_list_and_names_that_resolve_to_them() {
        let rules = allow(&["10.1.20.0/24", "10.1.30.15", "epic.local", "fd00::/8"]).rules().unwrap();
        let dns = |name: &str, ip: IpAddr| name == "epic.local" && ip == "10.1.30.22".parse::<IpAddr>().unwrap();
        assert!(rules.permits(ip("10.1.20.7"), dns));
        assert!(rules.permits(ip("10.1.30.15"), dns));
        assert!(!rules.permits(ip("10.1.30.16"), dns));
        assert!(rules.permits(ip("10.1.30.22"), dns), "through the name");
        assert!(rules.permits(ip("::ffff:10.1.20.7"), dns), "an IPv4 sender on a :: listener");
        assert!(rules.permits(ip("fd00::1"), dns));
        assert!(!rules.permits(ip("fe80::1"), dns));
        assert!(!rules.permits(ip("127.0.0.1"), dns), "loopback only when listed");
    }

    #[test]
    fn covers_the_hosts_about_names() {
        let rules = allow(&["10.1.20.0/24", "epic.local"]).rules().unwrap();
        let dns = |name: &str| match name {
            "epic.local" => vec![ip("10.1.30.22")],
            "lab.local" => vec![ip("10.1.20.9")],
            "far.local" => vec![ip("10.9.9.9")],
            _ => vec![],
        };
        assert_eq!(rules.covers("10.1.20.5", &dns), Some(true));
        assert_eq!(rules.covers("10.1.20.0/25", &dns), Some(true), "a smaller subnet inside");
        assert_eq!(rules.covers("10.1.0.0/16", &dns), Some(false), "a bigger subnet isn't all allowed");
        assert_eq!(rules.covers("EPIC.local", &dns), Some(true), "the same name");
        assert_eq!(rules.covers("10.1.30.22", &dns), Some(true), "an address a listed name has");
        assert_eq!(rules.covers("lab.local", &dns), Some(true), "a name inside the subnet");
        assert_eq!(rules.covers("far.local", &dns), Some(false));
        assert_eq!(rules.covers("gone.local", &dns), None, "can't tell");
    }
}
