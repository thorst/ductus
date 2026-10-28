//! The Windows service's account: what kind it is, told from its name alone, and what that means
//! for a Windows login to another machine. The installer applies the same rule in its conditions
//! (`packaging/windows/ductus.wxs`): keep the two in step.

/// The service's account when the installer is given none.
pub const DEFAULT: &str = r"NT SERVICE\Ductus";

/// What kind of account a service runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The computer's own: LocalSystem, Network Service, Local Service, or a virtual account
    /// (`NT SERVICE\Ductus`). No password; on the network it is the computer account (`HOST$`).
    Computer,
    /// A group managed service account (`HOSPITAL\svc-ductus$`): Windows keeps its password.
    Managed,
    /// A person-style account (`HOSPITAL\svc-ductus`, `.\ductus-svc`): it has a password, which
    /// the service manager must be given.
    Password,
}

impl Kind {
    pub fn of(account: &str) -> Kind {
        let a = account.trim().to_ascii_lowercase();
        let computer = ["localsystem", r"nt authority\system", r".\localsystem"].contains(&a.as_str())
            || a.starts_with(r"nt authority\")
            || a.starts_with(r"nt service\");
        if computer {
            Kind::Computer
        } else if a.ends_with('$') {
            Kind::Managed
        } else {
            Kind::Password
        }
    }

    /// Whether the service manager needs a password for it.
    pub fn needs_password(self) -> bool {
        self == Kind::Password
    }
}

/// How an account appears to another machine (a SQL Server's integrated login): the computer
/// account for the computer's own accounts, else itself.
pub fn on_the_network(account: &str, domain: &str, computer: &str) -> String {
    match Kind::of(account) {
        Kind::Computer => format!(r"{domain}\{computer}$"),
        _ => account.to_string(),
    }
}

/// The warning for SQL Servers reached with an integrated Windows login while the service runs as
/// one of the computer's own accounts, or None when there's nothing to say.
pub fn integrated_warning(account: &str, network_name: &str, databases: &[&str]) -> Option<String> {
    if databases.is_empty() || Kind::of(account) != Kind::Computer {
        return None;
    }
    let names = databases.iter().map(|d| format!("`{d}`")).collect::<Vec<_>>().join(", ");
    Some(format!(
        "the service runs as {account}, so its Windows login to SQL Server ({names}) is the computer account {network_name}: \
         every service on this computer running as one of its own accounts logs in as that too. \
         Give the service its own account (a gMSA: `ductus service account 'DOMAIN\\svc-ductus$'`) and a login for it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_told_apart_by_name() {
        for computer in
            [DEFAULT, "LocalSystem", r"NT AUTHORITY\NetworkService", r"NT AUTHORITY\LocalService", r"nt service\other"]
        {
            assert_eq!(Kind::of(computer), Kind::Computer, "{computer}");
        }
        assert_eq!(Kind::of(r"HOSPITAL\svc-ductus$"), Kind::Managed);
        for password in [r"HOSPITAL\svc-ductus", r".\ductus-svc", "svc@hospital.org"] {
            assert_eq!(Kind::of(password), Kind::Password, "{password}");
            assert!(Kind::of(password).needs_password());
        }
        assert!(!Kind::Managed.needs_password());
    }

    #[test]
    fn the_computer_s_own_accounts_are_the_computer_account_on_the_network() {
        assert_eq!(on_the_network(DEFAULT, "HOSPITAL", "SERVER01"), r"HOSPITAL\SERVER01$");
        assert_eq!(on_the_network(r"HOSPITAL\svc$", "HOSPITAL", "SERVER01"), r"HOSPITAL\svc$");
    }

    #[test]
    fn integrated_logins_warn_only_under_the_computer_s_own_accounts() {
        let w = integrated_warning(DEFAULT, r"HOSPITAL\SERVER01$", &["billing"]).unwrap();
        assert!(w.contains(r"is the computer account HOSPITAL\SERVER01$"), "{w}");
        assert!(w.contains("`billing`"), "{w}");
        assert_eq!(integrated_warning(r"HOSPITAL\svc$", "x", &["billing"]), None);
        assert_eq!(integrated_warning(DEFAULT, "x", &[]), None);
    }
}
