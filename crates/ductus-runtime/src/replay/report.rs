//! The replay's comparison and its report: per interface and destination, how many messages came
//! out the same, which differ and where (HL7 fields, JSON paths, lines), which can't be compared and
//! why. Three things are compared, whichever exist: the other version run beside this one, what the
//! destination recorded (its decision), and what it kept (`save_output`).
//!
//! A difference from the other version is the change being judged. A difference from the record or a
//! kept output that the other version shares is drift (the data a lookup reads, a setting): shown,
//! but it doesn't fail the replay. Without another version, record and kept outputs are the evidence.

use super::{Decision, Event, Row};
use chrono::{DateTime, Utc};
use ductus_core::format::Format;
use ductus_store::Status;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Examples kept per difference.
const EXAMPLES: usize = 3;
/// Longest value shown in an example.
const SHOWN: usize = 120;
/// Most differences listed for one message and destination.
const PER_MESSAGE: usize = 50;

/// How a destination's output is compared: HL7 by field, JSON by path, anything else by line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Hl7,
    Json,
    #[default]
    Text,
}

impl OutputKind {
    pub fn of(format: &Format) -> Self {
        match format {
            Format::Hl7 => OutputKind::Hl7,
            Format::Json => OutputKind::Json,
            _ => OutputKind::Text,
        }
    }
}

/// The version compared against, run beside this one.
#[derive(Debug, Clone, Serialize)]
pub struct Other {
    /// In the report's words: "0.6.0 (installed)", "the project at main~3".
    pub label: String,
    pub version: String,
    pub binary: String,
    pub project: String,
}

/// What the other version did with a row this version produced.
pub enum OtherSide<'a> {
    /// No other version runs, or it didn't replay this interface.
    NotRun,
    /// It replayed the interface but has no such row (a destination it doesn't have).
    Missing,
    Row(&'a Row),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Against {
    Other,
    Record,
    Stored,
}

impl Against {
    fn name(self) -> &'static str {
        match self {
            Against::Other => "other",
            Against::Record => "record",
            Against::Stored => "stored",
        }
    }
}

#[derive(Default)]
struct Group {
    count: u64,
    /// Differences from the record or a kept output the other version shares.
    drift: bool,
    examples: Vec<(i64, String, String)>,
}

#[derive(Default)]
struct Destination {
    output: OutputKind,
    compared: u64,
    identical: u64,
    different: u64,
    drift: u64,
    not_comparable: u64,
    waiting: u64,
    looked_up: u64,
    groups: BTreeMap<(Against, String), Group>,
    /// Why messages weren't comparable: how many, the first few ids.
    reasons: BTreeMap<String, (u64, Vec<i64>)>,
}

#[derive(Default)]
struct Interface {
    skipped: Option<String>,
    other_skipped: Option<String>,
    messages: u64,
    seconds: f64,
    destinations: BTreeMap<String, Destination>,
}

pub struct Report {
    pub version: String,
    pub environment: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub other: Option<Other>,
    /// What the reader should know about how it was compared.
    pub notes: Vec<String>,
    /// Field values left out of examples (the report then holds no patient data).
    pub hide_values: bool,
    pub seconds: f64,
    interfaces: BTreeMap<String, Interface>,
}

impl Report {
    pub fn new(
        environment: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        other: Option<Other>,
        hide_values: bool,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            environment: environment.into(),
            from,
            to,
            other,
            notes: Vec::new(),
            hide_values,
            seconds: 0.0,
            interfaces: BTreeMap::new(),
        }
    }

    /// This version's events other than rows (`add` takes those).
    pub fn event(&mut self, event: &Event) {
        match event {
            Event::Interface { interface, skipped, outputs } => {
                let entry = self.interfaces.entry(interface.clone()).or_default();
                entry.skipped = skipped.clone();
                for (name, kind) in outputs {
                    entry.destinations.entry(name.clone()).or_default().output = *kind;
                }
            }
            Event::Done { interface, messages, seconds } => {
                let entry = self.interfaces.entry(interface.clone()).or_default();
                entry.messages = *messages;
                entry.seconds = *seconds;
            }
            Event::Start { .. } | Event::Row(_) => {}
        }
    }

    /// The other version's: an interface it couldn't replay.
    pub fn other_event(&mut self, event: &Event) {
        if let Event::Interface { interface, skipped: Some(why), .. } = event
            && let Some(entry) = self.interfaces.get_mut(interface)
        {
            entry.other_skipped = Some(why.clone());
        }
    }

    fn other_label(&self) -> String {
        self.other.as_ref().map_or_else(|| "the other version".into(), |o| o.label.clone())
    }

    /// Compare one row of this version with the other version's, the record and the kept output.
    pub fn add(&mut self, row: &Row, other: OtherSide) {
        let label = self.other_label();
        let hide = self.hide_values;
        let d = self
            .interfaces
            .entry(row.interface.clone())
            .or_default()
            .destinations
            .entry(row.destination.clone())
            .or_default();
        d.compared += 1;
        if row.looked_up {
            d.looked_up += 1;
        }
        if let Decision::NotComparable { why } = &row.decision {
            return d.not_comparable(why.clone(), row.id);
        }
        let mut found: Vec<(Against, String, String, String)> = Vec::new();
        let ran = !matches!(other, OtherSide::NotRun);
        match other {
            OtherSide::NotRun => {}
            OtherSide::Missing => found.push((
                Against::Other,
                format!("not a destination in {label}"),
                "(none)".into(),
                summary(&row.decision),
            )),
            OtherSide::Row(theirs) => {
                if let Decision::NotComparable { why } = &theirs.decision {
                    return d.not_comparable(format!("in {label}: {why}"), row.id);
                }
                for (what, before, after) in compare(&theirs.decision, &row.decision, d.output) {
                    found.push((Against::Other, what, before, after));
                }
            }
        }
        match &row.recorded {
            None if !ran => {
                return d.not_comparable("no record: the destination is newer than the message".into(), row.id);
            }
            None => {}
            Some(r) if r.status == Status::Pending => {
                if !ran {
                    d.waiting += 1;
                    return;
                }
            }
            Some(r) => {
                if let Some(then) = recorded(r) {
                    let now = Recorded::of(&row.decision);
                    if then != now {
                        found.push((Against::Record, "decision".into(), then.to_string(), now.to_string()));
                    }
                }
                if let (Some(kept), Decision::Sent { output }) = (&r.output, &row.decision) {
                    let kept = String::from_utf8_lossy(kept);
                    for (what, before, after) in differences(d.output, &kept, output) {
                        found.push((Against::Stored, what, before, after));
                    }
                }
            }
        }
        let drift = ran && !found.iter().any(|(against, ..)| *against == Against::Other);
        if found.is_empty() {
            d.identical += 1;
        } else if drift {
            d.drift += 1;
        } else {
            d.different += 1;
        }
        for (against, what, before, after) in found {
            let group = d.groups.entry((against, what)).or_default();
            group.count += 1;
            group.drift |= drift;
            if group.examples.len() < EXAMPLES {
                group.examples.push((row.id, shown(&before, hide), shown(&after, hide)));
            }
        }
    }

    /// A row only the other version produced: a destination this version doesn't have.
    pub fn only_other(&mut self, row: &Row) {
        let what = format!("only in {}: not a destination in this version", self.other_label());
        let hide = self.hide_values;
        let d = self
            .interfaces
            .entry(row.interface.clone())
            .or_default()
            .destinations
            .entry(row.destination.clone())
            .or_default();
        d.compared += 1;
        d.different += 1;
        let group = d.groups.entry((Against::Other, what)).or_default();
        group.count += 1;
        if group.examples.len() < EXAMPLES {
            group.examples.push((row.id, shown(&summary(&row.decision), hide), "(none)".into()));
        }
    }

    /// Message-destination pairs that differ in a way that fails the replay.
    pub fn differences(&self) -> u64 {
        self.destinations().map(|(_, _, d)| d.different).sum()
    }

    /// Interfaces this version couldn't load (the engine wouldn't start).
    fn broken(&self) -> Vec<(&str, &str)> {
        self.interfaces
            .iter()
            .filter_map(|(name, i)| {
                i.skipped.as_deref().filter(|s| s.starts_with("its code")).map(|s| (name.as_str(), s))
            })
            .collect()
    }

    pub fn ok(&self) -> bool {
        self.differences() == 0 && self.broken().is_empty()
    }

    fn destinations(&self) -> impl Iterator<Item = (&str, &str, &Destination)> {
        self.interfaces
            .iter()
            .flat_map(|(i, interface)| interface.destinations.iter().map(move |(n, d)| (i.as_str(), n.as_str(), d)))
    }

    fn total(&self, count: impl Fn(&Destination) -> u64) -> u64 {
        self.destinations().map(|(_, _, d)| count(d)).sum()
    }

    /// The report as JSON, with a checksum of the rest of it (what a change board signs).
    pub fn document(&self) -> Value {
        let interfaces: Vec<Value> = self
            .interfaces
            .iter()
            .map(|(name, i)| {
                let destinations: Vec<Value> = i
                    .destinations
                    .iter()
                    .map(|(name, d)| {
                        let groups: Vec<Value> = d
                            .groups
                            .iter()
                            .map(|((against, what), g)| {
                                let examples: Vec<Value> = g
                                    .examples
                                    .iter()
                                    .map(|(id, before, after)| json!({ "id": id, "before": before, "after": after }))
                                    .collect();
                                json!({ "against": against.name(), "what": what, "count": g.count, "drift": g.drift, "examples": examples })
                            })
                            .collect();
                        let reasons: Vec<Value> = d
                            .reasons
                            .iter()
                            .map(|(why, (count, ids))| json!({ "why": why, "count": count, "ids": ids }))
                            .collect();
                        json!({
                            "name": name, "output": d.output, "compared": d.compared, "identical": d.identical,
                            "different": d.different, "drift": d.drift, "not_comparable": d.not_comparable,
                            "waiting": d.waiting, "looked_up": d.looked_up, "differences": groups,
                            "not_comparable_reasons": reasons,
                        })
                    })
                    .collect();
                json!({
                    "name": name, "messages": i.messages, "seconds": round(i.seconds), "skipped": i.skipped,
                    "other_skipped": i.other_skipped, "destinations": destinations,
                })
            })
            .collect();
        let mut document = json!({
            "ductus_replay_report": 1,
            "version": self.version,
            "other": self.other,
            "environment": self.environment,
            "from": self.from.to_rfc3339(),
            "to": self.to.to_rfc3339(),
            "ok": self.ok(),
            "messages": self.interfaces.values().map(|i| i.messages).sum::<u64>(),
            "compared": self.total(|d| d.compared),
            "identical": self.total(|d| d.identical),
            "different": self.total(|d| d.different),
            "drift": self.total(|d| d.drift),
            "not_comparable": self.total(|d| d.not_comparable),
            "waiting": self.total(|d| d.waiting),
            "values_hidden": self.hide_values,
            "notes": self.notes,
            "seconds": round(self.seconds),
            "interfaces": interfaces,
        });
        use sha2::Digest;
        let digest = sha2::Sha256::digest(document.to_string().as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        document["checksum"] = format!("sha256:{hex}").into();
        document
    }

    /// For the terminal: one line per destination, the differences under it.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let against = match &self.other {
            Some(o) => format!("against {}", o.label),
            None => "against the record and kept outputs".into(),
        };
        let _ = writeln!(
            out,
            "ductus replay: version {} {against}, {} to {} UTC, environment {} (nothing was sent)",
            self.version,
            self.from.format("%Y-%m-%d %H:%M"),
            self.to.format("%Y-%m-%d %H:%M"),
            self.environment
        );
        for note in &self.notes {
            let _ = writeln!(out, "note: {note}");
        }
        for (name, i) in &self.interfaces {
            let _ = writeln!(out, "\n{name}: {} message(s) in {:.1} s", number(i.messages), i.seconds);
            if let Some(why) = &i.skipped {
                let mark = if why.starts_with("its code") { "✗" } else { "note:" };
                let _ = writeln!(out, "  {mark} not replayed: {why}");
            }
            if let Some(why) = &i.other_skipped {
                let _ = writeln!(out, "  note: {} didn't replay it: {why}", self.other_label());
            }
            for (destination, d) in &i.destinations {
                let mark = if d.different > 0 {
                    "✗"
                } else if d.not_comparable > 0 || d.drift > 0 {
                    "⚠"
                } else {
                    "✓"
                };
                let _ = writeln!(out, "  {mark} {destination}: {}", counts(d));
                for ((against, what), g) in &d.groups {
                    let vs = match against {
                        Against::Other => self.other_label(),
                        Against::Record => "the record".into(),
                        Against::Stored => "the kept output".into(),
                    };
                    let drift = if g.drift { " (drift: the other version does the same)" } else { "" };
                    let example = g.examples.first().map(|(id, before, after)| {
                        if self.hide_values {
                            format!(" (e.g. message {id})")
                        } else {
                            format!(" (e.g. message {id}: {before:?} → {after:?})")
                        }
                    });
                    let _ = writeln!(
                        out,
                        "      vs {vs}: {what} differs in {} message(s){}{drift}",
                        number(g.count),
                        example.unwrap_or_default()
                    );
                }
                for (why, (count, ids)) in &d.reasons {
                    let _ =
                        writeln!(out, "      not comparable in {} message(s) (e.g. {}): {why}", number(*count), ids[0]);
                }
                if d.looked_up > 0 && (d.different > 0 || d.drift > 0) {
                    let _ = writeln!(
                        out,
                        "      its code looked something up in {} message(s): a difference from the record may be the data, not the code",
                        number(d.looked_up)
                    );
                }
            }
        }
        let _ = writeln!(
            out,
            "\n{} message(s), {} comparison(s): {} identical, {} different, {} drift, {} not comparable, {} waiting ({:.1} s)",
            number(self.interfaces.values().map(|i| i.messages).sum()),
            number(self.total(|d| d.compared)),
            number(self.total(|d| d.identical)),
            number(self.total(|d| d.different)),
            number(self.total(|d| d.drift)),
            number(self.total(|d| d.not_comparable)),
            number(self.total(|d| d.waiting)),
            self.seconds
        );
        let broken = self.broken();
        if !broken.is_empty() {
            let _ =
                writeln!(out, "{} interface(s) don't load in this version: the engine wouldn't start", broken.len());
        }
        out
    }

    /// A page a change board can read, print and sign: the summary and checksum first.
    pub fn html(&self) -> String {
        let document = self.document();
        let checksum = document["checksum"].as_str().unwrap_or_default();
        let mut out = String::new();
        let compared = self.other.as_ref().map_or_else(
            || "the record and kept outputs only".to_string(),
            |o| format!("{} (binary {}, project {})", o.label, o.binary, o.project),
        );
        let result =
            if self.ok() { "identical: nothing differs from what it's compared with" } else { "differences found" };
        let _ = write!(
            out,
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>Replay report</title><style>{CSS}</style></head><body>\n<h1>Replay report</h1>\n<table class=\"summary\">"
        );
        let rows = [
            ("Version replayed", self.version.clone()),
            ("Compared with", compared),
            (
                "Period (UTC)",
                format!("{} to {}", self.from.format("%Y-%m-%d %H:%M:%S"), self.to.format("%Y-%m-%d %H:%M:%S")),
            ),
            ("Environment", self.environment.clone()),
            ("Interfaces", number(self.interfaces.len() as u64)),
            ("Messages", number(self.interfaces.values().map(|i| i.messages).sum())),
            ("Comparisons (message × destination)", number(self.total(|d| d.compared))),
            ("Identical", number(self.total(|d| d.identical))),
            ("Different", number(self.total(|d| d.different))),
            ("Drift (the other version does the same)", number(self.total(|d| d.drift))),
            ("Not comparable", number(self.total(|d| d.not_comparable))),
            ("Waiting (not sent yet)", number(self.total(|d| d.waiting))),
            ("Result", result.to_string()),
            ("Took", format!("{:.1} s", self.seconds)),
            ("Checksum of the JSON report", checksum.to_string()),
        ];
        for (k, v) in rows {
            let _ = write!(out, "<tr><th>{}</th><td>{}</td></tr>", escape(k), escape(&v));
        }
        out.push_str("</table>\n");
        if !self.hide_values {
            out.push_str("<p class=\"note\">Examples show field values from real messages: keep this report as you keep patient data, or make it with <code>--hide-values</code>.</p>\n");
        }
        for note in &self.notes {
            let _ = writeln!(out, "<p class=\"note\">{}</p>", escape(note));
        }
        for (name, i) in &self.interfaces {
            let _ =
                writeln!(out, "<h2>{}</h2><p>{} message(s), {:.1} s</p>", escape(name), number(i.messages), i.seconds);
            if let Some(why) = &i.skipped {
                let _ = writeln!(out, "<p class=\"note\">Not replayed: {}</p>", escape(why));
            }
            if let Some(why) = &i.other_skipped {
                let _ = writeln!(
                    out,
                    "<p class=\"note\">{} didn't replay it: {}</p>",
                    escape(&self.other_label()),
                    escape(why)
                );
            }
            if i.destinations.is_empty() {
                continue;
            }
            out.push_str("<table><tr><th>Destination</th><th>Compared</th><th>Identical</th><th>Different</th><th>Drift</th><th>Not comparable</th><th>Waiting</th></tr>");
            for (destination, d) in &i.destinations {
                let _ = write!(
                    out,
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    escape(destination),
                    number(d.compared),
                    number(d.identical),
                    if d.different > 0 { "bad" } else { "" },
                    number(d.different),
                    number(d.drift),
                    number(d.not_comparable),
                    number(d.waiting)
                );
            }
            out.push_str("</table>\n");
            for (destination, d) in &i.destinations {
                if d.groups.is_empty() && d.reasons.is_empty() {
                    continue;
                }
                let _ = writeln!(out, "<h3>{} → {}</h3>", escape(name), escape(destination));
                if !d.groups.is_empty() {
                    out.push_str(
                        "<table><tr><th>Compared with</th><th>What</th><th>Messages</th><th>Examples</th></tr>",
                    );
                    for ((against, what), g) in &d.groups {
                        let vs = match against {
                            Against::Other => self.other_label(),
                            Against::Record => "the record".into(),
                            Against::Stored => "the kept output".into(),
                        };
                        let vs = if g.drift { format!("{vs} (drift)") } else { vs };
                        let examples: Vec<String> = g
                            .examples
                            .iter()
                            .map(|(id, before, after)| {
                                if self.hide_values {
                                    format!("message {id}")
                                } else {
                                    format!(
                                        "message {id}: <code>{}</code> → <code>{}</code>",
                                        escape(before),
                                        escape(after)
                                    )
                                }
                            })
                            .collect();
                        let _ = write!(
                            out,
                            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                            escape(&vs),
                            escape(what),
                            number(g.count),
                            examples.join("<br>")
                        );
                    }
                    out.push_str("</table>\n");
                }
                for (why, (count, ids)) in &d.reasons {
                    let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
                    let _ = writeln!(
                        out,
                        "<p class=\"note\">Not comparable in {} message(s) (e.g. {}): {}</p>",
                        number(*count),
                        ids.join(", "),
                        escape(why)
                    );
                }
                if d.looked_up > 0 && (d.different > 0 || d.drift > 0) {
                    let _ = writeln!(
                        out,
                        "<p class=\"note\">Its code looked something up in {} message(s): a difference from the record may be the data, not the code.</p>",
                        number(d.looked_up)
                    );
                }
            }
        }
        out.push_str("</body></html>\n");
        out
    }
}

impl Destination {
    fn not_comparable(&mut self, why: String, id: i64) {
        self.not_comparable += 1;
        let (count, ids) = self.reasons.entry(why).or_default();
        *count += 1;
        if ids.len() < EXAMPLES {
            ids.push(id);
        }
    }
}

const CSS: &str = "body{font:15px/1.5 system-ui,sans-serif;max-width:70rem;margin:2rem auto;padding:0 1rem;color:#1a1a1a;background:#fff}\
table{border-collapse:collapse;margin:.5rem 0 1.5rem;width:100%}th,td{border:1px solid #bbb;padding:.3rem .5rem;text-align:left;vertical-align:top}\
th{background:#f0f0f0}.summary th{width:22rem}code{font-size:13px;word-break:break-all}.note{color:#444}.bad{color:#a00;font-weight:600}\
h2{margin-top:2rem;border-bottom:1px solid #bbb}@media print{body{margin:0;max-width:none}h2{page-break-before:auto}}";

/// A row's decision in a few words, for "only in" differences.
fn summary(decision: &Decision) -> String {
    match decision {
        Decision::Sent { output } => format!("sent: {}", first_line(output)),
        other => Recorded::of(other).to_string(),
    }
}

/// What two versions decided, compared: the decision, else the output (field by field).
fn compare(before: &Decision, after: &Decision, kind: OutputKind) -> Vec<(String, String, String)> {
    match (before, after) {
        (Decision::Sent { output: b }, Decision::Sent { output: a }) => differences(kind, b, a),
        _ => {
            let (b, a) = (Recorded::of(before), Recorded::of(after));
            if b == a { Vec::new() } else { vec![("decision".into(), b.to_string(), a.to_string())] }
        }
    }
}

/// A decision without its output: what a destination's record can say.
#[derive(Debug, PartialEq)]
enum Recorded {
    Sent,
    Filtered(Option<String>),
    Quarantined(String),
    Other(String),
}

impl Recorded {
    fn of(decision: &Decision) -> Self {
        match decision {
            Decision::Sent { .. } => Recorded::Sent,
            Decision::Filtered { reason } => Recorded::Filtered(reason.clone()),
            // A deliberate reason, or the error's first line (its stack trace names lines that move
            // with every edit).
            Decision::Quarantined { reason } => Recorded::Quarantined(first_line(reason).to_string()),
            Decision::NotComparable { why } => Recorded::Other(why.clone()),
        }
    }
}

impl std::fmt::Display for Recorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Recorded::Sent => write!(f, "sent"),
            Recorded::Filtered(None) => write!(f, "filtered"),
            Recorded::Filtered(Some(reason)) => write!(f, "filtered ({reason})"),
            Recorded::Quarantined(reason) => write!(f, "quarantined ({reason})"),
            Recorded::Other(why) => write!(f, "{why}"),
        }
    }
}

/// What the code decided, as far as a destination's record tells: the receiver's refusals and the
/// rate limit's drops were the code's "send"; a person's edit or a removed destination says nothing
/// about the code (`None`).
fn recorded(r: &ductus_store::Recorded) -> Option<Recorded> {
    let error = r.last_error.as_deref();
    match r.status {
        Status::Sent => Some(Recorded::Sent),
        Status::Filtered => match error {
            Some("dropped: over rate limit") => Some(Recorded::Sent),
            Some(e) if e.starts_with("replaced by edited copy") || e.ends_with("was removed from interface.yaml") => {
                None
            }
            reason => Some(Recorded::Filtered(reason.map(str::to_string))),
        },
        Status::Quarantined => match error {
            Some(e) if receiver_refused(e) => Some(Recorded::Sent),
            e => Some(Recorded::Quarantined(first_line(e.unwrap_or("")).to_string())),
        },
        Status::Pending | Status::Duplicate => None,
    }
}

/// A quarantine the receiver caused (an AR, an HTTP 4xx, retries given up), as the worker words it:
/// the code had decided to send.
fn receiver_refused(error: &str) -> bool {
    error.starts_with("rejected (AR)")
        || error.starts_with("gave up after ")
        || error.strip_prefix("HTTP ").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// Where two outputs differ: `(where, before, after)`, at most `PER_MESSAGE`. HL7 by field
/// (`PID-5`; a repeated segment's occurrences grouped), JSON by path (array positions as `*`),
/// anything else by line.
pub(crate) fn differences(kind: OutputKind, before: &str, after: &str) -> Vec<(String, String, String)> {
    if before == after {
        return Vec::new();
    }
    let mut found = match kind {
        OutputKind::Hl7 => hl7_differences(before, after),
        OutputKind::Json => match (serde_json::from_str::<Value>(before), serde_json::from_str::<Value>(after)) {
            (Ok(b), Ok(a)) => {
                let mut found = Vec::new();
                json_differences("", &b, &a, &mut found);
                found
            }
            _ => line_differences(before, after),
        },
        OutputKind::Text => line_differences(before, after),
    };
    if found.is_empty() {
        // Equal by fields, not byte for byte.
        found.push((
            "line endings or spacing".into(),
            before.escape_debug().to_string(),
            after.escape_debug().to_string(),
        ));
    }
    found.truncate(PER_MESSAGE);
    found
}

/// An HL7 message's segments: `((name, occurrence), fields)`, field 0 the name.
fn hl7_segments(text: &str) -> Vec<((String, usize), Vec<&str>)> {
    let separator = text.strip_prefix("MSH").and_then(|rest| rest.chars().next()).unwrap_or('|');
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    text.split(['\r', '\n'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let fields: Vec<&str> = s.split(separator).collect();
            let name = fields[0].to_string();
            let n = seen.entry(name.clone()).or_default();
            *n += 1;
            ((name, *n), fields)
        })
        .collect()
}

fn hl7_differences(before: &str, after: &str) -> Vec<(String, String, String)> {
    let (b, a) = (hl7_segments(before), hl7_segments(after));
    fn find<'a>(segments: &[((String, usize), Vec<&'a str>)], key: &(String, usize)) -> Option<Vec<&'a str>> {
        segments.iter().find(|(k, _)| k == key).map(|(_, f)| f.clone())
    }
    let mut keys: Vec<(String, usize)> = a.iter().map(|(k, _)| k.clone()).collect();
    keys.extend(b.iter().map(|(k, _)| k.clone()).filter(|k| !a.iter().any(|(ak, _)| ak == k)));
    let mut found = Vec::new();
    for key in keys {
        let name = &key.0;
        match (find(&b, &key), find(&a, &key)) {
            (Some(bf), None) => found.push((name.clone(), bf.join("|"), "(no segment)".into())),
            (None, Some(af)) => found.push((name.clone(), "(no segment)".into(), af.join("|"))),
            (Some(bf), Some(af)) => {
                for i in 1..bf.len().max(af.len()) {
                    let (x, y) = (bf.get(i).copied().unwrap_or(""), af.get(i).copied().unwrap_or(""));
                    if x != y {
                        // MSH-1 is the separator itself: its first field after the name is MSH-2.
                        let number = if name == "MSH" { i + 1 } else { i };
                        found.push((format!("{name}-{number}"), x.to_string(), y.to_string()));
                    }
                }
            }
            (None, None) => {}
        }
    }
    found
}

fn json_differences(path: &str, before: &Value, after: &Value, found: &mut Vec<(String, String, String)>) {
    if found.len() >= PER_MESSAGE || before == after {
        return;
    }
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = format!("{path}/{key}");
                match (b.get(key), a.get(key)) {
                    (Some(x), Some(y)) => json_differences(&child, x, y, found),
                    (x, y) => found.push((child, json_shown(x), json_shown(y))),
                }
            }
        }
        (Value::Array(b), Value::Array(a)) => {
            for i in 0..b.len().max(a.len()) {
                let child = format!("{path}/*");
                match (b.get(i), a.get(i)) {
                    (Some(x), Some(y)) => json_differences(&child, x, y, found),
                    (x, y) => found.push((child, json_shown(x), json_shown(y))),
                }
            }
        }
        _ => {
            found.push((if path.is_empty() { "/".into() } else { path.into() }, before.to_string(), after.to_string()))
        }
    }
}

fn json_shown(value: Option<&Value>) -> String {
    value.map_or_else(|| "(none)".into(), Value::to_string)
}

fn line_differences(before: &str, after: &str) -> Vec<(String, String, String)> {
    let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    (0..b.len().max(a.len()))
        .filter_map(|i| {
            let (x, y) = (b.get(i).copied(), a.get(i).copied());
            (x != y).then(|| {
                (format!("line {}", i + 1), x.unwrap_or("(none)").to_string(), y.unwrap_or("(none)").to_string())
            })
        })
        .take(10)
        .collect()
}

fn shown(value: &str, hide: bool) -> String {
    if hide {
        return String::new();
    }
    let mut shown: String = value.chars().take(SHOWN).collect();
    if value.chars().count() > SHOWN {
        shown.push('…');
    }
    shown
}

fn counts(d: &Destination) -> String {
    let mut parts = vec![format!("{} compared", number(d.compared))];
    for (n, what) in [
        (d.identical, "identical"),
        (d.different, "different"),
        (d.drift, "drift"),
        (d.not_comparable, "not comparable"),
        (d.waiting, "waiting"),
    ] {
        if n > 0 {
            parts.push(format!("{} {what}", number(n)));
        }
    }
    parts.join(", ")
}

fn round(seconds: f64) -> f64 {
    (seconds * 100.0).round() / 100.0
}

/// 1234567 → "1,234,567".
fn number(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADT: &str = "MSH|^~\\&|ADT|HOSP|LAB|HOSP|20261029120000||ADT^A01|1|P|2.5\rPID|1||123||DOE^JOHN\rOBX|1|ST|A||x\rOBX|2|ST|B||y\r";

    #[test]
    fn hl7_outputs_differ_by_field_with_repeated_segments_grouped() {
        let after = ADT
            .replace("DOE^JOHN", "DOE^JON")
            .replace("OBX|2|ST|B||y", "OBX|2|ST|B||z")
            .replace("ADT^A01|1", "ADT^A01|2");
        let found = differences(OutputKind::Hl7, ADT, &after);
        assert_eq!(
            found,
            [
                ("MSH-10".to_string(), "1".to_string(), "2".to_string()),
                ("PID-5".into(), "DOE^JOHN".into(), "DOE^JON".into()),
                ("OBX-5".into(), "y".into(), "z".into()),
            ]
        );
        let with_segment = format!("{ADT}ZPI|1|extra\r");
        assert_eq!(
            differences(OutputKind::Hl7, ADT, &with_segment),
            [("ZPI".to_string(), "(no segment)".to_string(), "ZPI|1|extra".to_string())]
        );
        assert_eq!(differences(OutputKind::Hl7, ADT, &with_segment.replace("ZPI|1|extra\r", ""))[..], []);
        let crlf = ADT.replace('\r', "\r\n");
        assert_eq!(differences(OutputKind::Hl7, ADT, &crlf)[0].0, "line endings or spacing");
    }

    #[test]
    fn json_outputs_differ_by_path_and_text_by_line() {
        let found = differences(
            OutputKind::Json,
            r#"{"patient":{"name":"Doe","mrn":"1"},"orders":[{"code":"A"},{"code":"B"}]}"#,
            r#"{"patient":{"name":"Doe","mrn":"2"},"orders":[{"code":"A"},{"code":"C"}],"new":true}"#,
        );
        assert_eq!(
            found,
            [
                ("/new".to_string(), "(none)".to_string(), "true".to_string()),
                ("/orders/*/code".into(), "\"B\"".into(), "\"C\"".into()),
                ("/patient/mrn".into(), "\"1\"".into(), "\"2\"".into()),
            ]
        );
        assert_eq!(
            differences(OutputKind::Text, "a\nb\nc", "a\nB\nc"),
            [("line 2".to_string(), "b".to_string(), "B".to_string())]
        );
        assert_eq!(differences(OutputKind::Json, "not json", "not json!")[0].0, "line 1", "unparsable JSON by line");
    }

    fn record(status: Status, error: Option<&str>) -> ductus_store::Recorded {
        ductus_store::Recorded {
            id: 1,
            destination: "d".into(),
            status,
            last_error: error.map(Into::into),
            output: None,
        }
    }

    #[test]
    fn the_record_says_what_the_code_decided_when_it_can() {
        let sent = Some(Recorded::Sent);
        assert_eq!(recorded(&record(Status::Sent, None)), sent);
        assert_eq!(recorded(&record(Status::Quarantined, Some("rejected (AR): unknown patient"))), sent);
        assert_eq!(recorded(&record(Status::Quarantined, Some("HTTP 400 Bad Request: no"))), sent);
        assert_eq!(recorded(&record(Status::Quarantined, Some("gave up after 5 attempts: refused"))), sent);
        assert_eq!(recorded(&record(Status::Filtered, Some("dropped: over rate limit"))), sent);
        assert_eq!(
            recorded(&record(
                Status::Quarantined,
                Some("transform failed: Error: no PID\n    at transform (file:///x.ts:3:9)")
            )),
            Some(Recorded::Quarantined("transform failed: Error: no PID".into()))
        );
        assert_eq!(
            recorded(&record(Status::Quarantined, Some("HTTP is down"))),
            Some(Recorded::Quarantined("HTTP is down".into()))
        );
        assert_eq!(
            recorded(&record(Status::Filtered, Some("killed: test patient"))),
            Some(Recorded::Filtered(Some("killed: test patient".into())))
        );
        assert_eq!(recorded(&record(Status::Filtered, Some("replaced by edited copy 7"))), None);
        assert_eq!(
            recorded(&record(Status::Filtered, Some("destination `lab` was removed from interface.yaml"))),
            None
        );
    }

    fn row(id: i64, decision: Decision, recorded: Option<ductus_store::Recorded>) -> Row {
        Row { interface: "adt".into(), id, destination: "lab".into(), decision, looked_up: false, recorded }
    }

    fn sent(output: &str) -> Decision {
        Decision::Sent { output: output.into() }
    }

    #[test]
    fn differences_from_the_other_version_fail_and_shared_ones_are_drift() {
        let from = Utc::now();
        let mut report = Report::new("prod", from, from, None, false);
        report.event(&Event::Interface {
            interface: "adt".into(),
            skipped: None,
            outputs: BTreeMap::from([("lab".to_string(), OutputKind::Hl7)]),
        });
        let changed = ADT.replace("DOE^JOHN", "DOE^JON");
        let kept = |output: &str| {
            let mut r = record(Status::Sent, None);
            r.output = Some(output.as_bytes().to_vec());
            Some(r)
        };
        // Same as the other version, the record and the kept output.
        report.add(&row(1, sent(ADT), kept(ADT)), OtherSide::Row(&row(1, sent(ADT), None)));
        // Different from the other version: fails.
        report.add(&row(2, sent(&changed), kept(ADT)), OtherSide::Row(&row(2, sent(ADT), None)));
        // The other version does the same: drift (the data changed), doesn't fail.
        report.add(&row(3, sent(&changed), kept(ADT)), OtherSide::Row(&row(3, sent(&changed), None)));
        // Not sent yet when it was compared with the other version: compared all the same.
        report.add(&row(4, sent(ADT), Some(record(Status::Pending, None))), OtherSide::Row(&row(4, sent(ADT), None)));
        let writes = Decision::NotComparable { why: "writes to database `billing`: not made in a replay".into() };
        report.add(&row(5, writes, Some(record(Status::Sent, None))), OtherSide::Row(&row(5, sent(ADT), None)));
        report.add(
            &row(6, Decision::Filtered { reason: None }, Some(record(Status::Sent, None))),
            OtherSide::Row(&row(6, sent(ADT), None)),
        );

        let document = report.document();
        let lab = &document["interfaces"][0]["destinations"][0];
        assert_eq!(
            (
                lab["compared"].as_u64(),
                lab["identical"].as_u64(),
                lab["different"].as_u64(),
                lab["drift"].as_u64(),
                lab["not_comparable"].as_u64()
            ),
            (Some(6), Some(2), Some(2), Some(1), Some(1)),
            "{document:#}"
        );
        assert!(!report.ok());
        let groups = lab["differences"].as_array().unwrap();
        let other_pid = groups.iter().find(|g| g["against"] == "other" && g["what"] == "PID-5").unwrap();
        assert_eq!((other_pid["count"].as_u64(), other_pid["drift"].as_bool()), (Some(1), Some(false)));
        assert_eq!(other_pid["examples"][0], json!({ "id": 2, "before": "DOE^JOHN", "after": "DOE^JON" }));
        let stored_pid = groups.iter().find(|g| g["against"] == "stored" && g["what"] == "PID-5").unwrap();
        assert_eq!(stored_pid["count"].as_u64(), Some(2));
        let decision = groups.iter().find(|g| g["against"] == "other" && g["what"] == "decision").unwrap();
        assert_eq!(decision["examples"][0]["after"], "filtered");
        assert!(document["checksum"].as_str().unwrap().starts_with("sha256:"));
        let text = report.text();
        assert!(text.contains("✗ lab: 6 compared, 2 identical, 2 different, 1 drift, 1 not comparable"), "{text}");
        assert!(
            text.contains(
                "vs the other version: PID-5 differs in 1 message(s) (e.g. message 2: \"DOE^JOHN\" → \"DOE^JON\")"
            ),
            "{text}"
        );
        let html = report.html();
        assert!(html.contains("<td>DOE^JOHN</td>") || html.contains("<code>DOE^JOHN</code>"), "{html}");
    }

    #[test]
    fn without_another_version_the_record_and_kept_outputs_decide() {
        let from = Utc::now();
        let mut report = Report::new("prod", from, from, None, true);
        report.add(&row(1, sent(ADT), Some(record(Status::Sent, None))), OtherSide::NotRun);
        report.add(&row(2, sent(ADT), Some(record(Status::Pending, None))), OtherSide::NotRun);
        report.add(&row(3, sent(ADT), None), OtherSide::NotRun);
        report.add(
            &row(4, Decision::Filtered { reason: Some("killed: test".into()) }, Some(record(Status::Sent, None))),
            OtherSide::NotRun,
        );
        let document = report.document();
        let lab = &document["interfaces"][0]["destinations"][0];
        assert_eq!(
            (
                lab["identical"].as_u64(),
                lab["waiting"].as_u64(),
                lab["not_comparable"].as_u64(),
                lab["different"].as_u64()
            ),
            (Some(1), Some(1), Some(1), Some(1)),
            "{document:#}"
        );
        assert_eq!(
            lab["differences"][0]["examples"][0],
            json!({ "id": 4, "before": "", "after": "" }),
            "values hidden"
        );
        assert!(!report.ok());
        assert_eq!(number(1_234_567), "1,234,567");
        assert_eq!(number(12), "12");
    }
}
