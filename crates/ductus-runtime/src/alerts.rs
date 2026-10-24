//! Alerts while the engine runs (`alerts:` in ductus.yaml; the rules are `ductus_core::alerts`).
//!
//! Three tasks, all running whether or not anyone watches the UI:
//! - **evaluate** (every 15 s): each interface's and destination's health, as the API shows it,
//!   and each certificate's countdown (from its last check, `certificates.rs`), against its stored
//!   alert; a change (fired, escalated, reminded, recovered) is stored with its notifications in
//!   one transaction (`Store::change_alert`, compare-and-set, so two engines on one database
//!   record it once).
//! - **send**: claims due notifications from the outbox and sends each on its own (a mail server
//!   that hangs never holds up a webhook), retrying with back-off.
//! - **heartbeat**: calls a URL every so often, so an engine that is down is noticed by someone else.

use crate::control::Engine;
use crate::tls::{Certificates, Trust};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use ductus_core::alerts::{
    self, AlertOptions, AlertSettings, CertificateNow, Condition, Notice, Now, OWNER, OwnerContact, Person, Severity,
    Step,
};
use ductus_core::tls::ConnectTls;
use ductus_store::{AlertChange, AlertRow, NewEvent, OutboxItem};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// How often states are judged.
const EVERY: Duration = Duration::from_secs(15);
/// After the engine starts, states that need watching (stalled, behind, silent, missed) are
/// judged only once it has watched this long: a restart is not an alert storm.
const WATCH_FIRST: Duration = Duration::from_secs(60);
/// A notification claimed by an engine is its own for this long (then anyone may retry it).
const LEASE: Duration = Duration::from_secs(120);
/// How long one send may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// Tries per notification before it counts as failed (30 s, doubling, at most an hour apart:
/// about six hours in all).
const MAX_ATTEMPTS: i32 = 10;

/// The alerts of a running engine (or of `ductus alerts test`, which has no engine).
pub struct Alerts {
    pub settings: AlertSettings,
    pub owners: BTreeMap<String, OwnerContact>,
    environment: String,
    /// The mail server's TLS (its `ca` and `certificate`).
    smtp_trust: Trust,
    /// Built for the client certificate in use (its hash): rebuilt after a renewal.
    mailer: Mutex<Option<([u8; 32], AsyncSmtpTransport<Tokio1Executor>)>>,
    from: Option<Mailbox>,
    /// By the webhook's or heartbeat's `ca` and `certificate` (none: the platform's roots).
    http: HashMap<ConnectTls, reqwest::Client>,
    /// Wakes the sender when there is something new to send.
    wake: Notify,
    /// Failed sends per channel since the engine started (`ductus_alert_send_failures_total`).
    failures: Mutex<BTreeMap<String, u64>>,
}

impl Alerts {
    pub fn new(
        settings: AlertSettings,
        owners: BTreeMap<String, OwnerContact>,
        environment: &str,
        certificates: Arc<Certificates>,
    ) -> Result<Self> {
        let (smtp_trust, from) = match &settings.smtp {
            None => (Trust::default(), None),
            Some(smtp) => {
                let from: Mailbox = smtp.from.parse().with_context(|| format!("smtp.from `{}`", smtp.from))?;
                let trust = certificates.trust(&ConnectTls::new(&smtp.ca, &smtp.certificate)).context("alerts.smtp")?;
                (trust, Some(from))
            }
        };
        let mut http = HashMap::new();
        let hooks = settings
            .channels
            .values()
            .filter_map(|c| c.webhook.as_ref())
            .map(|w| ConnectTls::new(&w.ca, &w.certificate));
        let heartbeat = settings.heartbeat.iter().map(|h| ConnectTls::new(&h.ca, &h.certificate));
        for tls in hooks.chain(heartbeat).chain([ConnectTls::default()]) {
            if let std::collections::hash_map::Entry::Vacant(slot) = http.entry(tls) {
                let client = certificates.http(slot.key(), reqwest::Client::builder().timeout(SEND_TIMEOUT))?;
                slot.insert(client);
            }
        }
        let alerts = Self {
            settings,
            owners,
            environment: environment.to_string(),
            smtp_trust,
            mailer: Mutex::default(),
            from,
            http,
            wake: Notify::new(),
            failures: Mutex::default(),
        };
        alerts.mailer()?; // a mail server setting that can't be used stops the start
        Ok(alerts)
    }

    /// The mail transport (pooled), built again when the client certificate was renewed.
    fn mailer(&self) -> Result<Option<AsyncSmtpTransport<Tokio1Executor>>> {
        let Some(smtp) = &self.settings.smtp else { return Ok(None) };
        let pair = self.smtp_trust.client_pair().map(|p| p.pem());
        let hash = pair.as_ref().map(|(h, _)| *h).unwrap_or_default();
        let mut mailer = self.mailer.lock().unwrap();
        if let Some((built, transport)) = &*mailer
            && *built == hash
        {
            return Ok(Some(transport.clone()));
        }
        use ductus_core::alerts::SmtpTls;
        use lettre::transport::smtp::client::{Certificate, Identity, Tls, TlsParameters};
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host)
            .port(smtp.port())
            .timeout(Some(SEND_TIMEOUT));
        if smtp.tls != SmtpTls::None {
            // lettre takes certificates rather than rustls settings; with the platform verifier
            // feature it verifies as `Trust` does (the platform's roots plus these).
            let mut tls = TlsParameters::builder(smtp.host.clone());
            for root in self.smtp_trust.extra_roots() {
                tls = tls.add_root_certificate(Certificate::from_der(root.to_vec()).context("alerts.smtp: ca")?);
            }
            if let Some((_, pem)) = &pair {
                tls = tls.identify_with(Identity::from_pem(&pem.0, &pem.1).context("alerts.smtp: certificate")?);
            }
            let tls = tls.build_rustls().context("alerts.smtp: TLS")?;
            builder = builder.tls(if smtp.tls == SmtpTls::Tls { Tls::Wrapper(tls) } else { Tls::Required(tls) });
        }
        if let (Some(user), Some(password)) = (&smtp.username, &smtp.password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        let transport = builder.build();
        *mailer = Some((hash, transport.clone()));
        Ok(Some(transport))
    }

    /// The HTTP client for a webhook or the heartbeat.
    fn http_for(&self, ca: &Option<String>, certificate: &Option<String>) -> &reqwest::Client {
        let default = &self.http[&ConnectTls::default()];
        self.http.get(&ConnectTls::new(ca, certificate)).unwrap_or(default)
    }

    /// Failed sends per channel since the engine started.
    pub fn failures(&self) -> BTreeMap<String, u64> {
        self.failures.lock().unwrap().clone()
    }

    /// Where a delivery row's channel goes: `email:<address>` (an owner), `<channel>`, or
    /// `<channel>/email` and `<channel>/webhook` for a channel that has both (each its own row,
    /// so one failing doesn't send the other twice).
    fn targets(&self, channel: &str) -> Result<(Vec<String>, Option<&alerts::Webhook>)> {
        if let Some(address) = channel.strip_prefix("email:") {
            return Ok((vec![address.to_string()], None));
        }
        let (name, part) = match channel.rsplit_once('/') {
            Some((name, part @ ("email" | "webhook"))) => (name, Some(part)),
            _ => (channel, None),
        };
        let c =
            self.settings.channels.get(name).ok_or_else(|| anyhow!("channel `{name}` is no longer in ductus.yaml"))?;
        let email = if part != Some("webhook") { c.email.clone() } else { Vec::new() };
        let webhook = if part != Some("email") { c.webhook.as_ref() } else { None };
        Ok((email, webhook))
    }

    /// Delivery rows for a channel name (see `targets`).
    fn rows_for(&self, name: &str) -> Vec<String> {
        match self.settings.channels.get(name) {
            Some(c) if !c.email.is_empty() && c.webhook.is_some() => {
                vec![format!("{name}/email"), format!("{name}/webhook")]
            }
            Some(_) => vec![name.to_string()],
            None => Vec::new(),
        }
    }

    /// Send one notification to one channel now.
    pub async fn send(&self, channel: &str, notice: &Notice<'_>) -> Result<()> {
        let (emails, webhook) = self.targets(channel)?;
        if !emails.is_empty() {
            self.mail(&emails, notice).await?;
        }
        if let Some(hook) = webhook {
            self.post(hook, notice).await?;
        }
        Ok(())
    }

    async fn mail(&self, to: &[String], notice: &Notice<'_>) -> Result<()> {
        let (Some(mailer), Some(from)) = (self.mailer()?, &self.from) else {
            bail!("no `smtp:` in alerts (the mail server)");
        };
        let mut message = lettre::Message::builder().from(from.clone()).subject(notice.title());
        for address in to {
            let mailbox: Mailbox = address.parse().with_context(|| format!("`{address}` isn't an email address"))?;
            message = message.to(mailbox);
        }
        let message = message.header(ContentType::TEXT_PLAIN).body(notice.body())?;
        let smtp = self.settings.smtp.as_ref().map(|s| format!("{}:{}", s.host, s.port())).unwrap_or_default();
        mailer.send(message).await.with_context(|| format!("sending mail through {smtp}"))?;
        Ok(())
    }

    async fn post(&self, hook: &alerts::Webhook, notice: &Notice<'_>) -> Result<()> {
        let body = notice.webhook_body(hook.format);
        let response = self
            .http_for(&hook.ca, &hook.certificate)
            .post(&hook.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| anyhow!("calling {}: {}", hook.masked_url(), without_url(e)))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("{} answered {status}: {}", hook.masked_url(), text.chars().take(200).collect::<String>());
        }
        Ok(())
    }

    /// A test notification to a channel, sent at once (`POST /alerts/channels/{name}/test`,
    /// `ductus alerts test`).
    pub async fn test(&self, channel: &str) -> Result<()> {
        if !self.settings.channels.contains_key(channel) {
            let names: Vec<&str> = self.settings.channels.keys().map(String::as_str).collect();
            bail!("no channel named `{channel}` (defined: {})", names.join(", "));
        }
        let notice = Notice {
            kind: "test",
            id: 0,
            key: "test",
            interface: "test",
            destination: None,
            condition: None,
            severity: None,
            since: None,
            at: Utc::now(),
            summary: "This is a test notification.",
            environment: &self.environment,
        };
        for row in self.rows_for(channel) {
            self.send(&row, &notice).await?;
        }
        Ok(())
    }

    /// Who to send to for an alert: delivery rows (owners as `email:<address>`).
    fn channels(&self, recipients: &[&str], owner: &str, people: &[Person]) -> Vec<String> {
        let mut out = Vec::new();
        for to in recipients {
            let rows = if *to == OWNER {
                alerts::owner_addresses(owner, people, &self.owners)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| format!("email:{a}"))
                    .collect()
            } else {
                self.rows_for(to)
            };
            for row in rows {
                if !out.contains(&row) {
                    out.push(row);
                }
            }
        }
        out
    }
}

/// An error from reqwest without the URL (a webhook's URL is its secret).
fn without_url(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        text = format!("{text}: {s}");
        source = s.source();
    }
    text
}

/// Start judging, sending and the heartbeat. Ends with the engine.
pub async fn start(engine: &Arc<Engine>) {
    let Some(a) = engine.alerts.clone() else { return };
    let keys: Vec<(String, String, Option<String>)> = engine
        .interfaces
        .iter()
        .flat_map(|i| {
            let name = i.config.name.clone();
            std::iter::once((alerts::interface_key(&name), name.clone(), None)).chain(
                i.destinations
                    .iter()
                    .map(move |(d, _)| (alerts::destination_key(&name, d), name.clone(), Some(d.clone()))),
            )
        })
        .chain(engine.certificates.iter().map(|t| {
            // Ours is on this server, on as many ports as it is presented on.
            let place = match t.side {
                crate::certificates::Side::Ours => t.host.clone(),
                crate::certificates::Side::Remote => format!("{}:{}", t.host, t.port),
            };
            (t.key.clone(), t.resource.clone(), Some(place))
        }))
        .collect();
    if let Err(e) = engine.store.ensure_alerts(&keys).await {
        tracing::error!(error = %format!("{e:#}"), "could not set up alerts: none will be sent");
        return;
    }
    warn_unreachable_owners(engine, &a).await;
    tracing::info!(
        channels = a.settings.channels.len(),
        rules = a.settings.rules.len(),
        heartbeat = a.settings.heartbeat.is_some(),
        "alerts on"
    );
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(EVERY).await;
            let Some(engine) = weak.upgrade() else { return };
            let started = std::time::Instant::now();
            match evaluate(&engine).await {
                Ok(()) => tracing::debug!(took_ms = started.elapsed().as_secs_f64() * 1000.0, "alerts judged"),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "judging alerts failed; trying again"),
            }
        }
    });
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        loop {
            let Some(engine) = weak.upgrade() else { return };
            let Some(a) = engine.alerts.clone() else { return };
            if let Err(e) = send_due(&engine, &a).await {
                tracing::warn!(error = %format!("{e:#}"), "sending alert notifications failed; trying again");
            }
            drop(engine);
            tokio::select! {
                _ = a.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    });
    if let Some(heartbeat) = a.settings.heartbeat.clone() {
        let weak = Arc::downgrade(&a);
        tokio::spawn(async move {
            let mut failing = false;
            loop {
                let Some(a) = weak.upgrade() else { return };
                let http = a.http_for(&heartbeat.ca, &heartbeat.certificate);
                let result = http.get(&heartbeat.url).send().await.and_then(|r| r.error_for_status());
                match (result, failing) {
                    (Ok(_), true) => {
                        failing = false;
                        tracing::info!("heartbeat answered again");
                    }
                    (Err(e), false) => {
                        failing = true;
                        tracing::warn!(error = %without_url(e), "heartbeat call failed: the service watching it will raise its own alarm");
                    }
                    _ => {}
                }
                drop(a);
                tokio::time::sleep(heartbeat.every).await;
            }
        });
    }
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(90)).await;
        loop {
            let Some(engine) = weak.upgrade() else { return };
            if let Some(a) = &engine.alerts {
                match engine.store.trim_alerts(a.settings.keep, 5_000, 100).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(deleted = n, "deleted alert history past `alerts.keep`"),
                    Err(e) => tracing::error!(error = %format!("{e:#}"), "trimming alert history failed"),
                }
            }
            drop(engine);
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

/// Owners whose alerts reach nobody (no user with an email, not in `owners:`, not an address).
async fn warn_unreachable_owners(engine: &Engine, a: &Alerts) {
    if !a.settings.rules.iter().any(|r| r.to.iter().any(|t| t == OWNER))
        && !engine.interfaces.iter().any(|i| i.config.alert.to.iter().any(|t| t == OWNER))
    {
        return;
    }
    let users = engine.store.users().await.unwrap_or_default();
    let people = people(&users);
    let configs = engine.interfaces.iter().map(|i| i.config.as_ref());
    let mut owners: Vec<String> =
        ductus_core::owners::owned(configs, &engine.resources).into_iter().map(|o| o.owner).collect();
    owners.sort_by_key(|o| o.to_lowercase());
    owners.dedup_by(|x, y| ductus_core::owners::same(x, y));
    for owner in owners.iter().filter(|o| !o.is_empty()) {
        if alerts::owner_addresses(owner, &people, &a.owners).is_none() {
            tracing::warn!(%owner, "alerts for this owner reach nobody: no user with that name and an email, not in `owners:` in ductus.yaml, and not an email address");
        }
    }
}

/// The accounts as owners see them (disabled ones too: they still name someone, who gets nothing).
pub(crate) fn people(users: &[ductus_store::User]) -> Vec<Person<'_>> {
    fn some(s: &str) -> Option<&str> {
        (!s.trim().is_empty()).then_some(s)
    }
    users
        .iter()
        .map(|u| Person {
            username: &u.username,
            display_name: some(&u.display_name),
            email: some(&u.email),
            disabled: u.disabled_at.is_some() || u.directory_refused_at.is_some(),
        })
        .collect()
}

/// One thing that can alert, as it is now.
struct Judged {
    key: String,
    now: Judgement,
    summary: String,
    owner: String,
    /// Its interface's `alert:` options (none for a certificate of a resource).
    options: AlertOptions,
}

/// What is true now: a health state, or a certificate's countdown.
#[derive(Debug, Clone, Copy)]
enum Judgement {
    Health(Now),
    Certificate(CertificateNow),
}

impl Judgement {
    /// The bad state, its severity and (a certificate's) step.
    fn bad(self) -> (Option<Condition>, Option<Severity>, Option<i32>) {
        match self {
            Judgement::Health(Now::Bad(c, s)) => (Some(c), Some(s), None),
            Judgement::Certificate(CertificateNow::At { stage, severity }) => {
                (Some(Condition::Certificate), Some(severity), Some(stage))
            }
            _ => (None, None, None),
        }
    }
}

/// Judge every interface and destination once.
pub async fn evaluate(engine: &Engine) -> Result<()> {
    let Some(a) = &engine.alerts else { return Ok(()) };
    let snapshot = engine.snapshot().await?;
    let now = Utc::now();
    let watched = (now - engine.started_at).to_std().unwrap_or_default() >= WATCH_FIRST;
    let judge = |condition: Option<Condition>| match condition.and_then(|c| Some((c, a.settings.rule_for(c)?))) {
        Some((c, rule)) if watched || c.is_immediate() => Now::Bad(c, rule.severity),
        Some(_) => Now::Hold,
        None if watched => Now::Fine,
        None => Now::Hold,
    };
    let mut judged = Vec::new();
    for i in &engine.interfaces {
        let owner = ductus_core::about::given(&i.config.about.owner).unwrap_or("").to_string();
        let (state, summary) = engine.interface_state(i, now);
        judged.push(Judged {
            key: alerts::interface_key(&i.config.name),
            now: Judgement::Health(judge(Condition::of_interface(state))),
            summary,
            owner: owner.clone(),
            options: i.config.alert.clone(),
        });
        for (n, ((name, _), (_, d))) in i.destinations.iter().zip(i.config.destinations()).enumerate() {
            let diagnosis = engine.diagnose(i, n, &snapshot, now);
            let now = match diagnosis.state {
                ductus_core::monitor::State::Paused => Now::Hold,
                state => judge(Condition::of_destination(state)),
            };
            let owner = ductus_core::about::given(&d.owner).map_or_else(|| owner.clone(), str::to_string);
            judged.push(Judged {
                key: alerts::destination_key(&i.config.name, name),
                now: Judgement::Health(now),
                summary: diagnosis.summary,
                owner,
                options: i.config.alert.clone(),
            });
        }
    }
    judged.extend(judge_certificates(engine, a, now).await?);
    let rows: HashMap<String, AlertRow> =
        engine.store.alerts().await?.into_iter().map(|r| (r.key.clone(), r)).collect();
    let mut users = None;
    for j in judged {
        let Some(row) = rows.get(&j.key) else { continue }; // made at start; a new one at the next start
        let stored = stored(row);
        let step = match j.now {
            Judgement::Health(n) => alerts::step(&stored, n, now, a.settings.repeat, a.settings.settle),
            Judgement::Certificate(n) => {
                alerts::certificate_step(&stored, row.stage, n, now, a.settings.repeat, a.settings.settle)
            }
        };
        if matches!(step, Step::Nothing) {
            // Keep the words current for the UI (counts and ages change), quietly.
            if row.is_open() && row.summary.as_deref() != Some(&j.summary) {
                let change = keep(row, Some(&j.summary));
                engine.store.change_alert(row.id, row.rev, &change).await?;
            }
            continue;
        }
        let needs_people = matches!(step, Step::Fire | Step::Escalate | Step::Remind | Step::Recover);
        if needs_people && users.is_none() {
            users = Some(engine.store.users().await.unwrap_or_default());
        }
        let people = people(users.as_deref().unwrap_or_default());
        let change = change(engine, a, row, &j, step, now, &people).await?;
        if engine.store.change_alert(row.id, row.rev, &change).await? {
            if let Some(e) = &change.event {
                tracing::info!(alert = %j.key, kind = e.kind, state = e.state, notifications = e.deliveries.len(), "alert");
            }
            a.wake.notify_one();
        }
    }
    Ok(())
}

/// Each certificate the engine connects to, from its last check, and each it presents, from what
/// it loaded (no database read for those): its step, and the words.
async fn judge_certificates(engine: &Engine, a: &Alerts, now: DateTime<Utc>) -> Result<Vec<Judged>> {
    use crate::certificates::Side;
    if engine.certificates.is_empty() {
        return Ok(Vec::new());
    }
    let seen = if engine.certificates.iter().any(|t| t.side == Side::Remote) {
        engine.store.certificates().await?
    } else {
        Vec::new()
    };
    let rule = a.settings.rule_for(Condition::Certificate).map(|r| r.severity);
    let mut out = Vec::new();
    for t in &engine.certificates {
        let ours = crate::certificates::our_row(t, &engine.tls, now);
        let row = ours.as_ref().or_else(|| {
            seen.iter().find(|c| c.resource == t.resource && c.host == t.host && c.port == i32::from(t.port))
        });
        let presented = t.presented.join(", ");
        let options = t
            .interface
            .as_deref()
            .and_then(|i| engine.interface(i))
            .map(|i| i.config.alert.clone())
            .unwrap_or_default();
        let (judgement, summary) = match row.and_then(|r| Some((r, r.chain_min_not_after?))) {
            // Never read (not checked yet, or never reachable): nothing to count down from.
            None => {
                let why = row.and_then(|r| r.error.as_deref()).unwrap_or("not checked yet");
                (CertificateNow::Hold, format!("Certificate of {}:{} ({}): {why}.", t.host, t.port, t.resource))
            }
            Some((r, not_after)) => {
                let facts = alerts::CertificateFacts {
                    resource: &t.resource,
                    host: &t.host,
                    port: t.port,
                    not_after,
                    earliest: r.chain_min_subject.as_deref(),
                    // The dates are older than the last check only when it couldn't read them;
                    // ours are always current, and their error is a renewal that can't be used.
                    error: match t.side {
                        Side::Ours => r.error.as_deref(),
                        Side::Remote => r.error.as_deref().filter(|_| r.read_at.is_none_or(|read| read < r.checked_at)),
                    },
                    ours: (t.side == Side::Ours).then_some(presented.as_str()),
                };
                let summary = alerts::certificate_summary(&facts, now);
                let judgement = match rule {
                    Some(severity) => CertificateNow::of(
                        alerts::certificate_stage(not_after, now, &a.settings.certificates.warn_days),
                        severity,
                    ),
                    None => CertificateNow::Fine, // no rule names `certificate`
                };
                (judgement, summary)
            }
        };
        out.push(Judged {
            key: t.key.clone(),
            now: Judgement::Certificate(judgement),
            summary,
            owner: t.owner.clone(),
            options,
        });
    }
    Ok(out)
}

fn stored(row: &AlertRow) -> alerts::Stored {
    alerts::Stored {
        condition: Condition::parse(&row.state),
        severity: row.severity.as_deref().and_then(Severity::parse),
        since: row.since,
        ok_since: row.ok_since,
        last_notified_at: row.last_notified_at,
        acknowledged: row.ack_at.is_some(),
    }
}

/// The alert as it is, with new words.
fn keep<'a>(row: &'a AlertRow, summary: Option<&'a str>) -> AlertChange<'a> {
    AlertChange {
        state: &row.state,
        severity: row.severity.as_deref(),
        since: row.since,
        ok_since: row.ok_since,
        last_notified_at: row.last_notified_at,
        summary,
        stage: row.stage,
        fresh: false,
        unacknowledge: false,
        cancel_waiting: false,
        event: None,
    }
}

async fn change<'a>(
    engine: &Engine,
    a: &Alerts,
    row: &'a AlertRow,
    j: &'a Judged,
    step: Step,
    now: DateTime<Utc>,
    people: &[Person<'_>],
) -> Result<AlertChange<'a>> {
    let mut c = keep(row, Some(&j.summary));
    let (condition, severity, stage) = j.now.bad();
    // Who hears, and when: nobody while muted or silenced; warnings after quiet hours.
    let deliveries = |severity: Severity, condition: Condition| -> Vec<(String, DateTime<Utc>)> {
        if j.options.mute || row.is_silenced(now) {
            return Vec::new();
        }
        let Some(rule) = a.settings.rule_for(condition) else { return Vec::new() };
        let recipients = alerts::recipients(rule, &j.options);
        let due = a.settings.quiet_until(severity, now).unwrap_or(now);
        a.channels(&recipients, &j.owner, people).into_iter().map(|ch| (ch, due)).collect()
    };
    let label = |c: Option<Condition>| c.map_or("ok", Condition::label);
    match step {
        Step::Nothing => {}
        Step::Fire | Step::Escalate | Step::Remind => {
            let (Some(cond), Some(sev)) = (condition.or(Condition::parse(&row.state)), severity) else {
                return Ok(c);
            };
            let to = deliveries(sev, cond);
            c.state = label(Some(cond));
            c.severity = Some(sev.label());
            c.ok_since = None;
            c.stage = stage.or(row.stage);
            if step == Step::Fire {
                c.since = Some(now);
                c.fresh = true;
            }
            c.unacknowledge = step == Step::Escalate;
            if let Some((_, due)) = to.first() {
                c.last_notified_at = Some(*due);
            } else if step == Step::Remind {
                c.last_notified_at = Some(now); // muted or silenced: the next reminder is a repeat away
            }
            let kind = step.event().unwrap_or("fired");
            c.event = Some(NewEvent { kind, state: c.state, severity: c.severity, since: c.since, deliveries: to });
        }
        Step::Change | Step::Reopen => {
            c.state = label(condition);
            c.severity = severity.map(Severity::label);
            c.ok_since = None;
            c.stage = stage;
        }
        Step::Recovering => c.ok_since = Some(now),
        Step::Recover => {
            let was = Condition::parse(&row.state);
            let told = a.settings.recovery && engine.store.alert_was_told(row.id).await?;
            let to = match (was, row.severity.as_deref().and_then(Severity::parse)) {
                (Some(cond), Some(sev)) if told => deliveries(sev, cond),
                _ => Vec::new(),
            };
            c.state = "ok";
            c.severity = None;
            c.stage = None;
            c.ok_since = None;
            c.last_notified_at = None;
            c.fresh = true;
            c.cancel_waiting = true;
            c.since = None;
            c.event = Some(NewEvent {
                kind: "recovered",
                state: &row.state,
                severity: row.severity.as_deref(),
                since: row.since,
                deliveries: to,
            });
        }
    }
    Ok(c)
}

/// Send what's due, each notification on its own task.
async fn send_due(engine: &Engine, a: &Arc<Alerts>) -> Result<()> {
    loop {
        let claimed = engine.store.claim_alert_deliveries(20, LEASE).await?;
        if claimed.is_empty() {
            return Ok(());
        }
        let mut tasks = Vec::new();
        for item in claimed {
            let (a, store, environment) = (a.clone(), engine.store.clone(), engine.environment.clone());
            tasks.push(tokio::spawn(async move { deliver(&a, &store, &environment, item).await }));
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

async fn deliver(a: &Alerts, store: &ductus_store::Store, environment: &str, item: OutboxItem) {
    let notice = Notice {
        kind: &item.kind,
        id: i64::from(item.alert_id),
        key: &item.key,
        interface: &item.interface,
        destination: item.destination.as_deref(),
        condition: item.state.as_deref().and_then(Condition::parse),
        severity: item.severity.as_deref().and_then(Severity::parse),
        since: item.since,
        at: item.at,
        summary: item.summary.as_deref().unwrap_or(""),
        environment,
    };
    let result = match tokio::time::timeout(SEND_TIMEOUT, a.send(&item.channel, &notice)).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("took longer than {}s", SEND_TIMEOUT.as_secs())),
    };
    let saved = match result {
        Ok(()) => store.alert_delivery_sent(item.id).await,
        Err(e) => {
            let error = format!("{e:#}");
            let channel = if item.channel.starts_with("email:") { OWNER } else { &item.channel };
            *a.failures.lock().unwrap().entry(channel.to_string()).or_default() += 1;
            let retry = (item.attempts < MAX_ATTEMPTS).then(|| Utc::now() + backoff(item.attempts));
            tracing::warn!(channel = %item.channel, alert = %item.key, attempt = item.attempts, error, gave_up = retry.is_none(), "alert notification failed");
            store.alert_delivery_failed(item.id, &error, retry).await
        }
    };
    if let Err(e) = saved {
        // The lease runs out and it is sent again: at least once.
        tracing::error!(error = %format!("{e:#}"), "could not record a notification's outcome");
    }
}

/// 30 s after the first failed try, doubling, at most an hour.
fn backoff(attempts: i32) -> chrono::Duration {
    let secs = 30i64.saturating_mul(1 << attempts.clamp(1, 8).saturating_sub(1)).min(3600);
    chrono::Duration::seconds(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ductus_core::alerts::WebhookFormat;

    #[test]
    fn retries_back_off_to_an_hour() {
        let s: Vec<i64> = (1..=10).map(|n| backoff(n).num_seconds()).collect();
        assert_eq!(s, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
    }

    fn alerts(yaml: &str) -> Alerts {
        let settings: AlertSettings = serde_yaml_ng::from_str(yaml).unwrap();
        settings.check().unwrap();
        let mut owners = BTreeMap::new();
        owners.insert("Lab".to_string(), OwnerContact { email: vec!["lab@example.org".into()] });
        Alerts::new(settings, owners, "test", Default::default()).unwrap()
    }

    /// (lettre's connection pool needs a Tokio runtime, as in the engine.)
    #[tokio::test]
    async fn a_channel_with_email_and_a_webhook_is_two_rows() {
        let a = alerts(
            "smtp: { host: localhost, tls: none, from: d@example.org }
channels:
  both: { email: [a@example.org], webhook: { url: 'http://127.0.0.1:9/h' } }
  hook: { webhook: { url: 'http://127.0.0.1:9/h', format: teams } }
rules: [{ when: [error], severity: critical, to: [both, hook, owner] }]",
        );
        let anna = [Person { username: "anna", display_name: None, email: Some("anna@example.org"), disabled: false }];
        assert_eq!(
            a.channels(&["both", "hook", OWNER, "hook"], "Lab", &anna),
            ["both/email", "both/webhook", "hook", "email:lab@example.org"]
        );
        assert_eq!(a.channels(&[OWNER], "anna", &anna), ["email:anna@example.org"]);
        assert!(a.channels(&[OWNER], "nobody", &anna).is_empty());
        let (emails, hook) = a.targets("both/email").unwrap();
        assert_eq!((emails.len(), hook.is_none()), (1, true));
        let (emails, hook) = a.targets("both/webhook").unwrap();
        assert_eq!((emails.len(), hook.map(|h| h.format)), (0, Some(WebhookFormat::Json)));
        assert!(a.targets("gone").unwrap_err().to_string().contains("no longer in ductus.yaml"));
    }
}
