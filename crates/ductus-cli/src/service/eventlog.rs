//! The Windows Event Log: what the service writes there, beside its log files.
//!
//! Only `run --service` and `ductus service` write. The files keep everything; the Event Log gets
//! what Windows tools watch for (Event Viewer, SCOM, a Splunk forwarder): starts and stops, a
//! start that failed, an unclean last run, interfaces the runaway guard stopped, critical alerts,
//! and ERROR lines, each kind and target at most once a minute so a flapping receiver can't
//! flood it. The text Event Viewer shows is ours (`messages.rs`, built into ductus.exe).

use super::messages as id;
use ductus_runtime::events;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::EventLog::{
    DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE, REPORT_EVENT_TYPE,
    RegisterEventSourceW, ReportEventW,
};

/// The event source the installer registers.
const SOURCE: &str = "Ductus";
/// ERROR lines of one kind and target: at most one in this long.
const ONCE_PER: Duration = Duration::from_secs(60);
/// The Event Log takes up to ~31,000 characters a string; a long one is cut well before that.
const LONGEST: usize = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Information,
    Warning,
    Error,
}

impl Kind {
    fn raw(self) -> REPORT_EVENT_TYPE {
        match self {
            Kind::Information => EVENTLOG_INFORMATION_TYPE,
            Kind::Warning => EVENTLOG_WARNING_TYPE,
            Kind::Error => EVENTLOG_ERROR_TYPE,
        }
    }
}

/// A handle to the "Ductus" source.
pub struct EventLog(HANDLE);

// The handle is only passed to ReportEventW, which may be called from any thread.
unsafe impl Send for EventLog {}
unsafe impl Sync for EventLog {}

impl EventLog {
    /// None when Windows won't give one (the event is then only in the files).
    pub fn open() -> Option<EventLog> {
        let name = wide(SOURCE);
        let handle = unsafe { RegisterEventSourceW(std::ptr::null(), name.as_ptr()) };
        (!handle.is_null()).then_some(EventLog(handle))
    }

    pub fn write(&self, kind: Kind, event: u32, strings: &[&str]) {
        let wides: Vec<Vec<u16>> = strings.iter().map(|s| wide(&cut(s))).collect();
        let pointers: Vec<*const u16> = wides.iter().map(|w| w.as_ptr()).collect();
        unsafe {
            ReportEventW(
                self.0,
                kind.raw(),
                0,
                event,
                std::ptr::null_mut(),
                pointers.len() as u16,
                0,
                pointers.as_ptr(),
                std::ptr::null(),
            );
        }
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        unsafe { DeregisterEventSource(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn cut(s: &str) -> String {
    match s.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{}… (cut; the log file has all of it)", &s[..at]),
        None => s.to_string(),
    }
}

/// Write one event, if the Event Log can be opened (for `ductus service` and the service's own
/// messages outside the engine's log lines).
pub fn write(kind: Kind, event: u32, strings: &[&str]) {
    if let Some(log) = EventLog::open() {
        log.write(kind, event, strings);
    }
}

/// The service's own lines (`run.rs`), named so they map to their events here.
pub const COULD_NOT_START: &str = "the engine could not start";
pub const ENDED_ON_ERROR: &str = "the engine ended on an error";
pub const REFUSED: &str = "refusing to run";

/// What a log line becomes in the Event Log, if anything.
#[derive(Debug, PartialEq, Eq)]
pub enum Mapped {
    Event(Kind, u32),
    /// An ERROR line with no event of its own: 1100, once a minute per this key.
    Error(String),
    Nothing,
}

/// Which event a line is: by its message (the runtime names them in `events`).
pub fn map(level: &Level, line: &Line) -> Mapped {
    let m = line.message.as_str();
    let field = |name: &str| line.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    match m {
        _ if m == events::STARTED && *level == Level::INFO => Mapped::Event(Kind::Information, id::STARTED),
        _ if m == events::STOPPED && *level == Level::INFO => Mapped::Event(Kind::Information, id::STOPPED),
        _ if m.starts_with(events::STOP_TIMED_OUT) || m == events::STOP_CUT_SHORT => {
            Mapped::Event(Kind::Warning, id::STOPPED_FORCED)
        }
        _ if m == events::PREVIOUS_RUN_UNCLEAN => Mapped::Event(Kind::Warning, id::PREVIOUS_RUN_UNCLEAN),
        _ if m == events::INTERFACE_STOPPED => Mapped::Event(Kind::Error, id::INTERFACE_STOPPED),
        _ if m == events::ALERT => {
            let raised = matches!(field("kind"), Some("fired" | "escalated"));
            if raised && field("severity") == Some("critical") {
                Mapped::Event(Kind::Error, id::CRITICAL_ALERT)
            } else {
                Mapped::Nothing
            }
        }
        COULD_NOT_START | REFUSED => Mapped::Event(Kind::Error, id::START_FAILED),
        ENDED_ON_ERROR => Mapped::Event(Kind::Error, id::ENDED_ON_ERROR),
        _ if *level == Level::ERROR => {
            // The kind is the message; the target, what it's about.
            let target = ["interface", "destination", "alert", "channel"]
                .iter()
                .filter_map(|f| field(f).map(|v| format!(" {f}={v}")))
                .collect::<String>();
            Mapped::Error(format!("{m}{target}"))
        }
        _ => Mapped::Nothing,
    }
}

/// A log line: its message and fields, as the log file has them.
#[derive(Debug, Default)]
pub struct Line {
    pub message: String,
    pub fields: Vec<(String, String)>,
}

impl Line {
    pub fn text(&self) -> String {
        let mut out = self.message.clone();
        out.push_str(&self.fields_text());
        out
    }

    /// Only the fields, for events whose title already says what the message says.
    pub fn fields_text(&self) -> String {
        let mut out = String::new();
        for (name, value) in &self.fields {
            let _ = write!(out, "\n{name}: {value}");
        }
        out.trim_start().to_string()
    }
}

impl Visit for Line {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }
}

impl Line {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

/// ERROR lines already written in the last minute, per kind and target.
#[derive(Default)]
struct Window {
    last: Option<Instant>,
    more: u32,
    more_since: Option<chrono::DateTime<chrono::Local>>,
}

/// The tracing layer: copies the lines that matter to the Event Log.
pub struct EventLogLayer {
    log: EventLog,
    /// Said with the start: the project and the database.
    context: String,
    windows: Mutex<HashMap<String, Window>>,
}

impl EventLogLayer {
    pub fn new(context: String) -> Option<EventLogLayer> {
        Some(EventLogLayer { log: EventLog::open()?, context, windows: Mutex::default() })
    }

    /// Whether an ERROR line of this kind may be written now, and how many like it were held back.
    fn admit(&self, key: String, now: Instant) -> Option<String> {
        let mut windows = self.windows.lock().unwrap();
        let w = windows.entry(key).or_default();
        if w.last.is_some_and(|l| now.duration_since(l) < ONCE_PER) {
            w.more += 1;
            w.more_since.get_or_insert_with(chrono::Local::now);
            return None;
        }
        w.last = Some(now);
        let note = match (w.more, w.more_since.take()) {
            (0, _) | (_, None) => String::new(),
            (n, Some(since)) => format!("\n\nand {n} more like it since {}", since.format("%H:%M:%S")),
        };
        w.more = 0;
        Some(note)
    }
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::INFO {
            return;
        }
        let mut line = Line::default();
        event.record(&mut line);
        match map(&level, &line) {
            Mapped::Event(kind, id::STARTED) => {
                let details = format!("{}\n\n{}", self.context, line.fields_text());
                self.log.write(kind, id::STARTED, &[env!("CARGO_PKG_VERSION"), &details]);
            }
            // The title says it: the details are the fields.
            Mapped::Event(
                kind,
                event @ (id::STOPPED | id::PREVIOUS_RUN_UNCLEAN | id::INTERFACE_STOPPED | id::CRITICAL_ALERT),
            ) => self.log.write(kind, event, &[&line.fields_text()]),
            Mapped::Event(kind, event) => self.log.write(kind, event, &[&line.text()]),
            Mapped::Error(key) => {
                if let Some(note) = self.admit(key, Instant::now()) {
                    self.log.write(Kind::Error, id::ERROR, &[&format!("{}{note}", line.text())]);
                }
            }
            Mapped::Nothing => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &str, fields: &[(&str, &str)]) -> Line {
        Line { message: message.into(), fields: fields.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect() }
    }

    #[test]
    fn lines_map_to_their_events() {
        let event = |level: Level, l: Line| map(&level, &l);
        assert_eq!(
            event(Level::INFO, line(events::STARTED, &[("interfaces", "3")])),
            Mapped::Event(Kind::Information, id::STARTED)
        );
        assert_eq!(event(Level::INFO, line(events::STOPPED, &[])), Mapped::Event(Kind::Information, id::STOPPED));
        let timed_out = format!("{} 30s; stopping anyway", events::STOP_TIMED_OUT);
        assert_eq!(event(Level::WARN, line(&timed_out, &[])), Mapped::Event(Kind::Warning, id::STOPPED_FORCED));
        assert_eq!(
            event(Level::ERROR, line(events::INTERFACE_STOPPED, &[("interface", "adt")])),
            Mapped::Event(Kind::Error, id::INTERFACE_STOPPED)
        );
        let critical = line(events::ALERT, &[("kind", "fired"), ("severity", "critical")]);
        assert_eq!(event(Level::INFO, critical), Mapped::Event(Kind::Error, id::CRITICAL_ALERT));
        let warning = line(events::ALERT, &[("kind", "fired"), ("severity", "warning")]);
        assert_eq!(event(Level::INFO, warning), Mapped::Nothing);
        assert_eq!(event(Level::ERROR, line(COULD_NOT_START, &[])), Mapped::Event(Kind::Error, id::START_FAILED));
        // Other ERROR lines: one key per kind and target; anything else isn't written.
        assert_eq!(
            event(Level::ERROR, line("saving status failed; retrying", &[("interface", "adt"), ("id", "7")])),
            Mapped::Error("saving status failed; retrying interface=adt".into())
        );
        assert_eq!(event(Level::WARN, line("something", &[])), Mapped::Nothing);
        // A plain INFO line that happens to be called "stopped" by someone else at WARN isn't a stop.
        assert_eq!(event(Level::WARN, line(events::STOPPED, &[])), Mapped::Nothing);
    }

    #[test]
    fn errors_of_one_kind_are_written_once_a_minute_with_how_many_were_held() {
        let layer =
            EventLogLayer { log: EventLog(std::ptr::null_mut()), context: String::new(), windows: Mutex::default() };
        let t = Instant::now();
        assert_eq!(layer.admit("x".into(), t).as_deref(), Some(""));
        assert_eq!(layer.admit("x".into(), t + Duration::from_secs(1)), None);
        assert_eq!(layer.admit("x".into(), t + Duration::from_secs(2)), None);
        assert_eq!(layer.admit("y".into(), t + Duration::from_secs(2)).as_deref(), Some(""));
        let later = layer.admit("x".into(), t + Duration::from_secs(61)).unwrap();
        assert!(later.contains("and 2 more like it since"), "{later}");
        std::mem::forget(layer); // its handle isn't a real one
    }

    #[test]
    fn long_text_is_cut() {
        let long = "x".repeat(LONGEST + 10);
        assert!(cut(&long).ends_with("(cut; the log file has all of it)"));
        assert_eq!(cut("short"), "short");
    }
}
