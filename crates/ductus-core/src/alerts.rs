//! Alerts (`alerts:` in ductus.yaml): where notifications go (an SMTP server, email and webhook
//! channels), which health states send them to whom (rules), and how an alert lives. It **fires**
//! when a state turns bad, **escalates** when it gets worse (a higher severity), **reminds** every
//! `repeat` while nobody acknowledges it, and **recovers** once it has been fine for `settle`
//! (going bad again before that reopens the same alert: flap damping). Getting better without
//! being fine (stalled → behind) changes the alert quietly.
//!
//! ```yaml
//! alerts:
//!   smtp: { host: smtp.hospital.org, port: 587, tls: starttls, username: ductus,
//!           password: "${secret:SMTP_PASSWORD}", from: ductus@hospital.org }
//!   channels:
//!     oncall: { email: [interfaces-oncall@hospital.org] }
//!     chat:   { webhook: { url: "${secret:CHAT_WEBHOOK}", format: slack } }   # slack | teams | json
//!   rules:                                   # first match wins; `owner` = who owns it
//!     - { when: [error, stalled, stopped], severity: critical, to: [oncall, chat, owner] }
//!     - { when: [behind, silent, missed],  severity: warning,  to: [chat, owner] }
//!   repeat: 4h          # remind while still bad and not acknowledged (0 = never)
//!   recovery: true      # say when it's fine again
//!   settle: 2m          # fine for this long before it counts as recovered
//!   quiet: { between: "22:00-06:00", severities: [warning] }   # wait until morning
//!   heartbeat: { url: "${secret:HEARTBEAT_URL}", every: 5m }   # a dead-man's switch
//!   keep: 1y            # alert history
//!   certificates: { warn_days: [30, 14, 7] }   # a `certificate` alert's steps (and expired)
//! owners:               # owners that aren't users (a team's mailing list)
//!   "Lab Systems": { email: lab-systems@hospital.org }
//! ```
//!
//! This section of ductus.yaml may use `${NAME}` and `${secret:NAME}`: it is read per environment
//! (`Settings::alerts`). Everything here is pure; the engine (`ductus-runtime`'s `alerts.rs`)
//! feeds it the health states and stores what it decides.

use crate::monitor::{Hours, InterfaceState, State};
use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;

/// A rule's recipient meaning "whoever owns the interface (or the destination)".
pub const OWNER: &str = "owner";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertSettings {
    /// The mail server email channels (and owners) are sent through.
    #[serde(default)]
    pub smtp: Option<Smtp>,
    /// Named places to send to.
    #[serde(default)]
    pub channels: BTreeMap<String, Channel>,
    /// Which states alert, how badly, and to whom: the first rule naming a state wins. A state
    /// no rule names doesn't alert.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Remind this often while an alert is open and nobody acknowledged it. `0`: never.
    #[serde(default = "default_repeat", deserialize_with = "zero_or_duration")]
    pub repeat: Duration,
    /// Tell the same people when it's fine again.
    #[serde(default = "yes")]
    pub recovery: bool,
    /// Fine for this long before an alert counts as recovered; bad again sooner reopens it.
    #[serde(default = "default_settle", deserialize_with = "zero_or_duration")]
    pub settle: Duration,
    /// Hours when some severities wait.
    #[serde(default)]
    pub quiet: Option<Quiet>,
    /// A URL called every so often while the engine runs: a service that hears nothing raises
    /// its own alarm (an engine that is down can't send one).
    #[serde(default)]
    pub heartbeat: Option<Heartbeat>,
    /// How long alert history is kept.
    #[serde(default = "default_keep", deserialize_with = "crate::settings::keep_duration")]
    pub keep: Duration,
    /// When a certificate the engine connects to alerts (`certificate` in a rule).
    #[serde(default)]
    pub certificates: CertificateSettings,
}

/// `alerts.certificates`: the days before expiry at which a `certificate` alert fires (the first)
/// and escalates (each later one); an expired certificate is always critical.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateSettings {
    #[serde(default = "default_warn_days")]
    pub warn_days: Vec<u32>,
}

impl Default for CertificateSettings {
    fn default() -> Self {
        Self { warn_days: default_warn_days() }
    }
}

fn default_warn_days() -> Vec<u32> {
    vec![30, 14, 7]
}

fn default_repeat() -> Duration {
    Duration::from_secs(4 * 3600)
}

fn default_settle() -> Duration {
    Duration::from_secs(120)
}

fn default_keep() -> Duration {
    Duration::from_secs(365 * 86_400)
}

fn yes() -> bool {
    true
}

/// `0` (off), or a duration.
fn zero_or_duration<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    let text = String::deserialize(d)?;
    if text.trim() == "0" {
        return Ok(Duration::ZERO);
    }
    crate::config::parse_duration(&text).map_err(serde::de::Error::custom)
}

/// The mail server.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Smtp {
    pub host: String,
    /// Default: 587 with `starttls`, 465 with `tls`, 25 with `none`.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: SmtpTls,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// The sender address.
    pub from: String,
}

impl std::fmt::Debug for Smtp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Smtp")
            .field("host", &self.host)
            .field("port", &self.port())
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl Smtp {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            SmtpTls::Starttls => 587,
            SmtpTls::Tls => 465,
            SmtpTls::None => 25,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Plain connection upgraded to TLS (submission, port 587). Refused if the server can't.
    #[default]
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption: only for a relay on the same host or network segment.
    None,
}

/// A place to send to: email addresses, a webhook, or both.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    #[serde(default, deserialize_with = "one_or_many")]
    pub email: Vec<String>,
    #[serde(default)]
    pub webhook: Option<Webhook>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// Often holds a secret (chat webhooks are their own password): use `${secret:...}`.
    pub url: String,
    #[serde(default)]
    pub format: WebhookFormat,
}

impl std::fmt::Debug for Webhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhook").field("url", &self.masked_url()).field("format", &self.format).finish()
    }
}

impl Webhook {
    /// The URL without what may be secret (path, query, password): what's shown and logged.
    pub fn masked_url(&self) -> String {
        match url::Url::parse(&self.url) {
            Ok(u) => format!("{}://{}/…", u.scheme(), u.host_str().unwrap_or("?")),
            Err(_) => "(not a URL)".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebhookFormat {
    /// Ductus's own JSON: every field, for scripts and incident tools.
    #[default]
    Json,
    /// Slack incoming webhooks (and Mattermost, Rocket.Chat): `{"text": ...}`.
    Slack,
    /// Microsoft Teams Workflows webhooks: an Adaptive Card.
    Teams,
}

impl WebhookFormat {
    pub fn label(self) -> &'static str {
        match self {
            WebhookFormat::Json => "json",
            WebhookFormat::Slack => "slack",
            WebhookFormat::Teams => "teams",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub when: Vec<Condition>,
    pub severity: Severity,
    /// Channel names, and `owner`.
    pub to: Vec<String>,
}

/// A bad health state an alert is about: a destination's (`error`, `stalled`, `behind`), an
/// interface's (`stopped`, `silent`, `missed`), or a certificate's that is close to expiring or
/// has expired (`certificate`). A pause is on purpose, so it never alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    Error,
    Stalled,
    Behind,
    Stopped,
    Silent,
    Missed,
    Certificate,
}

impl Condition {
    pub const ALL: [Condition; 7] = [
        Condition::Error,
        Condition::Stalled,
        Condition::Behind,
        Condition::Stopped,
        Condition::Silent,
        Condition::Missed,
        Condition::Certificate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Condition::Error => "error",
            Condition::Stalled => "stalled",
            Condition::Behind => "behind",
            Condition::Stopped => "stopped",
            Condition::Silent => "silent",
            Condition::Missed => "missed",
            Condition::Certificate => "certificate",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.label() == label)
    }

    /// A destination's state as a condition (`None`: fine, or paused).
    pub fn of_destination(state: State) -> Option<Self> {
        match state {
            State::Ok | State::Paused => None,
            State::Behind => Some(Condition::Behind),
            State::Error => Some(Condition::Error),
            State::Stalled => Some(Condition::Stalled),
        }
    }

    pub fn of_interface(state: InterfaceState) -> Option<Self> {
        match state {
            InterfaceState::Ok => None,
            InterfaceState::Silent => Some(Condition::Silent),
            InterfaceState::Missed => Some(Condition::Missed),
            InterfaceState::Stopped => Some(Condition::Stopped),
        }
    }

    /// Known as soon as it is true (quarantined messages, a stopped interface, a certificate's
    /// date), rather than after watching for a while: these alert even right after the engine
    /// starts.
    pub fn is_immediate(self) -> bool {
        matches!(self, Condition::Error | Condition::Stopped | Condition::Certificate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        [Severity::Warning, Severity::Critical].into_iter().find(|s| s.label() == label)
    }
}

/// Hours when notifications of some severities wait (critical ones never should).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quiet {
    /// Server local time: `"22:00-06:00"`.
    pub between: Hours,
    #[serde(default = "warnings")]
    pub severities: Vec<Severity>,
}

fn warnings() -> Vec<Severity> {
    vec![Severity::Warning]
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub url: String,
    #[serde(default = "default_heartbeat", deserialize_with = "crate::config::duration")]
    pub every: Duration,
}

fn default_heartbeat() -> Duration {
    Duration::from_secs(300)
}

/// An owner that isn't a user (`owners:` in ductus.yaml): a team's mailing list.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerContact {
    #[serde(default, deserialize_with = "one_or_many")]
    pub email: Vec<String>,
}

/// `x` or `[x, y]`.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// A plausible email address: something@somewhere.
pub fn is_address(text: &str) -> bool {
    let text = text.trim();
    matches!(text.split_once('@'), Some((user, host)) if !user.is_empty() && host.contains('.') && !text.contains(char::is_whitespace))
}

impl AlertSettings {
    /// Problems that stop the engine (and fail `ductus validate`).
    pub fn check(&self) -> Result<()> {
        let mut problems = Vec::new();
        for (name, c) in &self.channels {
            if name == OWNER {
                problems.push(format!("channel `{OWNER}` is reserved for whoever owns the interface: rename it"));
            }
            if c.email.is_empty() && c.webhook.is_none() {
                problems.push(format!("channel `{name}` needs `email` or `webhook`"));
            }
            for address in c.email.iter().filter(|a| !is_address(a)) {
                problems.push(format!("channel `{name}`: `{address}` isn't an email address"));
            }
            if let Some(w) = &c.webhook {
                match url::Url::parse(&w.url) {
                    Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
                    _ => problems.push(format!("channel `{name}`: the webhook `url` must be an http(s) URL")),
                }
            }
        }
        let emails = self.channels.values().any(|c| !c.email.is_empty())
            || self.rules.iter().any(|r| r.to.iter().any(|t| t == OWNER));
        match &self.smtp {
            None if emails => problems.push("email channels and `owner` need `smtp:` (the mail server)".into()),
            Some(s) if !is_address(&s.from) => problems.push(format!("smtp.from `{}` isn't an email address", s.from)),
            Some(s) if s.username.is_some() != s.password.is_some() => {
                problems.push("smtp: give both `username` and `password`, or neither".into())
            }
            _ => {}
        }
        for (n, rule) in self.rules.iter().enumerate() {
            if rule.when.is_empty() || rule.to.is_empty() {
                problems.push(format!("rule {}: needs `when` and `to`", n + 1));
            }
            for to in rule.to.iter().filter(|t| *t != OWNER && !self.channels.contains_key(*t)) {
                problems.push(format!("rule {}: no channel named `{to}` ({})", n + 1, self.channel_names()));
            }
        }
        if let Some(h) = &self.heartbeat {
            if !matches!(url::Url::parse(&h.url), Ok(u) if matches!(u.scheme(), "http" | "https")) {
                problems.push("heartbeat.url must be an http(s) URL".into());
            }
            if h.every < Duration::from_secs(30) {
                problems.push("heartbeat.every must be at least 30s".into());
            }
        }
        if !self.repeat.is_zero() && self.repeat < Duration::from_secs(60) {
            problems.push("repeat must be at least 1m (or 0 for never)".into());
        }
        if self.keep < Duration::from_secs(30 * 86_400) {
            problems.push("keep must be at least 30d".into());
        }
        let days = &self.certificates.warn_days;
        if days.is_empty() || days.iter().any(|d| !(1..=365).contains(d)) || days.windows(2).any(|w| w[0] <= w[1]) {
            problems.push(
                "certificates.warn_days must list 1 to 365 days, longest first, each once (e.g. [30, 14, 7])".into(),
            );
        }
        if !problems.is_empty() {
            bail!("alerts: {}", problems.join("; "));
        }
        Ok(())
    }

    /// Worth saying, not wrong: states no rule names (they won't alert) and rules a state can
    /// never reach (an earlier rule names it).
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let unnamed: Vec<&str> =
            Condition::ALL.iter().filter(|c| self.rule_for(**c).is_none()).map(|c| c.label()).collect();
        if !unnamed.is_empty() {
            out.push(format!("alerts: no rule names {}: those states won't alert", unnamed.join(", ")));
        }
        for (n, rule) in self.rules.iter().enumerate() {
            let earlier = &self.rules[..n];
            for c in rule.when.iter().filter(|c| earlier.iter().any(|r| r.when.contains(c))) {
                out.push(format!("alerts: rule {} names `{}`, but an earlier rule already does", n + 1, c.label()));
            }
        }
        out
    }

    fn channel_names(&self) -> String {
        if self.channels.is_empty() {
            return "none are defined".into();
        }
        let names: Vec<&str> = self.channels.keys().map(String::as_str).collect();
        format!("defined: {}, or `{OWNER}`", names.join(", "))
    }

    /// The rule for a state: the first that names it.
    pub fn rule_for(&self, condition: Condition) -> Option<&Rule> {
        self.rules.iter().find(|r| r.when.contains(&condition))
    }

    /// An interface's `alert.to` must name channels (or `owner`).
    pub fn check_interface(&self, interface: &str, options: &AlertOptions) -> Vec<String> {
        options
            .to
            .iter()
            .filter(|t| *t != OWNER && !self.channels.contains_key(*t))
            .map(|t| format!("{interface}: alert.to names `{t}`, but alerts.channels has no such channel"))
            .collect()
    }

    /// When a notification of this severity may go: now (`None`), or when quiet hours end.
    pub fn quiet_until(&self, severity: Severity, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let quiet = self.quiet.as_ref().filter(|q| q.severities.contains(&severity))?;
        quiet.between.ends(at)
    }
}

/// `alert:` in interface.yaml.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertOptions {
    /// More recipients (channel names or `owner`) for this interface's alerts, on top of the rule's.
    #[serde(default)]
    pub to: Vec<String>,
    /// Send nothing about this interface (a test interface); its alerts still show in the UI.
    #[serde(default)]
    pub mute: bool,
}

/// Who hears about an alert: the rule's recipients, then the interface's own, each once.
pub fn recipients<'a>(rule: &'a Rule, options: &'a AlertOptions) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for to in rule.to.iter().chain(&options.to) {
        if !out.contains(&to.as_str()) {
            out.push(to);
        }
    }
    out
}

/// A user, as far as finding an owner's address goes.
#[derive(Debug, Clone)]
pub struct Person<'a> {
    pub username: &'a str,
    pub display_name: Option<&'a str>,
    pub email: Option<&'a str>,
}

/// An owner's email addresses: the user whose username, email or display name is the owner
/// text (ignoring case), else the `owners:` directory, else the text itself if it is an
/// address. `None`: nobody to send to (the rule's other recipients still hear).
pub fn owner_addresses(
    owner: &str,
    people: &[Person],
    directory: &BTreeMap<String, OwnerContact>,
) -> Option<Vec<String>> {
    let owner = owner.trim();
    if owner.is_empty() {
        return None;
    }
    let same = |a: &str| a.trim().eq_ignore_ascii_case(owner);
    let person = people.iter().find(|p| {
        p.email.is_some() && (same(p.username) || p.email.is_some_and(same) || p.display_name.is_some_and(same))
    });
    if let Some(email) = person.and_then(|p| p.email) {
        return Some(vec![email.to_string()]);
    }
    if let Some((_, contact)) = directory.iter().find(|(name, _)| same(name))
        && !contact.email.is_empty()
    {
        return Some(contact.email.clone());
    }
    is_address(owner).then(|| vec![owner.to_string()])
}

/// The key of an interface's own alert.
pub fn interface_key(interface: &str) -> String {
    format!("iface:{interface}")
}

/// The key of a destination's alert.
pub fn destination_key(interface: &str, destination: &str) -> String {
    format!("dest:{interface}/{destination}")
}

/// The key of a certificate's alert: the resource it belongs to (`empi`, `alerts.smtp`,
/// `adt/lab`) and the server's address.
pub fn certificate_key(resource: &str, host: &str, port: u16) -> String {
    format!("cert:{resource}:{host}:{port}")
}

/// Where a certificate is in its countdown, as a `certificate` alert's step: the smallest of
/// `warn_days` it is within (whole days, UTC dates), `0` once it has expired, `None` while it is
/// further out than the first.
pub fn certificate_stage(not_after: DateTime<Utc>, now: DateTime<Utc>, warn_days: &[u32]) -> Option<i32> {
    if not_after <= now {
        return Some(0);
    }
    let days = days_left(not_after, now);
    warn_days.iter().rev().find(|d| days <= i64::from(**d)).map(|d| *d as i32)
}

/// Whole days until `not_after`, counted in UTC dates: a certificate that ends tomorrow at 00:30
/// UTC has 1 day left all of today. Negative once past.
pub fn days_left(not_after: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (not_after.date_naive() - now.date_naive()).num_days()
}

/// What a certificate alert's words are made of.
#[derive(Debug, Clone)]
pub struct CertificateFacts<'a> {
    pub resource: &'a str,
    pub host: &'a str,
    pub port: u16,
    /// The earliest expiry in the chain the server presents.
    pub not_after: DateTime<Utc>,
    /// The certificate that expires first, when it isn't the server's own (an intermediate).
    pub earliest: Option<&'a str>,
    /// The last check's failure, if it failed (the dates are then from the check before).
    pub error: Option<&'a str>,
}

/// "Certificate of empi.example.org:443 (empi) expires in 14 days, on 2026-10-29." and what else
/// matters; dates in UTC. The first sentence is a notification's title.
pub fn certificate_summary(f: &CertificateFacts, now: DateTime<Utc>) -> String {
    let date = f.not_after.format("%Y-%m-%d");
    let days = days_left(f.not_after, now);
    let plural = |n: i64| if n == 1 { "day" } else { "days" };
    let what = format!("Certificate of {}:{} ({})", f.host, f.port, f.resource);
    let mut text = if f.not_after <= now {
        match -days {
            0 => format!("{what} expired today ({} UTC).", f.not_after.format("%Y-%m-%d %H:%M")),
            n => format!("{what} expired on {date} ({n} {} ago).", plural(n)),
        }
    } else {
        match days {
            0 => format!("{what} expires today ({} UTC).", f.not_after.format("%Y-%m-%d %H:%M")),
            n => format!("{what} expires in {n} {}, on {date}.", plural(n)),
        }
    };
    if let Some(subject) = f.earliest {
        text.push_str(&format!(
            " The earliest expiry in its chain is {subject}'s (an intermediate), not the server's own certificate."
        ));
    }
    if let Some(error) = f.error {
        let error: String = error.chars().take(200).collect();
        text.push_str(&format!(" The last check failed ({error}): these dates are from the check before."));
    }
    text
}

/// What is true now for a certificate: its step and severity (the rule's, critical once expired),
/// fine (renewed, or further out than the first step), or not known yet (never checked).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CertificateNow {
    Fine,
    At { stage: i32, severity: Severity },
    Hold,
}

impl CertificateNow {
    pub fn of(stage: Option<i32>, rule: Severity) -> Self {
        match stage {
            None => CertificateNow::Fine,
            Some(0) => CertificateNow::At { stage: 0, severity: Severity::Critical },
            Some(stage) => CertificateNow::At { stage, severity: rule },
        }
    }
}

/// The next step for a certificate's alert. It fires at the first step and escalates at each
/// closer one (whatever the severity: 14 days is news after 30); each step is told once. It
/// doesn't remind before it has expired (the steps are the reminders: `repeat` would mail every
/// few hours for a month); an expired one reminds like any alert. A renewal that still lands
/// within a step moves it quietly; past the first step it recovers after `settle`.
pub fn certificate_step(
    stored: &Stored,
    stored_stage: Option<i32>,
    now: CertificateNow,
    at: DateTime<Utc>,
    repeat: Duration,
    settle: Duration,
) -> Step {
    match now {
        CertificateNow::Hold => Step::Nothing,
        CertificateNow::Fine => step(stored, Now::Fine, at, repeat, settle),
        CertificateNow::At { stage, severity } => {
            let Some(old) = stored.condition.and(stored_stage) else { return Step::Fire };
            if stage < old {
                Step::Escalate
            } else if stored.ok_since.is_some() {
                Step::Reopen
            } else if stage > old {
                Step::Change
            } else if stage == 0 {
                step(stored, Now::Bad(Condition::Certificate, severity), at, repeat, settle)
            } else {
                Step::Nothing
            }
        }
    }
}

/// An alert as stored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stored {
    /// The bad state it is in; `None` when it's fine (or never was bad).
    pub condition: Option<Condition>,
    pub severity: Option<Severity>,
    /// Since when it is bad.
    pub since: Option<DateTime<Utc>>,
    /// Fine again since then, but not for `settle` yet.
    pub ok_since: Option<DateTime<Utc>>,
    /// When people were last told (or will be, after quiet hours).
    pub last_notified_at: Option<DateTime<Utc>>,
    pub acknowledged: bool,
}

/// What is true now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Now {
    Fine,
    Bad(Condition, Severity),
    /// Leave it as it is: a paused destination (its alert neither recovers nor reminds), or a
    /// state not watched long enough to judge.
    Hold,
}

/// What happens to an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// It turned bad: tell people.
    Fire,
    /// It got worse (a higher severity): tell people again, even if they acknowledged.
    Escalate,
    /// Still bad, in another way or less so: change it, tell nobody.
    Change,
    /// Bad again before it settled: the same alert goes on, nobody is told.
    Reopen,
    /// Fine, but not for long enough yet.
    Recovering,
    /// Fine for `settle`: it's over.
    Recover,
    /// Still bad and nobody acknowledged it for `repeat`.
    Remind,
}

impl Step {
    /// The event this step records (`alert_events.kind`), if any.
    pub fn event(self) -> Option<&'static str> {
        match self {
            Step::Fire => Some("fired"),
            Step::Escalate => Some("escalated"),
            Step::Recover => Some("recovered"),
            Step::Remind => Some("reminded"),
            Step::Nothing | Step::Change | Step::Reopen | Step::Recovering => None,
        }
    }
}

/// The next step for an alert.
pub fn step(stored: &Stored, now: Now, at: DateTime<Utc>, repeat: Duration, settle: Duration) -> Step {
    let elapsed = |t: DateTime<Utc>| (at - t).to_std().unwrap_or_default();
    match (now, stored.condition) {
        (Now::Hold, _) | (Now::Fine, None) => Step::Nothing,
        (Now::Bad(..), None) => Step::Fire,
        (Now::Bad(condition, severity), Some(old)) => {
            if Some(severity) > stored.severity {
                Step::Escalate
            } else if stored.ok_since.is_some() {
                Step::Reopen
            } else if condition != old || Some(severity) != stored.severity {
                Step::Change
            } else if !repeat.is_zero()
                && !stored.acknowledged
                && stored.last_notified_at.or(stored.since).is_some_and(|t| elapsed(t) >= repeat)
            {
                Step::Remind
            } else {
                Step::Nothing
            }
        }
        (Now::Fine, Some(_)) => match stored.ok_since {
            Some(since) if elapsed(since) >= settle => Step::Recover,
            None if settle.is_zero() => Step::Recover,
            None => Step::Recovering,
            Some(_) => Step::Nothing,
        },
    }
}

/// What an alert is about, for the words of a notification.
#[derive(Debug, Clone)]
pub struct Notice<'a> {
    /// `fired`, `escalated`, `reminded`, `recovered`, or `test`.
    pub kind: &'a str,
    pub id: i64,
    pub key: &'a str,
    pub interface: &'a str,
    pub destination: Option<&'a str>,
    pub condition: Option<Condition>,
    pub severity: Option<Severity>,
    /// Since when it was bad.
    pub since: Option<DateTime<Utc>>,
    pub at: DateTime<Utc>,
    /// The health view's words ("stalled: nothing sent for 12 min, 340 waiting; ...").
    pub summary: &'a str,
    pub environment: &'a str,
}

impl Notice<'_> {
    /// `adt-relay → lab`, or `adt-relay` (its own alert, or its only destination).
    pub fn subject(&self) -> String {
        match self.destination {
            Some(d) if d != crate::config::DEFAULT_DESTINATION => format!("{} → {d}", self.interface),
            _ => self.interface.to_string(),
        }
    }

    fn lasted(&self) -> String {
        let since = self.since.unwrap_or(self.at);
        crate::monitor::human((self.at - since).to_std().unwrap_or_default())
    }

    /// One line: an email subject, a chat message's first line.
    pub fn title(&self) -> String {
        let state = self.condition.map_or("", Condition::label);
        let severity = self.severity.map_or("", |s| s.label()).to_uppercase();
        let env = format!("[Ductus {}]", self.environment);
        if self.condition == Some(Condition::Certificate) {
            // The summary's first sentence says it all: "Certificate of x:443 (empi) expires in 14 days (…)".
            let what = self.summary.split(". ").next().unwrap_or(self.summary).trim_end_matches('.');
            return match self.kind {
                "recovered" => format!("{env} RECOVERED {what}"),
                "reminded" => format!("{env} {severity} still: {what}"),
                _ => format!("{env} {severity} {what}"),
            };
        }
        match self.kind {
            "recovered" => format!("{env} RECOVERED {}: no longer {state} (after {})", self.subject(), self.lasted()),
            "reminded" => format!("{env} {severity} still {state}: {} (for {})", self.subject(), self.lasted()),
            "escalated" => format!("{env} {severity} worse: {} is {state}", self.subject()),
            "test" => format!("{env} test notification"),
            _ => format!("{env} {severity} {} is {state}", self.subject()),
        }
    }

    /// The details, as plain text.
    pub fn body(&self) -> String {
        if self.kind == "test" {
            return format!(
                "This is a test from Ductus ({} environment), sent at {}. If you can read it, this channel works.\n",
                self.environment,
                local(self.at)
            );
        }
        if self.condition == Some(Condition::Certificate) {
            let todo = if self.kind == "recovered" {
                "It was renewed: nothing more to do."
            } else if self.summary.contains(" expired") {
                "Connections that check it fail until the server's owner renews it."
            } else {
                "Ask the server's owner to renew it before then; the engine's connections to it fail once it expires."
            };
            return format!(
                "{}\n\n{todo}\n\nAlert {} ({}), {} environment. The Connections page of the Ductus web UI lists every certificate; acknowledge or silence this alert on its Alerts page.\n",
                self.summary, self.id, self.key, self.environment
            );
        }
        let since = self.since.map(|t| format!(" since {}", local(t))).unwrap_or_default();
        let what = match self.kind {
            "recovered" => format!("{} is fine again (it was {}{since}).", self.subject(), self.state_text()),
            _ => format!("{} is {}{since}.", self.subject(), self.state_text()),
        };
        format!(
            "{what}\n\n{}\n\nAlert {} ({}), {} environment. Acknowledge or silence it on the Alerts page of the Ductus web UI.\n",
            self.summary, self.id, self.key, self.environment
        )
    }

    fn state_text(&self) -> String {
        match (self.condition, self.severity) {
            (Some(c), Some(s)) => format!("{} ({})", c.label(), s.label()),
            (Some(c), None) => c.label().into(),
            _ => "bad".into(),
        }
    }

    /// The request body a webhook gets.
    pub fn webhook_body(&self, format: WebhookFormat) -> serde_json::Value {
        match format {
            WebhookFormat::Json => json!({
                "kind": self.kind, "alert": self.id, "key": self.key, "interface": self.interface,
                "destination": self.destination, "state": self.condition.map(Condition::label),
                "severity": self.severity.map(Severity::label), "since": self.since.map(|t| t.to_rfc3339()),
                "at": self.at.to_rfc3339(),
                "summary": self.summary, "environment": self.environment, "title": self.title(),
            }),
            WebhookFormat::Slack => json!({ "text": format!("*{}*\n{}", self.title(), self.summary) }),
            WebhookFormat::Teams => json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard", "version": "1.4",
                        "body": [
                            { "type": "TextBlock", "text": self.title(), "weight": "Bolder", "wrap": true },
                            { "type": "TextBlock", "text": self.summary, "wrap": true },
                        ],
                    },
                }],
            }),
        }
    }
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn settings(yaml: &str) -> AlertSettings {
        let s: AlertSettings = serde_yaml_ng::from_str(yaml).unwrap();
        s.check().unwrap();
        s
    }

    const SAMPLE: &str = "
smtp: { host: smtp.example.org, from: ductus@example.org, username: d, password: p }
channels:
  oncall: { email: [oncall@example.org] }
  chat: { webhook: { url: 'https://hooks.example.org/T0/B0/secret', format: slack } }
rules:
  - { when: [error, stalled, stopped], severity: critical, to: [oncall, chat, owner] }
  - { when: [behind, silent, missed], severity: warning, to: [chat, owner] }
  - { when: [certificate], severity: warning, to: [oncall] }
";

    #[test]
    fn reads_the_settings_with_safe_defaults() {
        let s = settings(SAMPLE);
        assert_eq!((s.repeat.as_secs(), s.settle.as_secs(), s.keep.as_secs() / 86_400), (4 * 3600, 120, 365));
        assert!(s.recovery && s.quiet.is_none() && s.heartbeat.is_none());
        let smtp = s.smtp.as_ref().unwrap();
        assert_eq!((smtp.port(), smtp.tls), (587, SmtpTls::Starttls));
        assert!(!format!("{smtp:?}").contains("\"p\""), "the password isn't printed");
        let chat = s.channels["chat"].webhook.as_ref().unwrap();
        assert_eq!(chat.masked_url(), "https://hooks.example.org/…");
        assert!(!format!("{chat:?}").contains("secret"));
        assert_eq!(s.rule_for(Condition::Stalled).unwrap().severity, Severity::Critical);
        assert_eq!(s.rule_for(Condition::Silent).unwrap().severity, Severity::Warning);
        assert!(s.warnings().is_empty(), "{:?}", s.warnings());
        let s = settings(
            "repeat: '0'\nsettle: 0s\nchannels: { a: { webhook: { url: 'http://x.org/h' } } }\nrules: [{ when: [error], severity: critical, to: [a] }]",
        );
        assert!(s.repeat.is_zero() && s.settle.is_zero());
        assert_eq!(
            s.warnings(),
            ["alerts: no rule names stalled, behind, stopped, silent, missed, certificate: those states won't alert"]
        );
        assert_eq!(s.certificates.warn_days, [30, 14, 7]);
    }

    #[test]
    fn says_what_is_wrong_with_the_settings() {
        for (yaml, expected) in [
            ("channels: { a: {} }", "channel `a` needs `email` or `webhook`"),
            ("channels: { a: { email: nobody } }", "`nobody` isn't an email address"),
            ("channels: { a: { email: a@b.org } }", "email channels and `owner` need `smtp:`"),
            ("channels: { a: { webhook: { url: 'ftp://x' } } }", "must be an http(s) URL"),
            (
                "rules: [{ when: [error], severity: critical, to: [pager] }]",
                "no channel named `pager` (none are defined)",
            ),
            ("channels: { owner: { webhook: { url: 'http://x.org' } } }", "reserved"),
            ("repeat: 10s", "repeat must be at least 1m"),
            ("heartbeat: { url: 'http://x.org', every: 5s }", "at least 30s"),
            ("smtp: { host: h, from: me }", "isn't an email address"),
            ("certificates: { warn_days: [7, 14] }", "longest first"),
            ("certificates: { warn_days: [] }", "certificates.warn_days"),
            ("certificates: { warn_days: [400] }", "1 to 365 days"),
        ] {
            let s: AlertSettings = serde_yaml_ng::from_str(yaml).unwrap();
            let err = s.check().unwrap_err().to_string();
            assert!(err.contains(expected), "{yaml}: {err}");
        }
        assert!(
            serde_yaml_ng::from_str::<AlertSettings>("rules: [{ when: [paused], severity: warning, to: [] }]").is_err()
        );
        let s = settings(
            "channels: { a: { webhook: { url: 'http://x.org' } } }\nrules:\n  - { when: [error], severity: critical, to: [a] }\n  - { when: [error, behind], severity: warning, to: [a] }",
        );
        assert!(s.warnings().iter().any(|w| w.contains("rule 2 names `error`, but an earlier rule already does")));
        assert_eq!(
            s.check_interface("lab", &AlertOptions { to: vec!["a".into(), OWNER.into(), "x".into()], mute: false }),
            ["lab: alert.to names `x`, but alerts.channels has no such channel"]
        );
    }

    #[test]
    fn recipients_are_the_rule_s_then_the_interface_s_each_once() {
        let s = settings(SAMPLE);
        let rule = s.rule_for(Condition::Error).unwrap();
        let options = AlertOptions { to: vec!["chat".into(), "oncall".into()], mute: false };
        assert_eq!(recipients(rule, &options), ["oncall", "chat", "owner"]);
        let rule = s.rule_for(Condition::Behind).unwrap();
        assert_eq!(recipients(rule, &options), ["chat", "owner", "oncall"]);
    }

    #[test]
    fn finds_an_owner_s_address() {
        let people = [
            Person { username: "anna", display_name: Some("Anna Lee"), email: Some("anna@example.org") },
            Person { username: "bo", display_name: Some("Bo"), email: None },
        ];
        let mut directory = BTreeMap::new();
        directory.insert("Lab Systems".to_string(), OwnerContact { email: vec!["lab@example.org".into()] });
        let find = |owner: &str| owner_addresses(owner, &people, &directory);
        assert_eq!(find("anna").unwrap(), ["anna@example.org"]);
        assert_eq!(find(" anna lee ").unwrap(), ["anna@example.org"]);
        assert_eq!(find("ANNA@example.org").unwrap(), ["anna@example.org"]);
        assert_eq!(find("lab systems").unwrap(), ["lab@example.org"]);
        assert_eq!(find("someone@example.org").unwrap(), ["someone@example.org"]);
        assert_eq!(find("Bo"), None, "a user without an email");
        assert_eq!(find("Radiology"), None);
        assert_eq!(find(""), None);
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap() + chrono::Duration::minutes(minutes)
    }

    const REPEAT: Duration = Duration::from_secs(4 * 3600);
    const SETTLE: Duration = Duration::from_secs(120);

    /// Apply a step the way the engine stores it.
    fn apply(s: &mut Stored, now: Now, at: DateTime<Utc>) -> Step {
        let step = step(s, now, at, REPEAT, SETTLE);
        match (step, now) {
            (Step::Fire, Now::Bad(c, sev)) => {
                *s = Stored {
                    condition: Some(c),
                    severity: Some(sev),
                    since: Some(at),
                    last_notified_at: Some(at),
                    ..Default::default()
                }
            }
            (Step::Escalate, Now::Bad(c, sev)) => {
                (s.condition, s.severity, s.ok_since, s.acknowledged, s.last_notified_at) =
                    (Some(c), Some(sev), None, false, Some(at))
            }
            (Step::Change | Step::Reopen, Now::Bad(c, sev)) => {
                (s.condition, s.severity, s.ok_since) = (Some(c), Some(sev), None)
            }
            (Step::Recovering, _) => s.ok_since = Some(at),
            (Step::Recover, _) => *s = Stored::default(),
            (Step::Remind, _) => s.last_notified_at = Some(at),
            _ => {}
        }
        step
    }

    #[test]
    fn an_alert_fires_escalates_eases_and_recovers() {
        let mut s = Stored::default();
        let stalled = Now::Bad(Condition::Stalled, Severity::Critical);
        let behind = Now::Bad(Condition::Behind, Severity::Warning);
        assert_eq!(apply(&mut s, Now::Fine, t(0)), Step::Nothing);
        assert_eq!(apply(&mut s, behind, t(1)), Step::Fire);
        assert_eq!(apply(&mut s, behind, t(2)), Step::Nothing, "edge-triggered: told once");
        assert_eq!(apply(&mut s, stalled, t(3)), Step::Escalate);
        assert_eq!(apply(&mut s, behind, t(4)), Step::Change, "better but not fine: nobody is told");
        assert_eq!((s.condition, s.severity), (Some(Condition::Behind), Some(Severity::Warning)));
        assert_eq!(s.since, Some(t(1)), "the alert is still the one that fired");
        assert_eq!(apply(&mut s, Now::Hold, t(5)), Step::Nothing, "paused: left as it is");
        assert_eq!(apply(&mut s, Now::Fine, t(6)), Step::Recovering);
        assert_eq!(apply(&mut s, Now::Fine, t(7)), Step::Nothing, "fine, not for long enough");
        assert_eq!(apply(&mut s, Now::Fine, t(8)), Step::Recover);
        assert_eq!(s, Stored::default());
        assert_eq!(apply(&mut s, behind, t(9)), Step::Fire, "a new alert");
    }

    #[test]
    fn a_flapping_state_is_one_alert() {
        let mut s = Stored::default();
        let error = Now::Bad(Condition::Error, Severity::Critical);
        assert_eq!(apply(&mut s, error, t(0)), Step::Fire);
        assert_eq!(apply(&mut s, Now::Fine, t(1)), Step::Recovering);
        assert_eq!(apply(&mut s, error, t(2)), Step::Reopen, "bad again before it settled");
        assert_eq!(s.ok_since, None);
        assert_eq!(apply(&mut s, Now::Fine, t(3)), Step::Recovering);
        assert_eq!(apply(&mut s, Now::Fine, t(4)), Step::Nothing);
        assert_eq!(apply(&mut s, Now::Fine, t(5)), Step::Recover);
        let no_settle = step(
            &Stored { condition: Some(Condition::Error), ..Default::default() },
            Now::Fine,
            t(0),
            REPEAT,
            Duration::ZERO,
        );
        assert_eq!(no_settle, Step::Recover, "settle 0: recovered at once");
    }

    #[test]
    fn reminders_come_every_repeat_until_acknowledged() {
        let mut s = Stored::default();
        let error = Now::Bad(Condition::Error, Severity::Critical);
        apply(&mut s, error, t(0));
        assert_eq!(apply(&mut s, error, t(239)), Step::Nothing);
        assert_eq!(apply(&mut s, error, t(240)), Step::Remind);
        assert_eq!(apply(&mut s, error, t(300)), Step::Nothing);
        assert_eq!(apply(&mut s, error, t(480)), Step::Remind);
        s.acknowledged = true;
        assert_eq!(apply(&mut s, error, t(800)), Step::Nothing, "acknowledged: no reminders");
        let stalled = Now::Bad(Condition::Stalled, Severity::Critical);
        assert_eq!(apply(&mut s, stalled, t(801)), Step::Change, "the same severity isn't worse");
        assert!(s.acknowledged);
        let worse = Stored { severity: Some(Severity::Warning), ..s.clone() };
        assert_eq!(step(&worse, error, t(802), REPEAT, SETTLE), Step::Escalate, "worse ends the acknowledgement");
        let never = Stored { acknowledged: false, ..s };
        assert_eq!(step(&never, stalled, t(5000), Duration::ZERO, SETTLE), Step::Nothing, "repeat 0: never");
        let untold = Stored { last_notified_at: None, since: Some(t(0)), ..never };
        assert_eq!(step(&untold, stalled, t(240), REPEAT, SETTLE), Step::Remind, "silenced at first: reminded later");
    }

    #[test]
    fn quiet_hours_hold_warnings_until_they_end() {
        let s = settings(&format!("{SAMPLE}quiet: {{ between: '22:00-06:00' }}"));
        let at = |h: u32| Local.with_ymd_and_hms(2026, 10, 14, h, 30, 0).unwrap().with_timezone(&Utc);
        let morning = Local.with_ymd_and_hms(2026, 10, 15, 6, 0, 0).unwrap().with_timezone(&Utc);
        assert_eq!(s.quiet_until(Severity::Warning, at(23)), Some(morning));
        assert_eq!(s.quiet_until(Severity::Critical, at(23)), None, "critical never waits");
        assert_eq!(s.quiet_until(Severity::Warning, at(12)), None);
        let early = Local.with_ymd_and_hms(2026, 10, 15, 2, 0, 0).unwrap().with_timezone(&Utc);
        assert_eq!(s.quiet_until(Severity::Warning, early), Some(morning));
    }

    #[test]
    fn notifications_say_what_and_since_when() {
        let n = Notice {
            kind: "fired",
            id: 7,
            key: "dest:adt/lab",
            interface: "adt",
            destination: Some("lab"),
            condition: Some(Condition::Stalled),
            severity: Some(Severity::Critical),
            since: Some(t(0)),
            at: t(12),
            summary: "stalled: nothing sent for 12 min, 340 waiting",
            environment: "prod",
        };
        assert_eq!(n.title(), "[Ductus prod] CRITICAL adt → lab is stalled");
        assert!(n.body().contains("adt → lab is stalled (critical) since"), "{}", n.body());
        assert!(n.body().contains("340 waiting") && n.body().contains("Alert 7 (dest:adt/lab)"));
        let recovered = Notice { kind: "recovered", ..n.clone() };
        assert_eq!(recovered.title(), "[Ductus prod] RECOVERED adt → lab: no longer stalled (after 12 min)");
        assert!(recovered.body().contains("adt → lab is fine again (it was stalled (critical)"));
        assert_eq!(n.webhook_body(WebhookFormat::Json)["state"], "stalled");
        assert_eq!(n.webhook_body(WebhookFormat::Slack)["text"], format!("*{}*\n{}", n.title(), n.summary));
        let teams = n.webhook_body(WebhookFormat::Teams);
        assert_eq!(teams["attachments"][0]["content"]["type"], "AdaptiveCard");
        assert_eq!(teams["attachments"][0]["content"]["body"][0]["text"], n.title());
    }

    const WARN: [u32; 3] = [30, 14, 7];

    /// `days` whole days from `now`, at 09:00 UTC that day.
    fn ends_in(days: i64, now: DateTime<Utc>) -> DateTime<Utc> {
        let day = (now + chrono::Duration::days(days)).date_naive();
        day.and_hms_opt(9, 0, 0).unwrap().and_utc()
    }

    #[test]
    fn a_certificate_s_stage_is_the_closest_step_it_is_within() {
        let now = t(0); // 12:00 UTC
        let stage = |days: i64| certificate_stage(ends_in(days, now), now, &WARN);
        assert_eq!(
            [31, 30, 15, 14, 8, 7, 1].map(stage),
            [None, Some(30), Some(30), Some(14), Some(14), Some(7), Some(7)]
        );
        // Later today (whole days in UTC dates: 0 left, not expired yet), and past.
        let tonight = now + chrono::Duration::hours(6);
        assert_eq!(days_left(tonight, now), 0);
        assert_eq!(certificate_stage(tonight, now, &WARN), Some(7));
        assert_eq!(certificate_stage(now - chrono::Duration::seconds(1), now, &WARN), Some(0));
        assert_eq!(certificate_stage(now, now, &WARN), Some(0), "expired at its last second");
        assert_eq!(certificate_stage(ends_in(10, now), now, &[60]), Some(60), "one step of its own");
    }

    /// Apply a certificate step the way the engine stores it.
    fn apply_cert(s: &mut (Stored, Option<i32>), now: CertificateNow, at: DateTime<Utc>) -> Step {
        let step = certificate_step(&s.0, s.1, now, at, REPEAT, SETTLE);
        let stored = &mut s.0;
        match (step, now) {
            (Step::Fire, CertificateNow::At { stage, severity }) => {
                *s = (
                    Stored {
                        condition: Some(Condition::Certificate),
                        severity: Some(severity),
                        since: Some(at),
                        last_notified_at: Some(at),
                        ..Default::default()
                    },
                    Some(stage),
                )
            }
            (Step::Escalate, CertificateNow::At { stage, severity }) => {
                (stored.severity, stored.ok_since, stored.acknowledged, stored.last_notified_at) =
                    (Some(severity), None, false, Some(at));
                s.1 = Some(stage);
            }
            (Step::Change | Step::Reopen, CertificateNow::At { stage, severity }) => {
                (stored.severity, stored.ok_since) = (Some(severity), None);
                s.1 = Some(stage);
            }
            (Step::Recovering, _) => stored.ok_since = Some(at),
            (Step::Recover, _) => *s = (Stored::default(), None),
            (Step::Remind, _) => stored.last_notified_at = Some(at),
            _ => {}
        }
        step
    }

    #[test]
    fn a_certificate_alert_steps_31_30_14_7_expired_renewed() {
        let mut s = (Stored::default(), None);
        let at = |stage: Option<i32>| CertificateNow::of(stage, Severity::Warning);
        assert_eq!(apply_cert(&mut s, at(None), t(0)), Step::Nothing, "31 days: nothing yet");
        assert_eq!(apply_cert(&mut s, at(Some(30)), t(1)), Step::Fire);
        assert_eq!(apply_cert(&mut s, at(Some(30)), t(2)), Step::Nothing, "the same step: told once");
        assert_eq!(apply_cert(&mut s, at(Some(30)), t(600)), Step::Nothing, "no reminders before it expires");
        assert_eq!(apply_cert(&mut s, at(Some(14)), t(601)), Step::Escalate, "14 days: news, the same severity");
        assert_eq!(apply_cert(&mut s, at(Some(14)), t(602)), Step::Nothing);
        s.0.acknowledged = true;
        assert_eq!(apply_cert(&mut s, at(Some(7)), t(603)), Step::Escalate, "7 days: news even when acknowledged");
        assert!(!s.0.acknowledged, "a step ends the acknowledgement");
        assert_eq!(apply_cert(&mut s, at(Some(0)), t(604)), Step::Escalate, "expired");
        assert_eq!(s.0.severity, Some(Severity::Critical), "expired is critical whatever the rule says");
        assert_eq!(apply_cert(&mut s, at(Some(0)), t(605)), Step::Nothing);
        assert_eq!(apply_cert(&mut s, at(Some(0)), t(604 + 240)), Step::Remind, "an expired one reminds");
        assert_eq!(apply_cert(&mut s, at(None), t(900)), Step::Recovering, "renewed");
        assert_eq!(apply_cert(&mut s, at(None), t(902)), Step::Recover);
        assert_eq!(s, (Stored::default(), None));
        assert_eq!(apply_cert(&mut s, CertificateNow::Hold, t(903)), Step::Nothing, "never checked");
    }

    #[test]
    fn a_renewal_still_within_a_step_moves_it_quietly() {
        let mut s = (Stored::default(), None);
        let at = |stage: Option<i32>| CertificateNow::of(stage, Severity::Warning);
        apply_cert(&mut s, at(Some(30)), t(0));
        apply_cert(&mut s, at(Some(7)), t(1));
        assert_eq!(apply_cert(&mut s, at(Some(30)), t(2)), Step::Change, "a short renewal: nobody is told");
        assert_eq!(s.1, Some(30));
        assert_eq!(apply_cert(&mut s, at(Some(14)), t(3)), Step::Escalate, "and its own steps follow");
        // Fine for a moment (the check reached another server behind a balancer), then back.
        assert_eq!(apply_cert(&mut s, at(None), t(4)), Step::Recovering);
        assert_eq!(apply_cert(&mut s, at(Some(14)), t(5)), Step::Reopen);
        assert_eq!(apply_cert(&mut s, at(None), t(6)), Step::Recovering);
        assert_eq!(apply_cert(&mut s, at(Some(7)), t(7)), Step::Escalate, "closer while recovering: news");
    }

    #[test]
    fn certificate_notifications_say_how_long_is_left() {
        let now = t(0);
        let facts = CertificateFacts {
            resource: "empi",
            host: "empi.example.org",
            port: 443,
            not_after: ends_in(14, now),
            earliest: None,
            error: None,
        };
        let summary = certificate_summary(&facts, now);
        assert_eq!(summary, "Certificate of empi.example.org:443 (empi) expires in 14 days, on 2026-10-28.");
        let one = certificate_summary(&CertificateFacts { not_after: ends_in(1, now), ..facts.clone() }, now);
        assert!(one.contains("expires in 1 day, on"), "{one}");
        let expired = certificate_summary(&CertificateFacts { not_after: ends_in(-3, now), ..facts.clone() }, now);
        assert_eq!(expired, "Certificate of empi.example.org:443 (empi) expired on 2026-10-11 (3 days ago).");
        let chain = CertificateFacts { earliest: Some("Example Intermediate"), error: Some("refused"), ..facts };
        let chain = certificate_summary(&chain, now);
        assert!(chain.contains("Example Intermediate's (an intermediate)"), "{chain}");
        assert!(chain.contains("The last check failed (refused)"), "{chain}");

        let n = Notice {
            kind: "escalated",
            id: 9,
            key: "cert:empi:empi.example.org:443",
            interface: "empi",
            destination: Some("empi.example.org:443"),
            condition: Some(Condition::Certificate),
            severity: Some(Severity::Warning),
            since: Some(now),
            at: now,
            summary: &chain,
            environment: "prod",
        };
        assert_eq!(
            n.title(),
            "[Ductus prod] WARNING Certificate of empi.example.org:443 (empi) expires in 14 days, on 2026-10-28"
        );
        assert!(n.body().starts_with(&chain), "{}", n.body());
        assert!(n.body().contains("renew it before then"), "{}", n.body());
        let gone = Notice { summary: &expired, severity: Some(Severity::Critical), ..n.clone() };
        assert!(gone.title().starts_with("[Ductus prod] CRITICAL Certificate of"), "{}", gone.title());
        assert!(gone.body().contains("fail until the server's owner renews it"), "{}", gone.body());
        let renewed = Notice { kind: "recovered", summary: &summary, ..n };
        assert!(renewed.title().starts_with("[Ductus prod] RECOVERED Certificate of"), "{}", renewed.title());
        assert!(renewed.body().contains("It was renewed"));
    }
}
