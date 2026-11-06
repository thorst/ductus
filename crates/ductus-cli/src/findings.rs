//! What `ductus validate` finds, as text for people (the lines it always printed) or as one JSON
//! document for editors and CI (`--json`): each finding with its severity, a stable code, the file
//! and the place in it, and a fix where there's one to give. `CODES` is the list the reference page
//! (`docs/reference/validate.md`) is made from.

use ductus_core::yaml::{Located, Span, Stage};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Makes the project invalid (exit 1): the engine would refuse it, or it can't work.
    Error,
    /// Works, but probably not as meant, or less safely than it could.
    Warning,
    /// What wasn't checked, and why.
    Note,
}

/// Who found it: an editor's own TypeScript server already shows `typescript` findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Typescript,
    Ductus,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// `DUC120`, `DX3`, `TS2322`: stable, so tools can match on it.
    pub code: String,
    pub source: Source,
    /// What `validate` prints for it (without the ✗ or ⚠).
    pub message: String,
    /// The file, relative to the project, with `/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Where in it, from 1 (columns in characters); the end where known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
    /// The interface it's about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    /// What to do about it, when the message says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// How the text output writes it, when not `✗ message` / `⚠ message` / `note: message`.
    #[serde(skip)]
    text: Option<String>,
}

impl Finding {
    fn new(severity: Severity, code: &str, message: impl Into<String>) -> Self {
        debug_assert!(
            !code.starts_with("DUC") || CODES.iter().any(|c| c.code == code),
            "{code} isn't in findings::CODES"
        );
        Self {
            severity,
            code: code.into(),
            source: Source::Ductus,
            message: message.into(),
            file: None,
            line: None,
            column: None,
            end_line: None,
            end_column: None,
            interface: None,
            hint: None,
            text: None,
        }
    }

    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    pub fn note(code: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Note, code, message)
    }

    /// A finding of the TypeScript checker or of our lints in it (`DX`), printed as it always was.
    pub fn diagnostic(d: &ductus_script::Diagnostic, root: &Path) -> Self {
        // Codes 90000+ are Ductus lints; below that, TypeScript's own.
        let (code, source) = if d.code >= 90_000 {
            (format!("DX{}", d.code - 90_000), Source::Ductus)
        } else {
            (format!("TS{}", d.code), Source::Typescript)
        };
        let severity = match d.category.as_str() {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            _ => Severity::Note,
        };
        let file = d.file.as_deref().map(|f| relative(root, f));
        let at = file.as_deref().map(|f| format!("{f}:{}:{} ", d.line, d.column)).unwrap_or_default();
        let mut finding = Self::new(severity, &code, d.message.clone());
        finding.source = source;
        finding.text = Some(format!("{at}{} {code}: {}", d.category, d.message));
        if file.is_some() {
            finding.line = Some(d.line);
            finding.column = Some(d.column);
        }
        finding.file = file;
        finding
    }

    pub fn file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    pub fn interface(mut self, name: &str) -> Self {
        self.interface = Some(name.into());
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn at(mut self, line: u32, column: u32) -> Self {
        (self.line, self.column) = (Some(line), Some(column));
        self
    }

    fn span(mut self, span: Option<Span>) -> Self {
        if let Some(s) = span {
            (self.line, self.column) = (Some(s.start.line), Some(s.start.column));
            if s.end != s.start {
                (self.end_line, self.end_column) = (Some(s.end.line), Some(s.end.column));
            }
        }
        self
    }

    /// How `validate` prints it.
    fn line_of_text(&self) -> String {
        match (&self.text, self.severity) {
            (Some(text), _) => text.clone(),
            (None, Severity::Error) => format!("✗ {}", self.message),
            (None, Severity::Warning) => format!("⚠ {}", self.message),
            (None, Severity::Note) => format!("note: {}", self.message),
        }
    }
}

/// The path of an analysed file (absolute, `/`) relative to the project.
pub fn relative(root: &Path, file: &str) -> String {
    let root = ductus_runtime::project::key(root);
    file.strip_prefix(&root).map(|r| r.trim_start_matches('/').to_string()).unwrap_or_else(|| file.to_string())
}

/// Where validate's findings go: printed as they come (text), or kept for one JSON document.
pub struct Report {
    root: PathBuf,
    json: bool,
    findings: Vec<Finding>,
    /// Files read to place findings in them, by path relative to the project.
    texts: RefCell<HashMap<String, Option<String>>>,
}

impl Report {
    pub fn new(root: &Path, json: bool) -> Self {
        Self { root: root.to_path_buf(), json, findings: Vec::new(), texts: RefCell::new(HashMap::new()) }
    }

    /// A line of text output that isn't a finding (`environment: dev`, `✓ ...`).
    pub fn say(&self, line: impl std::fmt::Display) {
        if !self.json {
            println!("{line}");
        }
    }

    pub fn ok(&self, line: impl std::fmt::Display) {
        self.say(format!("✓ {line}"));
    }

    pub fn add(&mut self, finding: Finding) {
        if !self.json {
            println!("{}", finding.line_of_text());
        }
        self.findings.push(finding);
    }

    /// The findings that make the project invalid.
    pub fn errors(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Error).count()
    }

    /// `finding` placed at `key` in `file` (relative to the project), as deep as the file has it.
    pub fn at_key(&self, finding: Finding, file: &str, key: &[impl AsRef<str>]) -> Finding {
        let key: Vec<String> = key.iter().map(|k| k.as_ref().to_string()).collect();
        let span =
            if key.is_empty() { None } else { self.text(file).and_then(|t| ductus_core::yaml::locate(&t, &key)) };
        finding.file(file).span(span)
    }

    /// A file's load error placed where it says (a key, the parser's position, a missing
    /// reference), with the code for how far loading got: `base` + 0 syntax, 1 reference,
    /// 2 setting, 3 rule, 4 anything else (unreadable, unknown).
    pub fn load_error(
        &self,
        base: u32,
        severity: Severity,
        message: String,
        error: &anyhow::Error,
        file: &str,
    ) -> Finding {
        let located = Located::of(error);
        let stage = located.map_or(4, |l| match l.stage {
            Stage::Syntax => 0,
            Stage::Reference => 1,
            Stage::Setting => 2,
            Stage::Rule => 3,
        });
        let finding = Finding::new(severity, &format!("DUC{}", base + stage), message).file(file);
        match located {
            Some(l) => {
                let span = self.text(file).and_then(|t| l.span(&t));
                finding.span(span)
            }
            None => finding,
        }
    }

    fn text(&self, file: &str) -> Option<String> {
        self.texts
            .borrow_mut()
            .entry(file.to_string())
            .or_insert_with(|| std::fs::read_to_string(self.root.join(file)).ok())
            .clone()
    }

    /// The summary line (text), or the whole document (`--json`). True when valid.
    pub fn finish(self, environment: &str) -> anyhow::Result<bool> {
        let errors = self.errors();
        if self.json {
            let warnings = self.findings.iter().filter(|f| f.severity == Severity::Warning).count();
            let document = serde_json::json!({
                "environment": environment,
                "valid": errors == 0,
                "errors": errors,
                "warnings": warnings,
                "findings": self.findings,
            });
            println!("{}", serde_json::to_string_pretty(&document)?);
        } else {
            println!("\n{}", if errors == 0 { "valid".to_string() } else { format!("{errors} problem(s)") });
        }
        Ok(errors == 0)
    }
}

/// One of `validate`'s own codes, for the reference page.
pub struct Code {
    pub code: &'static str,
    /// Its usual severity (`DUC205` is an error with `require_about: true`).
    pub severity: &'static str,
    pub title: &'static str,
    /// When it's reported and what to do.
    pub about: &'static str,
}

const fn code(code: &'static str, severity: &'static str, title: &'static str, about: &'static str) -> Code {
    Code { code, severity, title, about }
}

/// Every `DUC` code `validate` reports. A code keeps its meaning once released: a new check gets
/// a new code, and a retired one's number isn't reused.
pub const CODES: &[Code] = &[
    code(
        "DUC100",
        "error",
        "An environment file isn't YAML",
        "`environments/<env>.yaml` doesn't parse: the line and column are where the parser stopped. Usually an indentation or a missing quote.",
    ),
    code(
        "DUC102",
        "error",
        "An environment file has a setting it doesn't know",
        "`environments/<env>.yaml` has a key other than `variables` and `console`, or a value of the wrong kind (`console` is `debug`, `info`, `warn` or `off`). Put every value under `variables:`.",
    ),
    code(
        "DUC104",
        "error",
        "The environment can't be loaded",
        "`--env` names an environment with no file in `environments/`, or its secrets (`secrets/<env>.json`) can't be read. Check the name, or create the file.",
    ),
    code(
        "DUC110",
        "error",
        "resources.yaml isn't YAML",
        "It doesn't parse: the line and column are where the parser stopped.",
    ),
    code(
        "DUC111",
        "error",
        "resources.yaml uses a value the environment doesn't have",
        "A `${NAME}` or `${secret:NAME}` has no value in this environment: the place is the first one missing; the message lists them all. Add it to `environments/<env>.yaml`, or set the secret with `ductus secret set NAME --env <env>`.",
    ),
    code(
        "DUC112",
        "error",
        "resources.yaml has a setting it doesn't know, or a value of the wrong kind",
        "The place is the setting (an unknown key itself). The reference's resources.yaml page lists every setting.",
    ),
    code(
        "DUC113",
        "error",
        "A resources.yaml entry breaks a rule",
        "An entry that parses but can't work as written (an SFTP server without a password or key, an API `url` that isn't http(s), a certificate without its key): the place is the entry.",
    ),
    code("DUC114", "error", "resources.yaml can't be read", "The file exists but can't be read (permissions)."),
    code(
        "DUC115",
        "warning",
        "A certificate's path has no drive letter (Windows)",
        "Windows reads it from the project's drive. Write the whole path, or one relative to the project folder.",
    ),
    code(
        "DUC116",
        "warning",
        "A directory is reached with plain ldap://",
        "Passwords cross the network readable. Use ldaps:// or `starttls: true` outside a test directory.",
    ),
    code(
        "DUC117",
        "warning",
        "A Postgres database's certificate isn't checked",
        "Whoever answers at its address is believed. Set `tls: verify-full` (with `ca:` for a private CA).",
    ),
    code(
        "DUC118",
        "warning",
        "A SQL Server's certificate isn't checked",
        "`trust_server_certificate: true`: whoever answers at its address is believed. Remove it, with `ca:` for a private CA.",
    ),
    code(
        "DUC119",
        "warning",
        "A Windows login to SQL Server from Linux uses NTLM",
        "Never Kerberos from Linux: Microsoft is retiring NTLM and some domains block it. A SQL login, or the engine on Windows, avoids it.",
    ),
    code(
        "DUC120",
        "error",
        "An interface.yaml isn't YAML",
        "It doesn't parse: the line and column are where the parser stopped.",
    ),
    code(
        "DUC121",
        "error",
        "An interface.yaml uses a value the environment doesn't have",
        "A `${NAME}` or `${secret:NAME}` has no value in this environment: the place is the first one missing. Add it to `environments/<env>.yaml` (or the secret).",
    ),
    code(
        "DUC122",
        "error",
        "An interface.yaml has a setting it doesn't know, or a value of the wrong kind",
        "The place is the setting (an unknown key itself, a port of 70000).",
    ),
    code(
        "DUC123",
        "error",
        "An interface.yaml breaks a rule",
        "Settings that parse but don't go together, or a value out of range (`workers` without `isolation: dedicated`, a destination name with capitals, the singular `destination:` removed in 0.7.4: the message says what to write instead): the place is the setting the message names.",
    ),
    code("DUC124", "error", "An interface.yaml can't be read", "The file exists but can't be read (permissions)."),
    code(
        "DUC125",
        "error",
        "An interface's code doesn't load",
        "Its code (a destination's own `script`, or the interface's) doesn't load the way the engine loads it, or doesn't export the function a destination runs (`transform`, or the one `transform:` names): the place is the destination's `transform` or `script`, else the interface's `script`.",
    ),
    code(
        "DUC130",
        "error",
        "ductus.yaml isn't YAML",
        "It doesn't parse: the line and column are where the parser stopped.",
    ),
    code(
        "DUC131",
        "error",
        "ductus.yaml's alerts use a value the environment doesn't have",
        "A `${NAME}` or `${secret:NAME}` under `alerts:` has no value in this environment.",
    ),
    code(
        "DUC132",
        "error",
        "ductus.yaml has a setting it doesn't know, or a value of the wrong kind",
        "The place is the setting.",
    ),
    code(
        "DUC133",
        "error",
        "ductus.yaml breaks a rule",
        "A setting out of range (`audit.keep` under a year, `isolates.heap_mb` under 64) or alerts that can't work as written.",
    ),
    code("DUC134", "error", "ductus.yaml can't be read", "The file exists but can't be read (permissions)."),
    code(
        "DUC150",
        "warning",
        "The Windows service's account can't log in where the project needs it",
        "Shown on Windows only: the service's account and the SQL Server logins that use it (`ductus service account`).",
    ),
    code(
        "DUC200",
        "error",
        "Interfaces want the same port",
        "Two MLLP listeners on one port, MLLP and HTTP on one port, two interfaces handling the same HTTP method and path, or HTTP interfaces on one port with a different `tls` or `allow`. The place is the second interface's source.",
    ),
    code(
        "DUC201",
        "error",
        "A route between interfaces can't work",
        "A route (`destinations: { b: { interface: b } }`) to an interface that doesn't exist or doesn't have `source: internal`, in a format the target doesn't read, or in a loop.",
    ),
    code(
        "DUC202",
        "error",
        "An interface names a resource resources.yaml doesn't have",
        "An SFTP server, database or API named in interface.yaml: add it to resources.yaml, or fix the name (the message lists those defined).",
    ),
    code(
        "DUC203",
        "error",
        "Code names a resource resources.yaml doesn't have",
        "`db(\"name\")` or `api(\"name\")` in TypeScript: the place is the call.",
    ),
    code(
        "DUC204",
        "warning",
        "An internal interface gets nothing",
        "It has `source: internal` but no interface routes to it.",
    ),
    code(
        "DUC205",
        "warning",
        "An interface doesn't say which systems it connects",
        "Name the sending and receiving application or vendor (`about.from`, `about.to`, or on the destination), so `ductus impact` can find it. An error with `require_about: true` in ductus.yaml.",
    ),
    code(
        "DUC206",
        "warning",
        "Code writes to the log for every message in this environment",
        "A `console` call (not in a test) whose lines the environment's `console:` lets through (`info` when it isn't set): each message the code runs for adds a log line, kept with the engine's own. Fine while looking into something; then remove it, or set `console: warn` (or `off`) in `environments/<env>.yaml`. Not reported for an environment that says `console: debug` (development and test, where logging everything is the point).",
    ),
    code(
        "DUC210",
        "error",
        "A TLS setting names the wrong certificate",
        "A listener's `tls.certificate`, or a connection's `ca` or `certificate`, names an entry `certificates:` doesn't have, or one of the wrong kind (a CA where a pair is needed, or the other way round).",
    ),
    code(
        "DUC211",
        "error",
        "A certificate doesn't load",
        "A `certificates:` entry's files can't be read, don't match their key, or have expired: the engine would refuse to start.",
    ),
    code(
        "DUC212",
        "warning",
        "A certificate expires within 30 days",
        "Renew it: the engine alerts too, 30, 14 and 7 days before.",
    ),
    code(
        "DUC213",
        "warning",
        "The REST API is on the network without TLS",
        "Passwords and tokens cross it readable, and browsers won't keep the session cookie: add `tls: { certificate: <name> }` to `api`.",
    ),
    code(
        "DUC220",
        "warning",
        "An `allow` list names a host that doesn't resolve",
        "Nothing is let in for it until it resolves (the engine looks again every 5 minutes). DNS seen from here may not be the engine's.",
    ),
    code(
        "DUC221",
        "warning",
        "A sender `about` names isn't allowed",
        "`about.from.hosts` names a host the listener's `allow` would refuse.",
    ),
    code(
        "DUC222",
        "warning",
        "Can't tell whether a sender is allowed",
        "`about.from.hosts` names a host that doesn't resolve from here.",
    ),
    code(
        "DUC223",
        "warning",
        "api.allow leaves out this machine",
        "`ductus status`, `drain` and the web UI on this machine would be refused: add 127.0.0.1 if you use them here.",
    ),
    code(
        "DUC230",
        "error",
        "Directory sign-in can't be set up",
        "`auth.ldap` names a directory resources.yaml doesn't have, or one that can't be used as written.",
    ),
    code(
        "DUC231",
        "warning",
        "Directory sign-in can't tell people apart",
        "Settings under which a renamed person and a new one given a departed person's name look the same (`attributes.id: none`, a `user_filter` that doesn't match one attribute).",
    ),
    code(
        "DUC240",
        "warning",
        "alert.to is set, but there are no alerts",
        "ductus.yaml has no `alerts:`, so nothing is sent: add them, or remove `alert.to`.",
    ),
    code(
        "DUC241",
        "warning",
        "Alerts that may not work as meant",
        "What `alerts:` would do that's probably a mistake (a rule nothing reaches, a channel no rule uses).",
    ),
    code(
        "DUC242",
        "error",
        "alert.to names a channel alerts don't have",
        "Add the channel under `alerts.channels`, or fix the name.",
    ),
    code(
        "DUC243",
        "warning",
        "An owner alerts can't reach",
        "Alerts go to `owner`, but the owner isn't an email address or a team in `owners:` (without a database, accounts can't be checked).",
    ),
    code(
        "DUC244",
        "warning",
        "An owner doesn't match one account",
        "Checked against the database's accounts: renamed, disabled, several accounts, or nobody. The message gives the fix (`ductus owners reassign`).",
    ),
    code("DUC250", "error", "docs-schema.yaml doesn't load", "It doesn't parse, or declares a field it can't use."),
    code(
        "DUC251",
        "error",
        "A documentation field's value is wrong or missing",
        "A `custom:` value isn't declared in docs-schema.yaml, isn't one of its values, or a required one is missing.",
    ),
    code(
        "DUC300",
        "error",
        "The database can't be checked",
        "`--database-url` (or DUCTUS_DATABASE_URL) was given, but it can't be connected to or read.",
    ),
    code(
        "DUC301",
        "warning",
        "The database connection isn't verified",
        "Whoever answers at its address is believed: add `sslmode=verify-full` to the URL (`sslrootcert=/path/ca.pem` for a private CA).",
    ),
    code(
        "DUC302",
        "warning",
        "Couldn't read how the database connection is protected",
        "The check of the connection's TLS failed; the rest went on.",
    ),
    code(
        "DUC303",
        "error",
        "Lanes would strand waiting messages",
        "`ordering.partitions` is lower than the lanes messages wait in: the engine would refuse to start. Keep it as it is until they're sent (`ductus drain --wait`).",
    ),
    code("DUC304", "error", "The lanes couldn't be checked", "Reading the waiting messages' lanes failed."),
    code(
        "DUC305",
        "warning",
        "More lanes with messages waiting",
        "A patient's (key's) new messages may be sent before its older ones until those are.",
    ),
    code("DUC306", "warning", "A retention change is still running", "Its messages are moving in the background."),
    code(
        "DUC307",
        "warning",
        "A shorter retention waits for a confirmation",
        "It deletes messages, so it waits for `ductus retention confirm <interface>`.",
    ),
    code("DUC308", "warning", "Retention changes at the next start", "Its messages move in the background then."),
    code(
        "DUC309",
        "warning",
        "Retention couldn't be read",
        "Reading the database's retention state failed; the rest went on.",
    ),
    code("DUC310", "warning", "The accounts couldn't be read", "Owners weren't matched to accounts."),
    code(
        "DUC390",
        "note",
        "Not checked against a database",
        "Without `--database-url` (or DUCTUS_DATABASE_URL), lanes that would strand waiting messages and retention changes aren't checked.",
    ),
    code(
        "DUC391",
        "note",
        "This version hasn't run on the database yet",
        "The lanes interfaces last ran with aren't recorded, so raised lanes can't be told (lowered ones are checked).",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code validate reports is listed (with what to do), and every listed one is reported
    /// somewhere: the reference page can't drift from the checks.
    #[test]
    fn every_code_is_listed_and_every_listed_code_is_used() {
        let checks = include_str!("project.rs");
        let used: std::collections::BTreeSet<String> = checks
            .match_indices("\"DUC")
            .map(|(at, _)| checks[at + 1..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect())
            .filter(|c: &String| c.len() == 6)
            .collect();
        // The load errors' codes are made from a base (`load_error`).
        let all = [0, 1, 2, 3, 4];
        let made: Vec<String> = [(100, &[0, 2, 4][..]), (110, &all), (120, &all), (130, &all)]
            .iter()
            .flat_map(|(base, stages)| stages.iter().map(move |s| format!("DUC{}", base + s)))
            .collect();
        let listed: Vec<&str> = CODES.iter().map(|c| c.code).collect();
        for code in used.iter().map(String::as_str).chain(made.iter().map(String::as_str)) {
            assert!(listed.contains(&code), "{code} is reported but not in CODES");
        }
        for code in &listed {
            assert!(used.contains(*code) || made.iter().any(|m| m == code), "{code} is in CODES but never reported");
        }
        let mut sorted = listed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, listed, "CODES in order, each once");
    }
}
