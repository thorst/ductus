//! The Windows service: running as one (`run.rs`), what it writes to the Event Log (`eventlog.rs`,
//! with its text in `messages.rs`), and its account (`account.rs`, `ductus service account`).

#[cfg(windows)]
pub mod account;
#[cfg(windows)]
pub mod eventlog;
// The table itself is used by build.rs and the tests; the code uses the IDs (on Windows).
#[allow(dead_code)]
pub mod messages;
#[cfg(windows)]
mod run;

#[cfg(windows)]
pub use run::run;

/// The service's name (and the Event Log source's).
#[cfg(windows)]
pub const NAME: &str = "Ductus";
/// The file in the log folder that says the service was stopped on purpose (the installer reads it).
#[cfg(windows)]
pub const STOPPED_MARKER: &str = "stopped";

/// `%ProgramData%\Ductus`: settings, project and logs.
#[cfg(windows)]
pub fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into())).join("Ductus")
}

/// Who is running this command, for the Event Log.
#[cfg(windows)]
pub fn who() -> String {
    match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!(r"{d}\{u}"),
        (_, Ok(u)) => u,
        _ => "unknown".into(),
    }
}

/// What `validate` and `doctor` say about the service's account (Windows only): who it runs as,
/// and the warning when SQL Servers reached with an integrated login would see the computer
/// account. `doctor` also says its own integrated logins are the person's running it.
#[cfg(windows)]
pub fn notes(resources: Option<&ductus_core::Resources>, doctor: bool) -> Vec<String> {
    use ductus_core::service_account::{Kind, integrated_warning};
    let integrated = resources.map(|r| r.integrated_sqlserver()).unwrap_or_default();
    let mut notes = Vec::new();
    let account = match account::current() {
        Ok(Some(a)) => a,
        Ok(None) => return notes, // no service here: a developer's machine
        Err(e) => {
            notes.push(format!("⚠ service: couldn't read its account: {e:#}"));
            return notes;
        }
    };
    let network = match Kind::of(&account) {
        Kind::Computer => account::network_name().unwrap_or_else(|| "nobody (this computer is in no domain)".into()),
        _ => account.clone(),
    };
    if Kind::of(&account) == Kind::Computer {
        notes.push(format!("✓ service: runs as {account} (on the network: {network})"));
    } else {
        notes.push(format!("✓ service: runs as {account}"));
    }
    if let Some(warning) = integrated_warning(&account, &network, &integrated) {
        notes.push(format!("⚠ {warning}"));
    }
    if doctor && !integrated.is_empty() {
        let names = integrated.iter().map(|d| format!("`{d}`")).collect::<Vec<_>>().join(", ");
        notes.push(format!(
            "note: doctor's Windows logins to {names} are yours ({}), not the service's ({network}): a login that works here may not for the service",
            who()
        ));
    }
    notes
}

#[cfg(not(windows))]
pub fn notes(_: Option<&ductus_core::Resources>, _: bool) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::messages::{MESSAGES, message_table};

    /// Read a message back the way Windows does (`FormatMessage` over the resource).
    fn find(table: &[u8], id: u32) -> Option<String> {
        let u32_at = |at: usize| u32::from_le_bytes(table[at..at + 4].try_into().unwrap());
        let u16_at = |at: usize| u16::from_le_bytes(table[at..at + 2].try_into().unwrap());
        for block in 0..u32_at(0) as usize {
            let (low, high, mut at) =
                (u32_at(4 + 12 * block), u32_at(8 + 12 * block), u32_at(12 + 12 * block) as usize);
            if !(low..=high).contains(&id) {
                continue;
            }
            for _ in low..id {
                at += u16_at(at) as usize;
            }
            let (length, flags) = (u16_at(at) as usize, u16_at(at + 2));
            assert_eq!(flags, 1, "UTF-16");
            assert_eq!(length % 4, 0, "entries are four-byte aligned");
            let units: Vec<u16> =
                table[at + 4..at + length].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let text = String::from_utf16(&units).unwrap();
            return Some(text.trim_end_matches('\0').to_string());
        }
        None
    }

    #[test]
    fn every_message_is_found_by_its_id() {
        let table = message_table(MESSAGES);
        for (id, text) in MESSAGES {
            assert_eq!(find(&table, *id).as_deref(), Some(format!("{text}\r\n").as_str()), "{id}");
        }
        assert_eq!(find(&table, 999), None);
        assert_eq!(find(&table, 1008), None);
        let mut ids: Vec<u32> = MESSAGES.iter().map(|(id, _)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), MESSAGES.len(), "one message per ID");
    }
}
