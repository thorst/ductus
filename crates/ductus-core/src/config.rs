//! `interface.yaml`: everything about an interface except its logic.

use crate::about::{About, System};
use crate::format::Format;
use crate::resources::Kind;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `interface.yaml`: one interface's settings. Where its messages come from (`source`), how
/// they're read (`format`), where they go (`destination`, or `destinations` for several), and how
/// they're delivered, kept, watched and documented. Its code is in the `script` beside it.
///
/// Each interface has a folder of its own under `interfaces/`, holding this file, its code, its
/// tests and sample messages. Only `name`, `source` and a destination must be given; everything
/// else has a default that's safe to start with.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceConfig {
    /// The interface's name, as the database, the web UI, logs, alerts and routes
    /// (`destination: { interface: <name> }`) know it. Use the folder's name, as `ductus new
    /// interface` does, and keep it short and stable: lower-case words joined by `-`.
    ///
    /// Messages are stored under the name, so a renamed interface is a new one: the old name's
    /// history stays under the old name, and messages still waiting there are sent by nobody.
    /// Rename only when nothing is waiting. Required. Example: `name: adt-epic-to-lab`.
    pub name: String,
    /// Where messages come from: exactly one of `mllp`, `http`, `file`, `sftp`, `schedule` or
    /// `internal`, each with its own settings. Required. Example: `source: { mllp: { port: 6661
    /// } }`.
    #[serde(with = "serde_yaml_ng::with::singleton_map")]
    #[cfg_attr(test, schemars(with = "Source"))]
    pub source: Source,
    /// The TypeScript file with the interface's code, relative to its folder. It exports
    /// `transform(msg, meta)`, plus any function a destination names with `transform:`.
    ///
    /// `ductus validate` type-checks it and the engine loads it at start. Without a script,
    /// messages go to the destinations as they came. A `schedule` source and an HTTP source in
    /// `respond` mode need one: there, the code makes the message or the answer. Example:
    /// `script: transform.ts`.
    #[serde(default)]
    pub script: Option<PathBuf>,
    /// What the messages are, so the code gets them parsed and keys can be read: `hl7`, `json`,
    /// `xml`, `text`, `csv` or `fixed` (fixed-length records, with their fields' widths).
    ///
    /// `hl7` gives the code segments and fields (`msg.get("PID-3.1")`); `json` and `xml` a
    /// document; `csv` rows (objects by column name with a header line); `fixed` an object per
    /// record; `text` the payload as a string. An MLLP source always carries HL7. What the code
    /// returns is written in this format too, unless `output` says otherwise. Default: `hl7`.
    /// Example: `format: { csv: { delimiter: "|" } }`.
    #[serde(default, deserialize_with = "crate::format::deserialize")]
    pub format: Format,
    /// The format the code's results are written in, when it isn't `format`: an HL7 feed turned
    /// into JSON for an API, a CSV file turned into HL7. Same values as `format`. A destination
    /// can set its own `output`. Default: the same as `format`. Example: `output: json`.
    #[serde(default, deserialize_with = "crate::format::deserialize_opt")]
    pub output: Option<Format>,
    /// Which messages must be delivered in the order they arrived: those with the same `key`
    /// (one patient's), in lanes spread over the workers. Without it, the whole interface is one
    /// lane, delivered strictly in arrival order. Default: no key (one lane). Example:
    /// `ordering: { key: PID-3.1 }`.
    #[serde(default)]
    pub ordering: Ordering,
    /// Where the interface's code runs.
    ///
    /// - `shared`: on the engine's shared pool, one thread per CPU core, each running many
    ///   interfaces. Cheap: most interfaces belong here.
    /// - `dedicated`: on threads of its own (`workers` of them, each with its own copy of the
    ///   JavaScript engine), so its load and its runaways touch no other interface, and it can
    ///   have database connections of its own (`dedicated_pools`).
    ///
    /// Use `dedicated` for a busy interface that needs more than one worker (with an ordering
    /// `key`, so there are lanes to share out), or for heavy or untrusted code. Each dedicated
    /// worker costs a thread and up to `isolates.heap_mb` of memory. Default: `shared`.
    /// Example: `isolation: dedicated`.
    #[serde(default)]
    pub isolation: Isolation,
    /// How many workers a dedicated interface has, each a thread with its own JavaScript engine,
    /// taking a share of the lanes (`ordering.partitions`). Only with `isolation: dedicated`.
    ///
    /// More workers help only with an ordering `key` (without one there's a single lane, and
    /// one worker does everything). Up to about the number of cores helps; past that they wait
    /// for each other. Default: `4`. Example: `workers: 8`.
    #[serde(default)]
    pub workers: Option<u32>,
    /// Database connections of the interface's own, by resources.yaml database name: `{
    /// patient_db: 4 }` gives it 4 connections to `patient_db` that no other interface uses.
    /// Only with `isolation: dedicated`.
    ///
    /// Its `db("patient_db")` calls use these first, so a busy interface can't take every
    /// connection of the shared pool (`max_connections`) from the others, nor theirs from it.
    /// Add them when a dedicated interface's lookups queue for connections, or when it must keep
    /// working while other interfaces are busy. Default: none. Example: `dedicated_pools: {
    /// patient_db: 4 }`.
    #[serde(default)]
    pub dedicated_pools: BTreeMap<String, u32>,
    /// When a sender gets its acknowledgement (ACK, or HTTP 202): after the message is safely on
    /// the database's disk, or as soon as the database has it.
    ///
    /// - `strict`: the ACK waits until the database has flushed the message to disk. Once a
    ///   sender has its ACK, the message survives a crash or power loss of any machine.
    /// - `relaxed`: the ACK goes out without waiting for the flush. One sender connection can go
    ///   much faster: with `strict` it can't send faster than one message per disk flush (about
    ///   400 a second at 2.4 ms a flush; `ductus doctor` measures yours). The cost: if the
    ///   *database server* crashes or loses power, the last fraction of a second of acknowledged
    ///   messages is lost, and the sender won't resend them because it has its ACK. An engine
    ///   crash alone loses nothing in either mode.
    ///
    /// Use `relaxed` only where one connection's peak is more than the disk allows and a lost
    /// message can be recovered another way (a resend from the source system, a nightly
    /// reconciliation). Before that, try a faster disk or more sender connections, which share
    /// flushes. Default: `strict`. Example: `durability: relaxed`.
    #[serde(default)]
    pub durability: Durability,
    /// Where messages go, when they go to one place: one connector (`mllp`, `file`, `sftp`,
    /// `http` or `interface`) and its options. Its name is `default` (in the web UI, alerts and
    /// `ductus pause adt-in default`).
    ///
    /// Use `destinations` instead to send to several places. An HTTP source in `respond` mode has
    /// none: its answer is the result. Required otherwise (this or `destinations`). Example:
    /// `destination: { mllp: { host: lab.hospital.org, port: 7001 } }`.
    #[serde(default)]
    pub destination: Option<DestinationConfig>,
    /// Several places messages go, by a name you choose (lower-case letters, digits, `-` and `_`).
    /// Each gets every message and is delivered independently: its own queue, order, retries and
    /// pause, so one that's down holds up only itself. Each runs its own function on the
    /// original message (`transform:`, default `transform`).
    ///
    /// A message is finished once every destination is done with it. Adding a destination later
    /// gives it only the messages still in flight, not older ones; removing one filters what was
    /// waiting for it. Use instead of `destination`, not with it. Example: `destinations: { lab:
    /// { mllp: { host: lab.hospital.org, port: 7001 } }, archive: { file: { dir: archive } } }`.
    #[serde(default)]
    pub destinations: BTreeMap<String, DestinationConfig>,
    /// How a delivery that fails for a reason that may pass (a receiver down, a timeout, a
    /// database unreachable from the code) is tried again: waits doubling from
    /// `initial_backoff_ms` to `max_backoff_ms`, and given up only after `max_attempts`, if set.
    /// A destination can set its own. Default: retry forever, 1 s doubling to 60 s.
    #[serde(default)]
    pub retry: Retry,
    /// Recognize a message sent again within a window (a sender resending after a lost ACK, a
    /// file dropped twice) by the values of `keys`, and don't deliver it twice.
    ///
    /// A repeat is stored with the status `duplicate` (so it can be found) but never queued. Use
    /// it when the receiver can't tolerate repeats and the sender is known to resend; most HL7
    /// receivers already ignore a repeated control ID (MSH-10). Default: off. Example: `dedupe:
    /// { keys: [MSH-10], window: 24h }`.
    #[serde(default)]
    pub dedupe: Option<Dedupe>,
    /// The most messages a minute each destination is sent, for receivers that can't take
    /// bursts (a device, a vendor API with a quota). Messages over it wait, in order, or are
    /// dropped (`over_limit: drop`). A destination can set its own; either way each destination
    /// has its own allowance. Default: no limit. Example: `rate_limit: { per_minute: 600 }`.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
    /// The longest the interface's code may run for one message without pausing, from `10ms` to
    /// `10m`. Time spent waiting on `db()`, `fetch()` or `api()` doesn't count, only running code.
    ///
    /// It's the guard against code that never ends, such as a loop over segments that an unusual
    /// message never lets finish. Past it, the code is stopped, its message is quarantined
    /// ("transform stopped: …") and the interface carries on; three in a row, or one that runs
    /// out of memory, stop the interface until the engine restarts. Interfaces in the shared
    /// pool wait while code on their thread runs, so a generous limit lets one runaway hold them
    /// all up that long.
    ///
    /// Set it close to what the code needs: `500ms` is plenty for most HL7 transforms. Raise it
    /// only for code that computes for seconds on purpose, and give that interface `isolation:
    /// dedicated` so no one else waits for it. Default: `5s`. Example: `timeout: 500ms`.
    #[serde(default = "defaults::timeout", deserialize_with = "duration")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::Duration"))]
    pub timeout: Duration,
    /// What the interface is for, who owns it, and the systems on each side. `ductus impact`
    /// reads it to answer "which interfaces does this vendor's outage, breach or VPN change
    /// touch?", and the web UI and alerts show it. Nothing here changes how messages flow.
    /// Default: empty (`require_about: true` in ductus.yaml makes `ductus validate` insist).
    /// Example: `about: { description: ADT from Epic to the lab, owner: lab-it@hospital.org }`.
    #[serde(default)]
    pub about: About,
    /// How long this interface's messages are kept: `1w`, `3w` or `7w` (or `7d`, `21d`, `49d`),
    /// when it should differ from `retention` in ductus.yaml.
    ///
    /// A message is kept at least its window and is gone within a day (`1w`) or a week after.
    /// Waiting and quarantined messages are never deleted: past the window they move to the held
    /// area. A shorter value deletes messages, so it waits for `ductus retention confirm
    /// <interface>`; a longer one applies at once. Use `1w` for busy feeds nobody looks back on,
    /// `7w` where problems surface weeks later. Not for HTTP `respond` mode (it keeps nothing).
    /// Default: `retention` in ductus.yaml (`7w`). Example: `retention: 1w`.
    #[serde(default)]
    pub retention: Option<crate::Retention>,
    /// What happens to quarantined messages once the retention window has passed: `keep` them in
    /// the held area until someone resends, corrects or dismisses them, or `purge` them with
    /// the rest. A quarantined message is a question nobody has answered yet, so `keep` is the
    /// safe choice; `purge` suits a feed whose sender resends anyway. Not for HTTP `respond`
    /// mode. Default: `quarantined` in ductus.yaml (`keep`). Example: `quarantined: purge`.
    #[serde(default)]
    pub quarantined: Option<crate::Quarantined>,
    /// What operations expects of this interface, in the terms its teams use: something
    /// received within a time (`active_within`) or by a deadline (`active_by`), and queues not
    /// too far behind (`behind`). Breaking one shows as a health state (silent, missed, behind)
    /// in the web UI, `ductus status` and Prometheus, and alerts when a rule names it.
    ///
    /// Nothing is expected by default: the engine can't know that a quiet feed is a problem, so
    /// these are operations' decisions. Example: `monitor: { active_within: 30m, between:
    /// "06:00-22:00", behind: 10m }`.
    #[serde(default)]
    pub monitor: crate::monitor::Monitor,
    /// This interface's alerts: more recipients on top of the rules' (`to`), or none at all
    /// (`mute`). The rules in ductus.yaml decide which states alert. Default: the rules' recipients
    /// only. Example: `alert: { to: [lab-team] }`.
    #[serde(default)]
    pub alert: crate::alerts::AlertOptions,
    /// Your company's own documentation fields (a BAA reference, a ticket, a runbook link),
    /// declared in `docs-schema.yaml` and checked by `ductus validate`; the web UI shows them on
    /// the interface's page. The engine itself ignores them. Default: none. Example: `custom: {
    /// baa_signed: "yes", runbook: "https://wiki/adt" }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,

    /// Folder the config was loaded from.
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    /// HL7 v2 over MLLP: the engine listens on `port`, and senders (an EHR, a lab system) connect
    /// and send. Each message is stored before its ACK goes back, so a sender that has its ACK
    /// never has to send it again. Example: `source: { mllp: { port: 6661 } }`.
    Mllp {
        /// The address to listen on: `0.0.0.0` (every IPv4 address of the server), `::` (every
        /// address, IPv6 too), one of the server's addresses, or `127.0.0.1` (this machine only,
        /// for testing). Default: `0.0.0.0`. Example: `host: 10.20.0.15`.
        #[serde(default = "defaults::host")]
        host: String,
        /// The port senders connect to. Each MLLP source needs a port of its own; `ductus new
        /// interface` picks a free one from `ports.mllp` in ductus.yaml, and `ductus validate`
        /// reports two interfaces on one port. Required. Example: `port: 6661`.
        port: u16,
        /// Accept only TLS on this port: the engine presents the `certificate` named here, and
        /// with `clients`, senders must present one too. Senders on plain MLLP can't connect
        /// then, so agree on a date with them. Default: plain MLLP. Example: `tls: { certificate:
        /// ductus-mllp }`.
        #[serde(default)]
        tls: Option<crate::tls::ListenTls>,
        /// Who may connect: addresses (`10.1.30.15`), subnets (`10.1.20.0/24`) and host names
        /// (looked up at start and every 5 minutes). Anyone else is disconnected before a byte is
        /// read and logged once a minute per address. A second line of defence beside the
        /// firewall, and the way to say "only the lab's interface engine". Default: anyone who
        /// can reach the port. Example: `allow: [10.1.20.0/24, epic-bridge01.hospital.org]`.
        #[serde(default)]
        allow: Option<crate::allow::Allow>,
    },
    /// An HTTP endpoint: the engine listens on `port` for requests to `path`. In `queue` mode the
    /// body is stored as a message and answered `202 Accepted`; in `respond` mode the code
    /// answers the request itself (a REST API, a lookup). Several HTTP interfaces can share a
    /// port with different paths, if they have the same `tls` and `allow`. Example: `source: {
    /// http: { port: 8080, path: /orders } }`.
    Http {
        /// The address to listen on: `0.0.0.0` (every IPv4 address of the server), `::` (every
        /// address, IPv6 too), one of the server's addresses, or `127.0.0.1` (this machine only).
        /// Default: `0.0.0.0`. Example: `host: 10.20.0.15`.
        #[serde(default = "defaults::host")]
        host: String,
        /// The port to listen on. HTTP interfaces may share one (each with its own path);
        /// `ductus new interface` picks a free one from `ports.http` in ductus.yaml. Required.
        /// Example: `port: 8080`.
        port: u16,
        /// HTTPS only, presenting the `certificate` named here; with `clients`, callers must
        /// present one too. Interfaces sharing a port must name the same. Default: plain HTTP.
        /// Example: `tls: { certificate: ductus-api }`.
        #[serde(default)]
        tls: Option<crate::tls::ListenTls>,
        /// Who may connect: addresses, subnets and host names, as for MLLP. Checked before the
        /// request is read, so interfaces sharing a port must have the same list. Default:
        /// anyone who can reach the port. Example: `allow: [10.1.20.0/24]`.
        #[serde(default)]
        allow: Option<crate::allow::Allow>,
        /// The path requests come to, starting with `/`. `{name}` parts match one path segment
        /// and are given to the code (`/patients/{mrn}` takes `/patients/12345`). Two interfaces
        /// on one port can't have the same method and path. Required. Example: `path:
        /// /patients/{mrn}`.
        path: String,
        /// The HTTP method this interface answers: `GET`, `POST`, `PUT`, `PATCH` or `DELETE`.
        /// Default: `POST`. Example: `method: GET`.
        #[serde(default = "defaults::post")]
        method: String,
        /// - `queue`: store the request body as a message, answer `202 Accepted` once it's
        ///   stored, and deliver it like any other message. For senders that push data.
        /// - `respond`: run the code on the request and send its result back as the response,
        ///   at once. Nothing is stored or queued, and there's no destination; if something
        ///   fails, the caller gets an error status and decides whether to try again. For REST
        ///   APIs and lookups that other systems call and wait on.
        ///
        /// Default: `queue`. Example: `mode: respond`.
        #[serde(default)]
        mode: HttpMode,
    },
    /// Files dropped in a folder on this server (or a share it mounts): each file, or each record
    /// in it with `split: records`, becomes a message. A file is moved or deleted only after
    /// everything in it is stored. Example: `source: { file: { dir: in, pattern: "*.hl7" } }`.
    File(FileSource),
    /// Files on an SFTP server, named by `server` (an `sftp:` entry in resources.yaml); the same
    /// options as `file`. The engine logs in, picks up what's ready, and moves or deletes it on
    /// the server. Example: `source: { sftp: { server: lab_server, dir: outbound } }`.
    Sftp(FileSource),
    /// No input: the code runs on a timer (`every` or `cron`) and what it returns becomes the
    /// message, for jobs that pull (a nightly query, an API polled every few minutes). A run is
    /// skipped while the previous one is still waiting. Needs a `script`. Example: `source: {
    /// schedule: { cron: "0 2 * * *" } }`.
    Schedule(Schedule),
    /// Messages routed from other interfaces (`destination: { interface: <this one> }`), with no
    /// listener of its own. Each is stored in the same transaction that marks it sent at the
    /// other interface, so nothing is lost or doubled between them. Use it to split a busy feed
    /// or to share one feed's preparation among several. Example: `source: internal`.
    Internal,
}

impl Source {
    /// What kind of source it is, as written in interface.yaml.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Mllp { .. } => "mllp",
            Source::Http { .. } => "http",
            Source::File(_) => "file",
            Source::Sftp(_) => "sftp",
            Source::Schedule(_) => "schedule",
            Source::Internal => "internal",
        }
    }

    /// Who may connect, for a listening source with an `allow:` list.
    pub fn allow(&self) -> Option<&crate::allow::Allow> {
        match self {
            Source::Mllp { allow, .. } | Source::Http { allow, .. } => allow.as_ref(),
            _ => None,
        }
    }
}

/// Watch a folder for files. Each file (or each record in it, with `split: records`) becomes a
/// message. A file is moved or deleted only after everything in it is stored.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileSource {
    /// The SFTP server to read from: an `sftp:` entry in resources.yaml, which holds its address
    /// and login. Only for `sftp` sources, and required there. Example: `server: lab_server`.
    #[serde(default)]
    pub server: Option<String>,
    /// The folder to watch. A local folder is relative to the interface's folder unless written
    /// whole (`/data/inbound/lab`, or on Windows `D:\inbound\lab`, in single quotes); an SFTP
    /// folder is relative to the folder the login starts in. Only files directly in it are read,
    /// not subfolders. Required. Example: `dir: inbound`.
    pub dir: String,
    /// Which file names to pick up, with `*` (any characters) and `?` (one character). Names
    /// starting with `.` are never picked up, since writers often use them for files still being
    /// written. Default: `*` (every file). Example: `pattern: "ADT_*.hl7"`.
    #[serde(default = "defaults::pattern")]
    pub pattern: String,
    /// How often the folder is looked at, at least `100ms`. A file is picked up once its size
    /// and time are the same at two looks in a row, so a file still being written is never read
    /// half-done: a file appears one to two polls after its writer finishes. Lower it for quicker
    /// pickup; on SFTP each look is a listing on the server. Default: `10s`. Example: `poll:
    /// 30s`.
    #[serde(default = "defaults::poll", deserialize_with = "duration")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::Duration"))]
    pub poll: Duration,
    /// Pick a file up only once a marker file next to it exists: with `marker: .done`, `a.csv`
    /// is read once `a.csv.done` appears. For writers that say when they're finished (many
    /// mainframe and batch jobs do); the file is then read at the next look, without waiting for
    /// it to stop changing. The marker goes with the file. Default: none (wait until the file
    /// stops changing). Example: `marker: .done`.
    #[serde(default)]
    pub marker: Option<String>,
    /// What happens to a file once everything in it is stored: `move` it to `archive_dir`, or
    /// `delete` it. Moving keeps a copy to look at or drop in again; the engine never deletes
    /// what it moved, so clean the archive yourself. Default: `move`. Example: `after: delete`.
    #[serde(default)]
    pub after: After,
    /// Where `after: move` puts files, relative to the interface's folder (or the SFTP login
    /// folder) unless written whole. Default: `archive` inside `dir`. Example: `archive_dir:
    /// /data/archive/lab`.
    #[serde(default)]
    pub archive_dir: Option<String>,
    /// - `none`: each file is one message.
    /// - `records`: each record in a file is a message, read as a stream, so a file of any size
    ///   works: an HL7 batch file is split at each MSH, CSV at each row (the header line kept
    ///   with every row), fixed-length, text and JSON Lines at each line. If the engine stops
    ///   halfway, it carries on from the last record stored.
    ///
    /// A file as one message is limited to 256 MB; split bigger ones. XML files are always one
    /// message each. Default: `none`. Example: `split: records`.
    #[serde(default)]
    pub split: Split,
}

impl FileSource {
    /// Where processed files go (`after: move`).
    pub fn archive(&self) -> String {
        self.archive_dir.clone().unwrap_or_else(|| format!("{}/archive", self.dir.trim_end_matches('/')))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum After {
    #[default]
    Move,
    Delete,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Split {
    #[default]
    None,
    Records,
}

/// When a schedule source runs: `every: 15m`, or `cron: "0 2 * * *"` (server local time).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// Run at start, then at this interval: for polling (an API every `5m`). Give this or
    /// `cron`, not both. Example: `every: 15m`.
    #[serde(default, deserialize_with = "duration_opt")]
    #[cfg_attr(test, schemars(with = "Option<crate::schema::shapes::Duration>"))]
    pub every: Option<Duration>,
    /// Run at the times a five-field cron expression gives (minute, hour, day of month, month,
    /// day of week), in the server's local time: for jobs at a time of day. Runs missed while
    /// the engine was down are made up by one run at start, not one per missed time. Give this
    /// or `every`. Example: `cron: "0 2 * * *"` (02:00 every day).
    #[serde(default)]
    pub cron: Option<String>,
}

/// Problems with how a set of interfaces share network ports: two MLLP listeners on one port,
/// MLLP and HTTP on one port, two interfaces handling the same HTTP method and path, or HTTP
/// interfaces on one port with different `tls`. (Several HTTP interfaces may share a port with
/// different routes: they share one server.)
pub fn check_endpoints<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<String> {
    use std::collections::HashMap;
    let mut problems = Vec::new();
    let mut mllp: HashMap<u16, &str> = HashMap::new();
    let mut http_ports: HashMap<u16, &str> = HashMap::new();
    let mut http_tls: HashMap<u16, (&str, &Option<crate::tls::ListenTls>)> = HashMap::new();
    let mut http_allow: HashMap<u16, (&str, &Option<crate::allow::Allow>)> = HashMap::new();
    let mut routes: HashMap<(u16, String, String), &str> = HashMap::new();
    for c in configs {
        match &c.source {
            Source::Mllp { port, .. } => {
                let first = *mllp.entry(*port).or_insert(&c.name);
                if first != c.name {
                    problems.push(format!("port {port}: MLLP listeners for both `{first}` and `{}`", c.name));
                }
            }
            Source::Http { port, method, path, tls, allow, .. } => {
                http_ports.entry(*port).or_insert(&c.name);
                let (first, first_tls) = *http_tls.entry(*port).or_insert((&c.name, tls));
                if first_tls != tls {
                    problems.push(format!(
                        "port {port}: `{first}` and `{}` share it with different `tls` (one server per port: give them the same)",
                        c.name
                    ));
                }
                // Checked before anything is read, so before the path is known: one list per port.
                let (first, first_allow) = *http_allow.entry(*port).or_insert((&c.name, allow));
                if first_allow != allow {
                    problems.push(format!(
                        "port {port}: `{first}` and `{}` share it with different `allow` (it's checked before the request is read: give them the same)",
                        c.name
                    ));
                }
                let key = (*port, method.to_ascii_uppercase(), path.clone());
                if let Some(other) = routes.insert(key, &c.name) {
                    problems.push(format!(
                        "port {port}: `{other}` and `{}` both handle {} {path}",
                        c.name,
                        method.to_ascii_uppercase()
                    ));
                }
            }
            Source::File(_) | Source::Sftp(_) | Source::Schedule(_) | Source::Internal => {}
        }
    }
    for (port, m) in &mllp {
        if let Some(h) = http_ports.get(port) {
            problems.push(format!("port {port}: MLLP (`{m}`) and HTTP (`{h}`) can't share a port"));
        }
    }
    problems.sort();
    problems
}

/// Problems with routes between interfaces (`destination: { interface: b }`): a target that
/// doesn't exist or doesn't have `source: internal`, a format the target doesn't read, and
/// loops (a message would go round forever).
pub fn check_routes<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let configs: BTreeMap<&str, &InterfaceConfig> = configs.into_iter().map(|c| (c.name.as_str(), c)).collect();
    let mut problems = Vec::new();
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in configs.values() {
        for (name, d) in c.destinations() {
            let Some(target) = d.route() else { continue };
            let from = c.label_destination(name);
            let Some(t) = configs.get(target) else {
                problems.push(format!("{from}: no interface named `{target}`"));
                continue;
            };
            if !matches!(t.source, Source::Internal) {
                problems.push(format!(
                    "{from} routes to `{target}`, which has a `{}` source: give `{target}` `source: internal`",
                    t.source.kind()
                ));
                continue;
            }
            let output = c.output_for(d);
            if *output != t.format {
                problems.push(format!(
                    "{from} sends {} but `{target}` reads {}: set `output:` on the destination or `format:` on `{target}`",
                    output.name(),
                    t.format.name()
                ));
            }
            edges.entry(c.name.as_str()).or_default().insert(target);
        }
    }
    // Loops: depth-first from each interface, in name order, so each loop is reported once
    // (from its first name).
    let mut reported: BTreeSet<BTreeSet<&str>> = BTreeSet::new();
    for &start in edges.keys() {
        let mut stack = vec![(start, vec![start])];
        while let Some((at, path)) = stack.pop() {
            for &next in edges.get(at).into_iter().flatten() {
                if next == start {
                    let members: BTreeSet<&str> = path.iter().copied().collect();
                    if reported.insert(members) {
                        problems.push(format!("routes loop: {} → {start}", path.join(" → ")));
                    }
                } else if next > start && !path.contains(&next) {
                    let mut longer = path.clone();
                    longer.push(next);
                    stack.push((next, longer));
                }
            }
        }
    }
    problems.sort();
    problems
}

/// Interfaces with `source: internal` that no interface routes to: they'd never get anything.
pub fn unrouted<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig> + Clone) -> Vec<&'a str> {
    let targets: std::collections::BTreeSet<&str> =
        configs.clone().into_iter().flat_map(|c| c.destinations().filter_map(|(_, d)| d.route())).collect();
    configs
        .into_iter()
        .filter(|c| matches!(c.source, Source::Internal) && !targets.contains(c.name.as_str()))
        .map(|c| c.name.as_str())
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum HttpMode {
    #[default]
    Queue,
    Respond,
}

/// Name of the destination given with `destination:` (rather than `destinations:`).
pub const DEFAULT_DESTINATION: &str = "default";

/// One destination: where to (`connector`) and how messages are prepared for it. What each
/// setting means is on `RawDestination`, the destination as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawDestination")]
pub struct DestinationConfig {
    pub connector: Destination,
    pub transform: Option<String>,
    pub output: Option<Format>,
    pub retry: Option<Retry>,
    pub rate_limit: Option<RateLimit>,
    pub save_output: bool,
    pub application: Option<String>,
    pub vendor: Option<String>,
    pub owner: Option<String>,
    pub monitor: crate::monitor::DestinationMonitor,
    pub custom: crate::docs::Custom,
}

/// A destination in a schema: one connector key (`mllp`, `file`, ...) beside the options, as
/// `RawDestination` reads it (exactly one, nothing else). The options are defined once
/// (`DestinationOptions`) and each connector's branch points at them.
#[cfg(test)]
impl schemars::JsonSchema for DestinationConfig {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Destination".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let defined = serde_json::to_value(generator.subschema_for::<RawDestination>()).expect("JSON");
        let at = defined["$ref"].as_str().expect("options are a definition").to_string();
        let options = serde_json::to_value(RawDestination::json_schema(generator)).expect("JSON");
        let connectors = serde_json::to_value(Destination::json_schema(generator)).expect("JSON");
        let variants = connectors["oneOf"].as_array().expect("one schema per connector");
        let one_of: Vec<serde_json::Value> = variants
            .iter()
            .map(|variant| {
                let keys = options["properties"].as_object().into_iter().flat_map(|p| p.keys());
                let mut properties: serde_json::Map<String, serde_json::Value> =
                    keys.map(|k| (k.clone(), serde_json::json!({ "$ref": format!("{at}/properties/{k}") }))).collect();
                let (key, connector) = variant["properties"].as_object().and_then(|p| p.iter().next()).expect("a key");
                let mut connector = connector.clone();
                if let (Some(d), None) = (variant.get("description"), connector.get("description")) {
                    connector["description"] = d.clone();
                }
                properties.insert(key.clone(), connector);
                serde_json::json!({
                    "type": "object",
                    "properties": properties,
                    "required": [key],
                    "additionalProperties": false
                })
            })
            .collect();
        schemars::json_schema!({ "oneOf": one_of, "x-type": "a connector (mllp, file, sftp, http, interface) and options" })
    }
}

impl DestinationConfig {
    /// The exported function that prepares this destination's messages.
    pub fn function(&self) -> &str {
        self.transform.as_deref().unwrap_or("transform")
    }

    /// The receiving system as configured on the destination itself (may be unknown).
    pub fn system(&self) -> System {
        System::new(&self.application, &self.vendor)
    }

    /// The interface it routes to (`interface: <name>`), if that's where it sends.
    pub fn route(&self) -> Option<&str> {
        match &self.connector {
            Destination::Interface(target) => Some(target),
            _ => None,
        }
    }

    /// The resources.yaml entry it sends through, if any: an SFTP server or an API.
    pub fn resource(&self) -> Option<(Kind, &str)> {
        match &self.connector {
            Destination::Sftp { server, .. } => Some((Kind::Sftp, server)),
            Destination::Http { api: Some(api), .. } => Some((Kind::Api, api)),
            _ => None,
        }
    }
}

/// A destination as written: exactly one connector key (`mllp`, `file`, `sftp`, `http`) next to
/// the options. Read through this so unknown keys are still errors.
#[derive(Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "DestinationOptions"))]
#[serde(deny_unknown_fields)]
struct RawDestination {
    #[serde(default)]
    #[cfg_attr(test, schemars(skip))]
    mllp: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    #[cfg_attr(test, schemars(skip))]
    file: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    #[cfg_attr(test, schemars(skip))]
    sftp: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    #[cfg_attr(test, schemars(skip))]
    http: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    #[cfg_attr(test, schemars(skip))]
    interface: Option<serde_yaml_ng::Value>,
    /// The function in the interface's `script` that prepares messages for this destination: its
    /// exported name. Each destination runs its function on the message as it was received, not
    /// on another destination's result, so `toLab` and `toBilling` can each shape it for their
    /// receiver. Default: `transform`. Example: `transform: toBilling`.
    #[serde(default)]
    transform: Option<String>,
    /// The format this destination's results are written in, when it differs from the
    /// interface's `output` (one receiver wants HL7, another JSON). Same values as `format`.
    /// Default: the interface's `output`, else its `format`. Example: `output: json`.
    #[serde(default, deserialize_with = "crate::format::deserialize_opt")]
    output: Option<Format>,
    /// This destination's own retry settings, in place of the interface's `retry` (all of them:
    /// what isn't given here takes its default, not the interface's value). Example: `retry: {
    /// max_attempts: 10 }`.
    #[serde(default)]
    retry: Option<Retry>,
    /// This destination's own rate limit, in place of the interface's `rate_limit`. Example:
    /// `rate_limit: { per_minute: 60 }`.
    #[serde(default)]
    rate_limit: Option<RateLimit>,
    /// Keep a copy of exactly what was sent to this destination, beside the original.
    ///
    /// Off by default because what was sent can be made again from the original by the code,
    /// and copies cost space: about 1.15 GB per million deliveries of a typical HL7 message.
    /// Turn it on where you must be able to show what a receiver got (a billing or regulatory
    /// feed), or to compare a code change against what was really sent (`ductus replay
    /// --against stored`). Copies are kept as long as their messages. Default: `false`.
    /// Example: `save_output: true`.
    #[serde(default)]
    save_output: bool,
    /// The receiving application, when this destination's differs from `about.to` (an interface
    /// that sends to several systems). `ductus impact` and the web UI use it. Default:
    /// `about.to.application`. Example: `application: Sunquest Lab`.
    #[serde(default)]
    application: Option<String>,
    /// The receiving application's vendor, when this destination's differs from `about.to`.
    /// Default: `about.to.vendor`. Example: `vendor: Sunquest`.
    #[serde(default)]
    vendor: Option<String>,
    /// Who to ask about this destination, when it isn't the interface's `about.owner` (its code
    /// or its receiver is another team's): best written as an email address. Alert rules that
    /// send to `owner` reach them for this destination's alerts. Default: `about.owner`.
    /// Example: `owner: billing-it@hospital.org`.
    #[serde(default)]
    owner: Option<String>,
    /// This destination's own `behind` limit, in place of the interface's `monitor.behind` (an
    /// archive may lag an hour; the lab may not). Default: the interface's. Example: `monitor: {
    /// behind: 2h }`.
    #[serde(default)]
    monitor: crate::monitor::DestinationMonitor,
    /// Your company's documentation fields for this destination (those whose `applies_to` in
    /// `docs-schema.yaml` includes `destination`). Default: none. Example: `custom: { analyst:
    /// ana@hospital.org }`.
    #[serde(default)]
    custom: crate::docs::Custom,
}

impl TryFrom<RawDestination> for DestinationConfig {
    type Error = String;
    fn try_from(raw: RawDestination) -> std::result::Result<Self, String> {
        let connectors = [
            ("mllp", raw.mllp),
            ("file", raw.file),
            ("sftp", raw.sftp),
            ("http", raw.http),
            ("interface", raw.interface),
        ];
        let mut connectors = connectors
            .into_iter()
            .filter_map(|(kind, value)| value.map(|v| serde_yaml_ng::Mapping::from_iter([(kind.into(), v)])));
        let (Some(connector), None) = (connectors.next(), connectors.next()) else {
            return Err("a destination needs exactly one of `mllp`, `file`, `sftp`, `http` or `interface`".into());
        };
        let connector =
            serde_yaml_ng::with::singleton_map_recursive::deserialize(serde_yaml_ng::Value::Mapping(connector))
                .map_err(|e| e.to_string())?;
        Ok(Self {
            connector,
            transform: raw.transform,
            output: raw.output,
            retry: raw.retry,
            rate_limit: raw.rate_limit,
            save_output: raw.save_output,
            application: raw.application,
            vendor: raw.vendor,
            owner: raw.owner,
            monitor: raw.monitor,
            custom: raw.custom,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Destination {
    /// Send each message over MLLP to a receiver (a lab system, another interface engine) and
    /// wait for its ACK: AA is delivered, AE is tried again, AR follows `on_reject`. One
    /// connection, kept open between messages. Example: `mllp: { host: lab.hospital.org, port:
    /// 7001 }`.
    Mllp {
        /// The receiver's host name or address. With `tls`, its certificate must be for this
        /// name. Required. Example: `host: lab.hospital.org`.
        host: String,
        /// The receiver's port. Required. Example: `port: 7001`.
        port: u16,
        /// How long one send may take, in milliseconds: connecting if needed, sending, and
        /// waiting for the ACK. Past it the connection is dropped and the message tried again
        /// later (it may have arrived: receivers that dedupe on MSH-10 take care of the repeat).
        /// Raise it for a receiver that is slow to answer; on Windows keep it above 2000.
        /// Default: `10000`. Example: `timeout_ms: 30000`.
        #[serde(default = "defaults::timeout_ms")]
        timeout_ms: u64,
        /// - `wait`: wait for the receiver's ACK before the next message; the message counts as
        ///   delivered only on AA.
        /// - `none`: send and carry on without reading an answer, for the rare receiver that
        ///   never sends ACKs. Nothing then tells the engine a message was refused or lost.
        ///
        /// Default: `wait`. Example: `ack: none`.
        #[serde(default)]
        ack: AckMode,
        /// What an AR (the receiver rejects the message) does: `quarantine` it for a person to
        /// look at, or `retry` it like an AE, for receivers that answer AR when they're busy or
        /// half-down. A message that keeps being refused holds up its lane while it's retried.
        /// AE is always tried again. Default: `quarantine`. Example: `on_reject: retry`.
        #[serde(default)]
        on_reject: OnReject,
        /// Send over TLS. `true` trusts the certificates this server trusts (the system's store);
        /// `{ ca, certificate }` also trusts the CA named by `ca`, and presents the client
        /// certificate named by `certificate`, both `certificates:` entries in resources.yaml.
        /// A receiver whose certificate isn't trusted or isn't for `host` is refused and tried
        /// again later, with the reason in the log and on the destination. Default: plain MLLP.
        /// Example: `tls: { ca: hie-ca, certificate: ductus-client }`.
        #[serde(default, deserialize_with = "crate::tls::connect_tls_opt")]
        #[cfg_attr(test, schemars(with = "Option<crate::schema::shapes::TlsSetting>"))]
        tls: Option<crate::tls::ConnectTls>,
    },
    /// Write each message as a file in a folder of this server (or a share it mounts), named by
    /// the message's id and the output format (`1234.hl7`). Files are written under a hidden
    /// temporary name and renamed when complete, so a reader never sees half a file. Example:
    /// `file: { dir: outbound }`.
    File {
        /// The folder to write to, made if missing. Relative to the interface's folder unless
        /// written whole. Required. Example: `dir: /data/outbound/lab`.
        dir: PathBuf,
    },
    /// Write each message as a file on an SFTP server, named like the `file` destination's.
    /// Files are written under a temporary name and renamed when complete, so a reader on the
    /// server never sees half a file. Example: `sftp: { server: lab_server, dir: inbound }`.
    Sftp {
        /// The SFTP server: an `sftp:` entry in resources.yaml, which holds its address and
        /// login. Required. Example: `server: lab_server`.
        server: String,
        /// The folder on the server to write to, relative to the folder the login starts in
        /// unless written whole. Required. Example: `dir: inbound/adt`.
        dir: String,
    },
    /// Send each message as an HTTP request (POST unless `method` says otherwise) to a `url`, or
    /// to a `path` under an API from resources.yaml (`api: billing`, which brings its URL,
    /// headers, timeout and TLS). A 2xx answer is delivered; 408, 429, 5xx and network errors are
    /// tried again; any other 4xx is quarantined (the receiver refused this message). Example:
    /// `http: { api: billing, path: /charges }`.
    Http {
        /// The address to send to, `https://` or `http://`. Give this or `api`, not both. A token
        /// in it, or in `headers`, belongs in a secret: `${secret:NAME}`. Example: `url:
        /// "https://lab.example.org/api/orders"`.
        #[serde(default)]
        url: Option<String>,
        /// An `apis:` entry in resources.yaml to send through: it brings the base URL, headers
        /// (an `Authorization` token) and timeout, so the same API is set up once for every
        /// interface and each environment. Give this or `url`. Example: `api: billing`.
        #[serde(default)]
        api: Option<String>,
        /// With `api`: the path under its URL (`/charges` under `https://billing/api/v2`).
        /// Default: the API's URL itself. Example: `path: /charges`.
        #[serde(default)]
        path: Option<String>,
        /// The HTTP method: `POST`, `PUT`, `PATCH`, or any other the receiver expects. Default:
        /// `POST`. Example: `method: PUT`.
        #[serde(default = "defaults::post")]
        method: String,
        /// Headers sent with every request, added to the API's (and replacing an API header of
        /// the same name). Put tokens in secrets. Default: none (the API's, with `api`).
        /// Example: `headers: { X-Facility: north }`.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
        /// How long a request may take, in milliseconds, before it's given up and tried again
        /// later. Default: the API's `timeout`, else `10000`. Example: `timeout_ms: 30000`.
        #[serde(default)]
        timeout_ms: Option<u64>,
        /// With an `https://` `url`: a CA to trust for this server besides the system's (a
        /// `certificates:` entry), when the receiver's certificate comes from a private CA.
        /// With `api`, the API's own `ca` applies instead. Example: `ca: lab-ca`.
        #[serde(default)]
        ca: Option<String>,
        /// With an `https://` `url`: the client certificate to present (a `certificates:` entry
        /// with its key), for receivers that ask for one (mutual TLS). With `api`, the API's own
        /// applies instead. Example: `certificate: ductus-client`.
        #[serde(default)]
        certificate: Option<String>,
    },
    /// Put each message into another interface's queue: name an interface that has `source:
    /// internal`. It's stored there in the same transaction that marks it sent here: no network
    /// hop, nothing lost or sent twice between the two. The other interface's `format` must be
    /// what this destination outputs, and routes can't form a loop (`ductus validate` checks
    /// both). Example: `interface: adt-to-billing`.
    Interface(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    #[default]
    Strict,
    Relaxed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    #[default]
    Shared,
    Dedicated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum AckMode {
    #[default]
    Wait,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum OnReject {
    #[default]
    Quarantine,
    Retry,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Dedupe {
    /// The fields whose values, together, make a message the same as one seen before, as key
    /// paths in the interface's `format`: HL7 `MSH-10`, JSON `order.id`, XML `Order/@id`, a CSV
    /// column or a fixed-length field's name. Choose fields a resend repeats and a new message
    /// never does: the sender's control ID, or an order number with its status. Default:
    /// `[MSH-10]` (HL7's message control ID). Example: `keys: [ORC-2, ORC-5]`.
    #[serde(default = "defaults::dedupe_keys")]
    pub keys: Vec<String>,
    /// How long a message's keys are remembered: a repeat within this long is a duplicate,
    /// after it a new message. Cover the longest a sender may take to resend (after a
    /// restart, a weekend). Default: `24h`. Example: `window: 7d`.
    #[serde(default = "defaults::dedupe_window", deserialize_with = "duration")]
    #[cfg_attr(test, schemars(with = "crate::schema::shapes::Duration"))]
    pub window: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    /// How many messages a minute the destination may be sent, at least 1: what the receiver
    /// (or its vendor's quota) allows. Required. Example: `per_minute: 600` (10 a second).
    pub per_minute: u32,
    /// - `average`: messages go at `per_minute` on average, and up to `burst` may go at once
    ///   after a quiet spell (a token bucket).
    /// - `hard`: never more than `per_minute` in any one calendar minute, for quotas counted
    ///   that way.
    ///
    /// Default: `average`. Example: `mode: hard`.
    #[serde(default)]
    pub mode: RateMode,
    /// With `mode: average`: how many may go at once after a quiet spell. Lower it for a
    /// receiver that can't take a burst at all. Default: `per_minute`. Example: `burst: 20`.
    pub burst: Option<u32>,
    /// What happens to messages over the limit: `wait` (they're held, in order, until they're
    /// allowed) or `drop` (they're filtered, with the reason, and never sent). Drop only what is
    /// worthless late, such as frequent status updates where the next one replaces the last.
    /// Default: `wait`. Example: `over_limit: drop`.
    #[serde(default)]
    pub over_limit: OverLimit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum RateMode {
    #[default]
    Average,
    Hard,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum OverLimit {
    #[default]
    Wait,
    Drop,
}

fn check_rate_limit(r: &RateLimit) -> Result<()> {
    if r.per_minute == 0 {
        bail!("rate_limit.per_minute must be at least 1");
    }
    if r.burst == Some(0) {
        bail!("rate_limit.burst must be at least 1");
    }
    Ok(())
}

/// Parse durations like "500ms", "90s", "15m", "24h", "7d". (`audit.keep` also takes years.)
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: u64 = number.parse().with_context(|| format!("invalid duration `{text}`"))?;
    let seconds = match unit.trim() {
        "ms" => return Ok(Duration::from_millis(n)),
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("invalid duration `{text}` (use e.g. 90s, 15m, 24h, 7d)"),
    };
    Ok(Duration::from_secs(seconds))
}

pub(crate) fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    let text = String::deserialize(d)?;
    parse_duration(&text).map_err(serde::de::Error::custom)
}

pub(crate) fn duration_opt<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Duration>, D::Error> {
    duration(d).map(Some)
}

/// Messages with the same key value are delivered in the order received.
/// With no key, the whole interface is one ordered lane.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Ordering {
    /// The field whose value decides which messages stay in order with each other, as a key path
    /// in the interface's `format`: usually the patient (`PID-3.1`, the MRN), or an order or
    /// account. Messages with the same value are delivered in arrival order; different values
    /// can overtake each other, so one patient's failing message doesn't hold up everyone.
    ///
    /// Choose the narrowest value correctness allows: by patient for ADT, by order for results.
    /// A message whose field is empty goes to the lane of the empty value. Without a key, the
    /// whole interface is delivered in one strict order, one message at a time per destination.
    /// Default: none. Example: `key: PID-3.1`.
    pub key: Option<String>,
    /// How many lanes messages are spread over by their `key`. Messages with the same key value
    /// always go to the same lane, and each lane is delivered strictly in arrival order, so more
    /// lanes let more patients' messages move at once while each patient's stay in order.
    /// Without a `key` this is ignored: the whole interface is one lane.
    ///
    /// The lanes are divided among the workers (one per destination in the shared pool;
    /// `workers` with `isolation: dedicated`), and a worker with no lane has nothing to do. The
    /// default suits up to 64 workers; to give N workers 64 lanes each, use `64 * N`.
    ///
    /// Change it only when nothing is waiting (`ductus drain --wait` first). Lowered with
    /// messages waiting, the engine refuses to start: they'd be in lanes no worker reads.
    /// Raised, it starts, but a patient's waiting messages and their new ones can go out of order
    /// until the backlog drains. Default: `64`. Example: `ordering: { key: PID-3.1, partitions:
    /// 128 }`.
    #[serde(default = "defaults::partitions")]
    pub partitions: u32,
}

impl Default for Ordering {
    fn default() -> Self {
        Self { key: None, partitions: defaults::partitions() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// Give up after this many failed delivery attempts and quarantine the message ("gave up
    /// after N attempts: …"). While a message is being retried it holds up its lane, so set this
    /// where a person looking at it beats waiting for the receiver (a receiver that refuses
    /// some messages for good with errors that look temporary). Leave it unset where every
    /// message must get there once the receiver is back. The code's own temporary errors (a
    /// database down) are always retried. Default: no limit. Example: `max_attempts: 10`.
    pub max_attempts: Option<u32>,
    /// The first wait after a failure, in milliseconds; each later one doubles, up to
    /// `max_backoff_ms`. Default: `1000`. Example: `initial_backoff_ms: 5000`.
    #[serde(default = "defaults::backoff_ms")]
    pub initial_backoff_ms: u64,
    /// The longest wait between tries, in milliseconds: how soon after a receiver comes back
    /// the waiting messages start moving, at worst. Default: `60000`. Example: `max_backoff_ms:
    /// 300000`.
    #[serde(default = "defaults::max_backoff_ms")]
    pub max_backoff_ms: u64,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: None,
            initial_backoff_ms: defaults::backoff_ms(),
            max_backoff_ms: defaults::max_backoff_ms(),
        }
    }
}

mod defaults {
    pub fn workers() -> u32 {
        4
    }
    pub fn post() -> String {
        "POST".into()
    }
    pub fn host() -> String {
        "0.0.0.0".into()
    }
    pub fn timeout_ms() -> u64 {
        10_000
    }
    pub fn partitions() -> u32 {
        64
    }
    pub fn backoff_ms() -> u64 {
        1_000
    }
    pub fn max_backoff_ms() -> u64 {
        60_000
    }
    pub fn dedupe_keys() -> Vec<String> {
        vec!["MSH-10".into()]
    }
    pub fn dedupe_window() -> std::time::Duration {
        std::time::Duration::from_secs(24 * 3600)
    }
    pub fn pattern() -> String {
        "*".into()
    }
    pub fn poll() -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }
    pub fn timeout() -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
}

impl InterfaceConfig {
    /// Load `<dir>/interface.yaml` with no environment (any `${...}` reference is an error).
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_in(dir, &crate::Environment::default())
    }

    /// Load `<dir>/interface.yaml` with the environment's variables and secrets substituted.
    pub fn load_in(dir: &Path, env: &crate::Environment) -> Result<Self> {
        let file = dir.join("interface.yaml");
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let mut value: serde_yaml_ng::Value =
            crate::env::parse_yaml(&text).with_context(|| format!("parsing {}", file.display()))?;
        env.substitute(&mut value).with_context(|| format!("in {}", file.display()))?;
        let mut config: Self = serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))?;
        config.dir = dir.to_path_buf();
        config.validate().with_context(|| format!("validating {}", file.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        match (self.isolation, self.workers) {
            (Isolation::Shared, Some(_)) => {
                bail!("workers only applies with `isolation: dedicated` (shared interfaces use the engine's pool)")
            }
            (Isolation::Dedicated, Some(0)) => bail!("workers must be at least 1"),
            _ => {}
        }
        if !self.dedicated_pools.is_empty() && self.isolation == Isolation::Shared {
            bail!(
                "dedicated_pools needs `isolation: dedicated` (interfaces in the shared pool use the shared \
                 connection pools; set max_connections in resources.yaml)"
            );
        }
        if let Some((name, _)) = self.dedicated_pools.iter().find(|(_, n)| **n == 0) {
            bail!("dedicated_pools.{name} must be at least 1 connection");
        }
        if self.ordering.partitions == 0 {
            bail!("ordering.partitions must be at least 1");
        }
        if !(Duration::from_millis(10)..=Duration::from_secs(600)).contains(&self.timeout) {
            bail!("timeout must be between 10ms and 10m (other interfaces on the thread wait while code runs)");
        }
        if let Some(script) = self.script_path()
            && !script.is_file()
        {
            bail!("script not found: {}", script.display());
        }
        if let Some(d) = &self.dedupe
            && d.keys.is_empty()
        {
            bail!("dedupe.keys must list at least one key path");
        }
        let keys = self.ordering.key.iter().chain(self.dedupe.iter().flat_map(|d| d.keys.iter()));
        for key in keys {
            self.format.check_key_path(key).with_context(|| format!("key `{key}` (format {})", self.format.name()))?;
        }
        if matches!(self.source, Source::Mllp { .. }) && self.format != Format::Hl7 {
            bail!("an MLLP source carries HL7: `format: {}` needs another source type", self.format.name());
        }
        if self.destination.is_some() && !self.destinations.is_empty() {
            bail!("use either `destination` (one) or `destinations` (several, by name), not both");
        }
        let has_destination = self.destinations().next().is_some();
        match (self.responds(), has_destination) {
            (true, true) => {
                bail!("an HTTP source in `respond` mode answers with the transform's result; remove `destination`")
            }
            (true, false) if self.script.is_none() => bail!("an HTTP source in `respond` mode needs a `script`"),
            (true, false) if self.retention.is_some() => {
                bail!("an HTTP source in `respond` mode stores no messages; remove `retention`")
            }
            (true, false) if self.quarantined.is_some() => {
                bail!("an HTTP source in `respond` mode stores no messages; remove `quarantined`")
            }
            (false, false) => bail!("`destination` is required (or `destinations`, to send to several places)"),
            _ => {}
        }
        for (name, d) in self.destinations() {
            let what = || format!("destination `{name}`");
            let valid_name = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-';
            if name.is_empty() || name.len() > 64 || !name.chars().all(valid_name) {
                bail!("{}: names use lower-case letters, digits, `_` and `-` (at most 64)", what());
            }
            if let Some(function) = &d.transform {
                let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
                if function.is_empty() || function.starts_with(|c: char| c.is_ascii_digit()) {
                    bail!("{}: `transform` must name an exported function", what());
                }
                if !function.chars().all(identifier) {
                    bail!("{}: `transform: {function}` isn't a function name", what());
                }
                if self.script.is_none() {
                    bail!("{}: `transform: {function}` needs a `script` that exports it", what());
                }
            }
            if let Destination::Sftp { server, .. } = &d.connector
                && server.trim().is_empty()
            {
                bail!("{}: SFTP needs `server` (a name under `sftp:` in resources.yaml)", what());
            }
            if let Destination::Http { url, api, path, ca, certificate, .. } = &d.connector {
                let tls = ca.is_some() || certificate.is_some();
                if tls && api.is_some() {
                    bail!("{}: `ca` and `certificate` go on the API in resources.yaml (it brings them)", what());
                }
                if tls && url.as_deref().is_some_and(|u| !u.trim().to_ascii_lowercase().starts_with("https://")) {
                    bail!("{}: `ca` and `certificate` are for an https:// `url`", what());
                }
                match (url, api) {
                    (Some(_), Some(_)) | (None, None) => {
                        bail!("{}: HTTP needs either `url` or `api` (a name under `apis:` in resources.yaml)", what())
                    }
                    (Some(_), None) if path.is_some() => {
                        bail!("{}: `path` goes with `api`; put it in the `url` instead", what())
                    }
                    (Some(u), None) if crate::resources::Endpoint::from_url(u).is_none() => {
                        bail!("{}: `url` must be an http(s) URL, not `{u}`", what())
                    }
                    _ => {}
                }
            }
            if let Some(target) = d.route() {
                if target.trim().is_empty() {
                    bail!("{}: `interface` needs the name of the interface to route to", what());
                }
                if target == self.name {
                    bail!("{}: an interface can't route to itself (messages would go round forever)", what());
                }
            }
            if let Some(r) = &d.rate_limit {
                check_rate_limit(r).with_context(what)?;
            }
        }
        for host in &self.about.from.hosts {
            crate::about::Network::parse(host).context("about.from.hosts")?;
        }
        if let Some(allow) = self.source.allow() {
            allow.rules().context("source: allow")?;
        }
        if let Source::Http { path, method, .. } = &self.source {
            if !path.starts_with('/') {
                bail!("HTTP path must start with /");
            }
            if !matches!(method.to_ascii_uppercase().as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                bail!("HTTP method must be GET, POST, PUT, PATCH or DELETE");
            }
        }
        if self.responds() && (self.ordering.key.is_some() || self.dedupe.is_some() || self.rate_limit.is_some()) {
            bail!("ordering, dedupe and rate_limit apply to queued messages, not `respond` mode");
        }
        if self.responds() && !self.monitor.is_empty() {
            bail!("`monitor` watches queued messages; an HTTP source in `respond` mode has none");
        }
        self.monitor.validate()?;
        match &self.source {
            Source::File(f) | Source::Sftp(f) => {
                let sftp = matches!(self.source, Source::Sftp(_));
                match (&f.server, sftp) {
                    (None, true) => bail!("an SFTP source needs `server` (a name under `sftp:` in resources.yaml)"),
                    (Some(_), false) => bail!("`server` is for SFTP sources; use `source: {{ sftp: ... }}`"),
                    _ => {}
                }
                if f.pattern.trim().is_empty() {
                    bail!("pattern can't be empty (use * for every file)");
                }
                if f.poll < Duration::from_millis(100) {
                    bail!("poll must be at least 100ms");
                }
                if f.marker.as_deref().is_some_and(|m| m.trim().is_empty()) {
                    bail!("marker can't be empty (e.g. `.done`)");
                }
                if f.split == Split::Records && self.format == Format::Xml {
                    bail!("`split: records` doesn't apply to XML; each XML file is one message");
                }
            }
            Source::Schedule(s) => {
                crate::schedule::Timing::new(s)?;
                if self.script.is_none() {
                    bail!("a schedule source needs a `script`: its transform produces the messages");
                }
                if self.ordering.key.is_some() || self.dedupe.is_some() {
                    bail!("ordering and dedupe don't apply to a schedule source (runs are one at a time)");
                }
            }
            Source::Mllp { .. } | Source::Http { .. } | Source::Internal => {}
        }
        if let Some(r) = &self.rate_limit {
            check_rate_limit(r)?;
        }
        Ok(())
    }

    /// Every destination with its name (`default` for one given with `destination:`).
    pub fn destinations(&self) -> impl Iterator<Item = (&str, &DestinationConfig)> {
        let single = self.destination.iter().map(|d| (DEFAULT_DESTINATION, d));
        single.chain(self.destinations.iter().map(|(name, d)| (name.as_str(), d)))
    }

    /// "`adt` destination `lab`" (or "`adt`" when it has just one destination), for messages.
    pub fn label_destination(&self, name: &str) -> String {
        if self.destination.is_some() {
            format!("`{}`", self.name)
        } else {
            format!("`{}` destination `{name}`", self.name)
        }
    }

    /// The functions the engine calls, each with its formats (see `format_spec`): one per
    /// destination, or `transform` for respond mode. Same order as `destinations()`.
    pub fn functions(&self) -> Vec<(String, String)> {
        if self.responds() {
            return vec![("transform".into(), self.format_spec())];
        }
        self.destinations()
            .map(|(_, d)| (d.function().to_string(), self.format_spec_with(Some(self.output_for(d)))))
            .collect()
    }

    /// A destination's retry settings: its own, or the interface's.
    pub fn retry_for<'a>(&'a self, destination: &'a DestinationConfig) -> &'a Retry {
        destination.retry.as_ref().unwrap_or(&self.retry)
    }

    /// A destination's rate limit: its own, or the interface's (each destination has its own
    /// allowance either way).
    pub fn rate_limit_for<'a>(&'a self, destination: &'a DestinationConfig) -> Option<&'a RateLimit> {
        destination.rate_limit.as_ref().or(self.rate_limit.as_ref())
    }

    /// When a destination's queue counts as behind: its own limit, or the interface's.
    pub fn behind_for(&self, destination: &DestinationConfig) -> Option<crate::monitor::Behind> {
        destination.monitor.behind.or(self.monitor.behind)
    }

    /// The format a destination's results are serialized in.
    pub fn output_for<'a>(&'a self, destination: &'a DestinationConfig) -> &'a Format {
        destination.output.as_ref().or(self.output.as_ref()).unwrap_or(&self.format)
    }

    /// Input and output formats as JSON, for the SDK: `{"input": {...}, "output": {...}}`,
    /// plus `"http": true` when `transform` receives HTTP requests.
    ///
    /// Two sources change what `transform` receives: a schedule passes its run details (JSON;
    /// `format` is then what the transform produces), and a CSV or fixed-length file split into
    /// records passes one record (`"record": true`) rather than a list of one.
    pub fn format_spec(&self) -> String {
        self.format_spec_with(self.output.as_ref())
    }

    /// `format_spec` with another output format (a destination's own).
    fn format_spec_with(&self, output: Option<&Format>) -> String {
        let output = output.unwrap_or(&self.format);
        let mut input = serde_json::to_value(&self.format).expect("formats serialize");
        match &self.source {
            Source::Schedule(_) => input = serde_json::json!({ "kind": "json" }),
            Source::File(f) | Source::Sftp(f) if f.split == Split::Records => {
                if matches!(self.format, Format::Csv(_) | Format::Fixed(_)) {
                    input["record"] = true.into();
                }
            }
            _ => {}
        }
        serde_json::json!({ "input": input, "output": output, "http": self.responds() }).to_string()
    }

    /// The resources.yaml entries this config refers to, each with where (`source`,
    /// `destination `lab``, `dedicated_pools`). Code may use more (`db()`, `api()`).
    pub fn resource_references(&self) -> Vec<(Kind, &str, String)> {
        let mut out = Vec::new();
        if let Source::Sftp(FileSource { server: Some(s), .. }) = &self.source {
            out.push((Kind::Sftp, s.as_str(), "source".to_string()));
        }
        for (name, d) in self.destinations() {
            if let Some((kind, resource)) = d.resource() {
                out.push((kind, resource, format!("destination `{name}`")));
            }
        }
        for db in self.dedicated_pools.keys() {
            out.push((Kind::Database, db.as_str(), "dedicated_pools".to_string()));
        }
        out
    }

    /// An HTTP source in `respond` mode: requests are answered directly, nothing is queued.
    pub fn responds(&self) -> bool {
        matches!(self.source, Source::Http { mode: HttpMode::Respond, .. })
    }

    pub fn script_path(&self) -> Option<PathBuf> {
        self.script.as_ref().map(|s| self.dir.join(s))
    }

    /// Number of independent workers (slots) processing this interface's lanes:
    /// one in the shared pool, `workers` when dedicated.
    pub fn slots(&self) -> u32 {
        match self.isolation {
            Isolation::Shared => 1,
            Isolation::Dedicated => self.workers.unwrap_or(defaults::workers()),
        }
    }

    /// Partitions are spread across slots round-robin. Each partition has exactly one owner,
    /// which is what keeps per-key ordering.
    pub fn partitions_for_worker(&self, worker: u32) -> Vec<i32> {
        let slots = self.slots();
        (0..self.partition_count()).filter(|p| p % slots == worker).map(|p| p as i32).collect()
    }

    /// Effective partition count: one lane when there's no ordering key.
    pub fn partition_count(&self) -> u32 {
        if self.ordering.key.is_some() { self.ordering.partitions } else { 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let yaml = r#"
name: adt
source: { mllp: { port: 6661 } }
script: transform.ts
ordering: { key: PID-3.1, partitions: 8 }
isolation: dedicated
workers: 2
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
retry: { max_attempts: 3 }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(c.name, "adt");
        assert!(matches!(c.source, Source::Mllp { port: 6661, .. }));
        assert_eq!(c.ordering.key.as_deref(), Some("PID-3.1"));
        assert_eq!(c.retry.max_attempts, Some(3));
        assert_eq!(c.partitions_for_worker(0), vec![0, 2, 4, 6]);
        assert_eq!(c.partitions_for_worker(1), vec![1, 3, 5, 7]);
    }

    #[test]
    fn script_timeout_has_a_safe_default_and_range() {
        let load = |extra: &str| -> InterfaceConfig {
            let dest = "destination: { file: { dir: o } }";
            serde_yaml_ng::from_str(&format!("name: a\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}\n{extra}")).unwrap()
        };
        assert_eq!(load("").timeout, Duration::from_secs(5));
        let c = load("timeout: 250ms");
        assert_eq!(c.timeout, Duration::from_millis(250));
        assert!(c.validate().is_ok());
        for bad in ["timeout: 5ms", "timeout: 1h"] {
            assert!(load(bad).validate().unwrap_err().to_string().contains("between 10ms and 10m"), "{bad}");
        }
    }

    #[test]
    fn http_sources_and_destinations() {
        let respond: InterfaceConfig = serde_yaml_ng::from_str(
            "name: api\nsource: { http: { port: 8080, path: '/patients/{mrn}', method: GET, mode: respond } }\nscript: t.ts\nformat: json\n",
        )
        .unwrap();
        assert!(respond.responds() && respond.destination.is_none());
        assert!(respond.format_spec().contains(r#""http":true"#));

        let queue: InterfaceConfig = serde_yaml_ng::from_str(
            "name: orders\nsource: { http: { port: 8080, path: /orders } }\nformat: json\ndestination: { http: { url: 'https://x/api', headers: { Authorization: Bearer t } } }\n",
        )
        .unwrap();
        assert!(!queue.responds());
        assert!(matches!(&queue.source, Source::Http { method, mode: HttpMode::Queue, .. } if method == "POST"));
        assert!(
            matches!(&queue.destination.as_ref().unwrap().connector, Destination::Http { headers, .. } if headers["Authorization"] == "Bearer t")
        );
        assert!(queue.validate().is_ok());

        let mut bad = respond.clone();
        bad.script = None; // no script file in this test; the destination check comes first
        bad.destination = queue.destination.clone();
        assert!(bad.validate().unwrap_err().to_string().contains("remove `destination`"));
        let mut bad = queue.clone();
        bad.destination = None;
        assert!(bad.validate().unwrap_err().to_string().contains("required"));

        // `ca` and `certificate`: for an https URL; an API brings its own.
        let with = |http: &str| {
            let c: InterfaceConfig = serde_yaml_ng::from_str(&format!(
                "name: orders\nsource: {{ http: {{ port: 8080, path: /orders }} }}\nformat: json\ndestination: {{ http: {{ {http} }} }}\n"
            ))
            .unwrap();
            c.validate().map_err(|e| e.to_string())
        };
        assert!(with("url: 'https://x/api', ca: lab-ca, certificate: ours").is_ok());
        assert!(with("url: 'http://x/api', ca: lab-ca").unwrap_err().contains("are for an https:// `url`"));
        assert!(with("api: lab, certificate: ours").unwrap_err().contains("go on the API in resources.yaml"));
    }

    #[test]
    fn retention_is_one_of_the_fixed_windows() {
        let load = |extra: &str| {
            serde_yaml_ng::from_str::<InterfaceConfig>(&format!(
                "name: a\nsource: {{ mllp: {{ port: 6661 }} }}\ndestination: {{ file: {{ dir: o }} }}\n{extra}"
            ))
        };
        assert_eq!(load("").unwrap().retention, None, "ductus.yaml's applies");
        assert_eq!(load("retention: 3w").unwrap().retention.map(|r| r.days()), Some(21));
        let err = load("retention: 60d").unwrap_err().to_string();
        assert!(err.contains("1w, 3w or 7w"), "{err}");
        assert_eq!(load("").unwrap().quarantined, None);
        assert_eq!(load("quarantined: purge").unwrap().quarantined, Some(crate::Quarantined::Purge));

        // Respond mode stores nothing, so it has nothing to keep.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.ts"), "").unwrap();
        let mut respond: InterfaceConfig = serde_yaml_ng::from_str(
            "name: api\nsource: { http: { port: 8080, path: /p, mode: respond } }\nscript: t.ts\nformat: json\nretention: 1w\n",
        )
        .unwrap();
        respond.dir = dir.path().to_path_buf();
        assert!(respond.validate().unwrap_err().to_string().contains("remove `retention`"));
        respond.retention = None;
        respond.quarantined = Some(crate::Quarantined::Keep);
        assert!(respond.validate().unwrap_err().to_string().contains("remove `quarantined`"));
    }

    #[test]
    fn finds_port_conflicts() {
        let load = |yaml: &str| -> InterfaceConfig { serde_yaml_ng::from_str(yaml).unwrap() };
        let dest = "destination: { file: { dir: o } }";
        let a = load(&format!("name: a\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}"));
        let b = load(&format!("name: b\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}"));
        let c = load(&format!("name: c\nsource: {{ http: {{ port: 8080, path: /x }} }}\nformat: json\n{dest}"));
        let d = load(&format!(
            "name: d\nsource: {{ http: {{ port: 8080, path: /x, method: post }} }}\nformat: json\n{dest}"
        ));
        let e = load(&format!("name: e\nsource: {{ http: {{ port: 8080, path: /y }} }}\nformat: json\n{dest}"));
        let f = load(&format!("name: f\nsource: {{ http: {{ port: 6661, path: /z }} }}\nformat: json\n{dest}"));
        assert!(check_endpoints([&c, &e]).is_empty(), "different paths may share a port");
        let tls = |name: &str, certificate: &str| {
            load(&format!(
                "name: {name}\nsource: {{ http: {{ port: 8443, path: /{name}, tls: {{ certificate: {certificate} }} }} }}\nformat: json\n{dest}"
            ))
        };
        let (g, h, i) = (tls("g", "api"), tls("h", "api"), tls("i", "other"));
        assert!(check_endpoints([&g, &h]).is_empty(), "the same tls may share a port");
        assert_eq!(
            check_endpoints([&g, &i]),
            ["port 8443: `g` and `i` share it with different `tls` (one server per port: give them the same)"]
        );
        let allow = |name: &str, list: &str| {
            load(&format!(
                "name: {name}\nsource: {{ http: {{ port: 8081, path: /{name}, allow: [{list}] }} }}\nformat: json\n{dest}"
            ))
        };
        let (j, k, l) = (allow("j", "10.1.20.0/24"), allow("k", "10.1.20.0/24"), allow("l", "epic"));
        assert!(check_endpoints([&j, &k]).is_empty(), "the same allow may share a port");
        assert_eq!(
            check_endpoints([&j, &l]),
            [
                "port 8081: `j` and `l` share it with different `allow` (it's checked before the request is read: give them the same)"
            ]
        );
        assert_eq!(j.source.allow().map(|a| a.summary()).as_deref(), Some("10.1.20.0/24"));
        let mut bad = load(&format!("name: m\nsource: {{ mllp: {{ port: 6670, allow: [] }} }}\n{dest}"));
        bad.dir = std::env::temp_dir();
        let err = format!("{:#}", bad.validate().unwrap_err());
        assert!(err.contains("source: allow") && err.contains("lets no one connect"), "{err}");
        assert_eq!(
            check_endpoints([&a, &b, &c, &d, &f]),
            [
                "port 6661: MLLP (`a`) and HTTP (`f`) can't share a port",
                "port 6661: MLLP listeners for both `a` and `b`",
                "port 8080: `c` and `d` both handle POST /x",
            ]
        );
    }

    #[test]
    fn parses_reliability_options() {
        let yaml = r#"
name: device
source: { mllp: { port: 1 } }
destination: { mllp: { host: h, port: 2, ack: none, on_reject: retry } }
dedupe: { keys: [PID-3.1, EVN-2], window: 30m }
rate_limit: { per_minute: 100, burst: 500, over_limit: drop }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(
            c.destination.as_ref().unwrap().connector,
            Destination::Mllp { ack: AckMode::None, on_reject: OnReject::Retry, .. }
        ));
        let d = c.dedupe.unwrap();
        assert_eq!(d.keys, ["PID-3.1", "EVN-2"]);
        assert_eq!(d.window, Duration::from_secs(1800));
        let r = c.rate_limit.unwrap();
        assert_eq!((r.per_minute, r.burst, r.mode, r.over_limit), (100, Some(500), RateMode::Average, OverLimit::Drop));

        let defaults: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { mllp: { host: h, port: 2 } }\ndedupe: {}\n",
        )
        .unwrap();
        assert!(matches!(
            defaults.destination.as_ref().unwrap().connector,
            Destination::Mllp { ack: AckMode::Wait, on_reject: OnReject::Quarantine, .. }
        ));
        assert_eq!(defaults.dedupe.unwrap().keys, ["MSH-10"]);
    }

    #[test]
    fn shared_interfaces_have_one_slot_with_every_lane() {
        let shared: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\nordering: { key: PID-3.1, partitions: 4 }\ndestination: { file: { dir: o } }\n",
        )
        .unwrap();
        assert_eq!(shared.isolation, Isolation::Shared);
        assert_eq!(shared.slots(), 1);
        assert_eq!(shared.partitions_for_worker(0), vec![0, 1, 2, 3]);

        let mut bad = shared.clone();
        bad.workers = Some(4);
        assert!(bad.validate().unwrap_err().to_string().contains("isolation: dedicated"));
    }

    #[test]
    fn dedicated_pools_need_a_dedicated_interface() {
        let mut c: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: o } }\ndedicated_pools: { patient_db: 4 }\n",
        )
        .unwrap();
        assert!(c.validate().unwrap_err().to_string().contains("dedicated_pools needs `isolation: dedicated`"));
        c.isolation = Isolation::Dedicated;
        assert!(c.validate().is_ok());
        c.dedicated_pools.insert("patient_db".into(), 0);
        assert!(c.validate().unwrap_err().to_string().contains("at least 1"));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert!(parse_duration("5 weeks").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn file_sftp_and_schedule_sources() {
        let load = |yaml: &str| -> Result<InterfaceConfig> {
            let c: InterfaceConfig = serde_yaml_ng::from_str(yaml)?;
            c.validate()?;
            Ok(c)
        };
        let dest = "destination: { file: { dir: out } }";
        let csv = load(&format!(
            "name: a\nsource: {{ file: {{ dir: in, pattern: '*.csv', poll: 2s, split: records }} }}\nformat: csv\n{dest}"
        ))
        .unwrap();
        let Source::File(f) = &csv.source else { panic!() };
        assert_eq!((f.poll, f.after, f.archive().as_str()), (Duration::from_secs(2), After::Move, "in/archive"));
        assert!(csv.format_spec().contains(r#""record":true"#), "a split CSV passes one record");
        assert!(csv.resource_references().is_empty());

        let whole = load(&format!("name: b\nsource: {{ file: {{ dir: in }} }}\nformat: csv\n{dest}")).unwrap();
        assert!(!whole.format_spec().contains("record"));

        let sftp = load(
            "name: c\nsource: { sftp: { server: lab, dir: /out, after: delete } }\ndestination: { sftp: { server: emr, dir: inbound } }\n",
        )
        .unwrap();
        assert_eq!(
            sftp.resource_references(),
            [(Kind::Sftp, "lab", "source".into()), (Kind::Sftp, "emr", "destination `default`".into())]
        );

        let err = |yaml: &str| format!("{:#}", load(yaml).unwrap_err());
        assert!(err(&format!("name: d\nsource: {{ sftp: {{ dir: x }} }}\n{dest}")).contains("needs `server`"));
        assert!(err(&format!("name: d\nsource: {{ file: {{ dir: x, server: s }} }}\n{dest}")).contains("for SFTP"));
        assert!(
            err(&format!("name: d\nsource: {{ file: {{ dir: x, split: records }} }}\nformat: xml\n{dest}"))
                .contains("XML")
        );
        assert!(err(&format!("name: d\nsource: {{ file: {{ dir: x, poll: 10ms }} }}\n{dest}")).contains("100ms"));

        let mut nightly: InterfaceConfig = serde_yaml_ng::from_str(&format!(
            "name: e\nsource: {{ schedule: {{ cron: '0 2 * * *' }} }}\nscript: t.ts\nformat: csv\n{dest}"
        ))
        .unwrap();
        assert!(nightly.format_spec().contains(r#""input":{"kind":"json"}"#), "the transform gets the run details");
        nightly.script = None;
        assert!(nightly.validate().unwrap_err().to_string().contains("needs a `script`"));
        assert!(
            err(&format!("name: f\nsource: {{ schedule: {{ every: 1m, cron: '* * * * *' }} }}\n{dest}"))
                .contains("one of `every`")
        );
    }

    #[test]
    fn several_destinations_each_with_their_own_options() {
        let yaml = r#"
name: adt
source: { mllp: { port: 6661 } }
script: transform.ts
output: hl7
retry: { max_attempts: 9 }
destinations:
  lab: { mllp: { host: lab, port: 6662 } }
  billing:
    http: { url: 'https://billing/api' }
    transform: toBilling
    output: json
    retry: { max_attempts: 2 }
    rate_limit: { per_minute: 60 }
    save_output: true
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let all: Vec<_> = c.destinations().collect();
        assert_eq!(all.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["billing", "lab"]);
        let (billing, lab) = (all[0].1, all[1].1);
        assert!(matches!(billing.connector, Destination::Http { .. }) && billing.save_output);
        assert_eq!((billing.function(), lab.function()), ("toBilling", "transform"));
        assert_eq!((c.retry_for(billing).max_attempts, c.retry_for(lab).max_attempts), (Some(2), Some(9)));
        assert!(c.rate_limit_for(billing).is_some() && c.rate_limit_for(lab).is_none());
        assert_eq!((c.output_for(billing), c.output_for(lab)), (&Format::Json, &Format::Hl7));
        let functions = c.functions();
        assert_eq!(functions[0].0, "toBilling");
        assert!(functions[0].1.contains(r#""output":{"kind":"json"}"#), "{}", functions[0].1);
        assert!(functions[1].1.contains(r#""output":{"kind":"hl7"}"#));

        let single: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: o }, save_output: true }\n",
        )
        .unwrap();
        let (name, d) = single.destinations().next().unwrap();
        assert_eq!(name, DEFAULT_DESTINATION);
        assert!(d.save_output && matches!(d.connector, Destination::File { .. }));

        let err = |yaml: &str| match serde_yaml_ng::from_str::<InterfaceConfig>(yaml) {
            Ok(c) => format!("{:#}", c.validate().unwrap_err()),
            Err(e) => e.to_string(),
        };
        let head = "name: x\nsource: { mllp: { port: 1 } }\n";
        assert!(err(&format!("{head}destinations: {{ a: {{ transform: t }} }}")).contains("exactly one of"));
        assert!(
            err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o }}, mllp: {{ host: h, port: 2 }} }} }}"))
                .contains("exactly one of")
        );
        assert!(err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o }}, bogus: 1 }} }}")).contains("bogus"));
        assert!(err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o, bogus: 1 }} }} }}")).contains("bogus"));
        assert!(
            err(&format!(
                "{head}destination: {{ file: {{ dir: o }} }}\ndestinations: {{ a: {{ file: {{ dir: o }} }} }}"
            ))
            .contains("not both")
        );
        assert!(err(&format!("{head}destinations: {{ Lab: {{ file: {{ dir: o }} }} }}")).contains("lower-case"));
        assert!(
            err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o }}, transform: to-x }} }}"))
                .contains("isn't a function name")
        );
        assert!(
            err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o }}, transform: toA }} }}"))
                .contains("needs a `script`")
        );
        assert!(
            err(&format!("{head}destinations: {{ a: {{ file: {{ dir: o }}, rate_limit: {{ per_minute: 0 }} }} }}"))
                .contains("destination `a`")
        );
        assert!(err(&format!("{head}destinations: {{}}")).contains("is required"));
    }

    #[test]
    fn about_and_api_destinations() {
        let yaml = r#"
name: adt
source: { mllp: { port: 6661 } }
about:
  description: ADT to the lab and billing
  owner: Integration team
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24, epic-vpn.local] }
  to: { application: Sunquest Lab }
destinations:
  lab: { mllp: { host: lab, port: 6662 } }
  billing:
    http: { api: billing, path: /charges, headers: { X-Source: ductus } }
    vendor: Experian
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(c.validate().is_ok(), "{:#}", c.validate().unwrap_err());
        assert_eq!(c.about.from.system().label(), "Epic (Epic Systems)");
        let all: Vec<_> = c.destinations().collect();
        let (billing, lab) = (all[0].1, all[1].1);
        assert_eq!((billing.system().label(), lab.system().is_known()), ("(Experian)".to_string(), false));
        assert_eq!(billing.resource(), Some((Kind::Api, "billing")));
        assert_eq!(c.resource_references(), [(Kind::Api, "billing", "destination `billing`".into())]);

        let err = |yaml: &str| {
            let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
            format!("{:#}", c.validate().unwrap_err())
        };
        let head = "name: x\nsource: { mllp: { port: 1 } }\n";
        assert!(err(&format!("{head}destination: {{ http: {{ method: PUT }} }}")).contains("either `url` or `api`"));
        assert!(
            err(&format!("{head}destination: {{ http: {{ url: 'https://a/b', api: b }} }}"))
                .contains("either `url` or `api`")
        );
        assert!(
            err(&format!("{head}destination: {{ http: {{ url: 'https://a/b', path: /c }} }}"))
                .contains("goes with `api`")
        );
        assert!(err(&format!("{head}destination: {{ http: {{ url: 'ftp://a/b' }} }}")).contains("http(s) URL"));
        assert!(
            err(&format!("{head}destination: {{ file: {{ dir: o }} }}\nabout: {{ from: {{ hosts: [10.1.2.0/40] }} }}"))
                .contains("about.from.hosts")
        );
        assert!(
            serde_yaml_ng::from_str::<InterfaceConfig>(&format!(
                "{head}destination: {{ file: {{ dir: o }} }}\nabout: {{ to: {{ hosts: [h] }} }}"
            ))
            .is_err(),
            "hosts are for the sending side"
        );
    }

    #[test]
    fn routes_between_interfaces() {
        let load = |yaml: &str| -> InterfaceConfig {
            let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
            c.validate().unwrap();
            c
        };
        let front = load(
            "name: front\nsource: { mllp: { port: 1 } }\ndestinations:\n  lab: { interface: lab }\n  billing: { interface: billing, output: json }\n",
        );
        let lab = load("name: lab\nsource: internal\ndestination: { file: { dir: o } }\n");
        let billing = load("name: billing\nsource: internal\nformat: json\ndestination: { interface: archive }\n");
        let archive = load("name: archive\nsource: internal\nformat: json\ndestination: { file: { dir: o } }\n");
        assert!(matches!(lab.source, Source::Internal));
        assert_eq!(front.destinations().map(|(_, d)| d.route()).collect::<Vec<_>>(), [Some("billing"), Some("lab")]);
        assert!(check_routes([&front, &lab, &billing, &archive]).is_empty());
        assert!(unrouted([&front, &lab, &billing, &archive]).is_empty());
        assert_eq!(unrouted([&front, &lab, &billing]), Vec::<&str>::new(), "archive isn't loaded");
        assert_eq!(unrouted([&lab]), ["lab"], "nothing routes to it");
        assert!(check_endpoints([&front, &lab]).is_empty());

        // A target that's missing, has another source, or reads another format.
        let wrong_format = load("name: lab\nsource: internal\nformat: json\ndestination: { file: { dir: o } }\n");
        let outside = load("name: lab\nsource: { mllp: { port: 2 } }\ndestination: { file: { dir: o } }\n");
        assert_eq!(
            check_routes([&front, &wrong_format, &billing]),
            [
                "`billing`: no interface named `archive`",
                "`front` destination `lab` sends hl7 but `lab` reads json: set `output:` on the destination or `format:` on `lab`",
            ]
        );
        assert_eq!(
            check_routes([&front, &outside, &billing, &archive]),
            ["`front` destination `lab` routes to `lab`, which has a `mllp` source: give `lab` `source: internal`"]
        );

        // Loops, each reported once.
        let a = load("name: a\nsource: internal\ndestination: { interface: b }\n");
        let b = load("name: b\nsource: internal\ndestinations: { x: { interface: c }, y: { interface: a } }\n");
        let c = load("name: c\nsource: internal\ndestination: { interface: a }\n");
        assert_eq!(check_routes([&a, &b, &c]), ["routes loop: a → b → a", "routes loop: a → b → c → a"]);

        let err = |yaml: &str| {
            format!("{:#}", serde_yaml_ng::from_str::<InterfaceConfig>(yaml).unwrap().validate().unwrap_err())
        };
        assert!(err("name: a\nsource: internal\ndestination: { interface: a }\n").contains("route to itself"));
        assert!(err("name: a\nsource: internal\ndestination: { interface: ' ' }\n").contains("needs the name"));
        assert!(err("name: a\nsource: internal\n").contains("is required"));
    }

    #[test]
    fn monitor_expectations_with_a_destination_of_its_own() {
        let yaml = r#"
name: adt
source: { mllp: { port: 6661 } }
monitor: { active_within: 30m, between: "06:00-22:00", behind: 500 }
destinations:
  lab: { mllp: { host: lab, port: 6662 } }
  billing: { file: { dir: o }, monitor: { behind: { age: 10m } } }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(c.validate().is_ok());
        let all: Vec<_> = c.destinations().collect();
        let (billing, lab) = (all[0].1, all[1].1);
        assert_eq!(c.behind_for(lab).unwrap().messages, Some(500));
        let own = c.behind_for(billing).unwrap();
        assert_eq!((own.messages, own.age), (None, Some(Duration::from_secs(600))), "replaces, not merges");

        let err = |yaml: &str| match serde_yaml_ng::from_str::<InterfaceConfig>(yaml) {
            Ok(c) => format!("{:#}", c.validate().unwrap_err()),
            Err(e) => e.to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.ts"), "").unwrap();
        let mut respond: InterfaceConfig = serde_yaml_ng::from_str(
            "name: api\nsource: { http: { port: 8080, path: /p, mode: respond } }\nscript: t.ts\nformat: json\nmonitor: { behind: 5 }\n",
        )
        .unwrap();
        respond.dir = dir.path().to_path_buf();
        assert!(respond.validate().unwrap_err().to_string().contains("`respond` mode has none"));
        let head = "name: x\nsource: { mllp: { port: 1 } }\n";
        assert!(
            err(&format!("{head}destination: {{ file: {{ dir: o }}, monitor: {{ active_within: 5m }} }}"))
                .contains("active_within")
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: out } }\nbogus: 1\n";
        assert!(serde_yaml_ng::from_str::<InterfaceConfig>(yaml).is_err());
    }
}
