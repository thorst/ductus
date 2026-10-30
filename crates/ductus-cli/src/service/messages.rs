// The Event Log's messages: what Event Viewer shows for each event ID the service writes. build.rs
// compiles this table into ductus.exe (a MESSAGETABLE resource), which the installer registers as
// the "Ductus" source's message file; `eventlog.rs` writes the IDs. `%1`, `%2` are the strings an
// event carries, `%n` a line break. Plain items only: build.rs `include!`s this file.

/// Started: `%1` the version, `%2` the details.
pub const STARTED: u32 = 1000;
pub const STOPPED: u32 = 1001;
pub const STOPPED_FORCED: u32 = 1002;
pub const START_FAILED: u32 = 1003;
pub const PREVIOUS_RUN_UNCLEAN: u32 = 1004;
/// `%1` the account.
pub const NEEDS_PASSWORD: u32 = 1005;
pub const ENDED_ON_ERROR: u32 = 1006;
pub const ACCOUNT_CHANGED: u32 = 1007;
pub const INTERFACE_STOPPED: u32 = 1010;
pub const CRITICAL_ALERT: u32 = 1020;
pub const ERROR: u32 = 1100;

pub const MESSAGES: &[(u32, &str)] = &[
    (STARTED, "Ductus %1 started.%n%n%2"),
    (STOPPED, "Ductus stopped cleanly: it finished the messages in hand.%n%n%1"),
    (
        STOPPED_FORCED,
        "Ductus stopped before it finished the messages in hand: they stay pending and are processed again at the next start (a destination may get one again).%n%n%1",
    ),
    (
        START_FAILED,
        "Ductus could not start. The service stays stopped until the cause is fixed and it is started again.%n%n%1",
    ),
    (
        PREVIOUS_RUN_UNCLEAN,
        "The last run of Ductus on this computer did not stop cleanly (it was killed, crashed or lost power). The messages it had in hand are processed again, as the delivery guarantee says.%n%n%1",
    ),
    (
        NEEDS_PASSWORD,
        "The Ductus service runs as %1, an account with a password, which an install or upgrade can't keep: the service was left stopped. As an administrator, run%n%n    ductus service account %1 --start%n%nand type its password when asked (a script passes it with --password-stdin). A gMSA (an account ending in $) needs none of this: Windows keeps its password.",
    ),
    (ENDED_ON_ERROR, "Ductus stopped on an error.%n%n%1"),
    (ACCOUNT_CHANGED, "The Ductus service's account was changed.%n%n%1"),
    (
        INTERFACE_STOPPED,
        "An interface was stopped: its code ran away. Its messages wait until the code is fixed and the engine is restarted.%n%n%1",
    ),
    (CRITICAL_ALERT, "A critical alert fired.%n%n%1"),
    (ERROR, "Ductus logged an error.%n%n%1"),
];

/// Each event for the reference page (`ductus reference`): its level in the Event Log, and when
/// it's written and what to do, for whoever watches the log (SCOM, Splunk, a person).
pub const ABOUT: &[(u32, &str, &str)] = &[
    (
        STARTED,
        "Information",
        "The engine started and is taking messages: its version, and the start's details (interfaces, where the API listens).",
    ),
    (
        STOPPED,
        "Information",
        "The service stopped cleanly (a stop, a shutdown, or a drain that ended with a stop): every message in hand was finished first.",
    ),
    (
        STOPPED_FORCED,
        "Warning",
        "The service stopped before finishing the messages in hand (the stop timeout passed, or a second stop came). Nothing is lost: they're pending and are processed again at the next start, so a destination may get one twice. If it recurs, look at what was slow (a receiver timing out) in the log files.",
    ),
    (
        START_FAILED,
        "Error",
        "The engine couldn't start, and the service stays stopped: the text says why (the database can't be reached, the project has an error, a certificate can't be used, another engine runs some of these interfaces). Fix the cause and start the service.",
    ),
    (
        PREVIOUS_RUN_UNCLEAN,
        "Warning",
        "The previous run didn't stop cleanly (killed, crashed, or the machine lost power). The messages it had in hand are processed again; the previous run's last lines are in the log files.",
    ),
    (
        NEEDS_PASSWORD,
        "Warning",
        "The service runs as an account with a password, which an install or upgrade can't keep, so it was left stopped. Run `ductus service account <account> --start` as an administrator and type the password; a gMSA needs none of this.",
    ),
    (ENDED_ON_ERROR, "Error", "The engine stopped on an error while running: the text says which."),
    (
        ACCOUNT_CHANGED,
        "Information",
        "The service's account was changed with `ductus service account`: from which, to which, and by whom.",
    ),
    (
        INTERFACE_STOPPED,
        "Error",
        "An interface's code was stopped: it ran past its `timeout` three times in a row, or out of memory. Its messages wait; fix the code, deploy it and restart the service.",
    ),
    (
        CRITICAL_ALERT,
        "Error",
        "A critical alert fired or escalated (the alert channels are told too), for tools that watch the Event Log.",
    ),
    (
        ERROR,
        "Error",
        "Any other error the engine logged, at most once a minute for each kind of message and what it's about; the log files have every line.",
    ),
];

/// The table as a MESSAGETABLE resource's data (`MESSAGE_RESOURCE_DATA`): blocks of consecutive
/// IDs, then each message as UTF-16 ending in CRLF and a NUL, padded to four bytes.
pub fn message_table(messages: &[(u32, &str)]) -> Vec<u8> {
    let mut sorted: Vec<(u32, &str)> = messages.to_vec();
    sorted.sort_by_key(|(id, _)| *id);
    let mut blocks: Vec<Vec<(u32, &str)>> = Vec::new();
    for m in sorted {
        match blocks.last_mut() {
            Some(b) if b.last().map(|l| l.0 + 1) == Some(m.0) => b.push(m),
            _ => blocks.push(vec![m]),
        }
    }
    let entry = |text: &str| -> Vec<u8> {
        let mut bytes: Vec<u8> = format!("{text}\r\n").encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        let mut e = Vec::with_capacity(bytes.len() + 4);
        e.extend(((bytes.len() + 4) as u16).to_le_bytes());
        e.extend(1u16.to_le_bytes()); // MESSAGE_RESOURCE_UNICODE
        e.extend(bytes);
        e
    };
    let mut out = (blocks.len() as u32).to_le_bytes().to_vec();
    let mut offset = 4 + 12 * blocks.len();
    let mut entries = Vec::new();
    for b in &blocks {
        out.extend(b[0].0.to_le_bytes());
        out.extend(b[b.len() - 1].0.to_le_bytes());
        out.extend((offset as u32).to_le_bytes());
        for (_, text) in b {
            let e = entry(text);
            offset += e.len();
            entries.extend(e);
        }
    }
    out.extend(entries);
    out
}
