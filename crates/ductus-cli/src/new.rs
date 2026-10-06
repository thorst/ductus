//! `ductus new interface`: a working interface to start from (config, code, a test and a
//! sample message) that passes `ductus test` and `ductus validate` as generated.

use anyhow::{Context, Result, bail};
use ductus_core::settings::{self, PortRange, Settings};
use ductus_core::{Environment, InterfaceConfig, Resources};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceKind {
    /// HL7 over MLLP.
    Mllp,
    /// A REST endpoint answering with the transform's result.
    Http,
    /// Files in a local folder.
    File,
    /// Files on an SFTP server from resources.yaml.
    Sftp,
    /// A timer: the transform runs every hour and produces the message.
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FormatKind {
    Hl7,
    Json,
    Xml,
    Text,
    Csv,
    Fixed,
}

pub struct NewInterface {
    pub root: std::path::PathBuf,
    pub name: String,
    pub source: SourceKind,
    pub format: Option<FormatKind>,
    pub port: Option<u16>,
    pub server: Option<String>,
}

pub fn interface(o: NewInterface) -> Result<()> {
    let NewInterface { root, name, source, format, port, server } = o;
    let valid_name = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid_name {
        bail!("interface names use lower-case letters, digits, - and _ (e.g. `adt-to-lab`), not `{name}`");
    }
    let dir = root.join("interfaces").join(&name);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }

    use FormatKind as F;
    use SourceKind as S;
    let format = format.unwrap_or(match source {
        S::Mllp | S::File | S::Sftp => F::Hl7,
        S::Http | S::Schedule => F::Json,
    });
    match (source, format) {
        (S::Mllp, f) if f != F::Hl7 => bail!("an MLLP source carries HL7; leave out --format"),
        (S::Http, f) if f != F::Json => {
            bail!("HTTP interfaces are generated with JSON; change `format` in interface.yaml afterwards")
        }
        (S::Schedule, f) if f != F::Json && f != F::Csv => {
            bail!("schedule interfaces are generated with --format json or csv (what the transform produces)")
        }
        _ => {}
    }

    let port = match source {
        S::Mllp | S::Http => {
            let ranges = Settings::load(&root)?.ports;
            let (range, kind) = if source == S::Mllp { (ranges.mllp, "mllp") } else { (ranges.http, "http") };
            Some(pick_port(&root, port, range, kind)?)
        }
        _ => {
            if port.is_some() {
                bail!("--port is for MLLP and HTTP sources");
            }
            None
        }
    };
    let server = match source {
        S::Sftp => Some(sftp_server(&root, server)?),
        _ if server.is_some() => bail!("--server is for SFTP sources"),
        _ => None,
    };

    let files = render(&name, source, format, port, server.as_deref());
    std::fs::create_dir_all(dir.join("fixtures")).with_context(|| format!("creating {}", dir.display()))?;
    for (file, content) in &files {
        std::fs::write(dir.join(file), content).with_context(|| format!("writing {file}"))?;
    }

    let summary = match (source, port) {
        (S::Mllp, Some(p)) => format!("MLLP on port {p}"),
        (S::Http, Some(p)) => format!("HTTP on port {p}: GET /{name}/{{id}}"),
        (S::Sftp, _) => format!("files from SFTP server `{}`", server.unwrap_or_default()),
        (S::File, _) => format!("files from in/{name}"),
        _ => "every hour".to_string(),
    };
    println!("created interfaces/{name}/ ({summary})");
    for (file, _) in &files {
        println!("  {file}");
    }
    println!("\nnext: ductus test --interface {name}");
    Ok(())
}

/// The requested port if it's free, else the first free one in the range. "Free" means no
/// interface listens on it in any environment (ports can differ per environment).
fn pick_port(root: &Path, wanted: Option<u16>, range: PortRange, kind: &str) -> Result<u16> {
    let mut used: BTreeMap<u16, String> = BTreeMap::new();
    let mut unknown = Vec::new();
    for env_name in settings::environments(root)? {
        let env = Environment::load(root, &env_name)?;
        let Ok(entries) = std::fs::read_dir(root.join("interfaces")) else { break };
        for dir in entries.flatten().map(|e| e.path()).filter(|p| p.join("interface.yaml").is_file()) {
            match InterfaceConfig::load_in(&dir, &env) {
                Ok(c) => {
                    for l in ductus_core::ports::listeners([&c]) {
                        used.entry(l.port).or_insert_with(|| format!("`{}` ({env_name})", c.name));
                    }
                }
                Err(_) => {
                    unknown.push(format!("{} ({env_name})", dir.file_name().unwrap_or_default().to_string_lossy()))
                }
            }
        }
    }
    if !unknown.is_empty() {
        eprintln!("note: these don't load, so their ports weren't checked: {}", unknown.join(", "));
    }
    if let Some(p) = wanted {
        if let Some(by) = used.get(&p) {
            bail!("port {p} is already used by {by}");
        }
        return Ok(p);
    }
    (range.first..=range.last)
        .find(|p| !used.contains_key(p))
        .with_context(|| format!("no free port in {range} (widen ports.{kind} in {} or pass --port)", settings::FILE))
}

/// The SFTP server to read from: the one given, or the only one in resources.yaml.
fn sftp_server(root: &Path, wanted: Option<String>) -> Result<String> {
    let env_name = settings::environments(root)?.into_iter().next().unwrap_or_else(|| "dev".into());
    let resources = Resources::load(root, &Environment::load(root, &env_name)?)?;
    let known: Vec<&String> = resources.sftp.keys().collect();
    match wanted {
        Some(s) if resources.sftp.contains_key(&s) => Ok(s),
        Some(s) => bail!("no sftp server named `{s}` in resources.yaml (defined: {})", list(&known)),
        None if known.len() == 1 => Ok(known[0].clone()),
        None if known.is_empty() => {
            bail!("an SFTP source needs a server: add one under `sftp:` in resources.yaml first")
        }
        None => bail!("which SFTP server? pass --server (defined: {})", list(&known)),
    }
}

fn list(names: &[&String]) -> String {
    names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}

const OUTCOMES: &str = "
 * Return what to send on, null to drop it, kill(\"reason\") to drop it with a reason,
 * or throw quarantine(\"reason\") to hold it for review.";

/// File name → content for the new interface folder.
fn render(
    name: &str,
    source: SourceKind,
    format: FormatKind,
    port: Option<u16>,
    server: Option<&str>,
) -> Vec<(String, String)> {
    use FormatKind as F;
    use SourceKind as S;
    let ext = match format {
        F::Hl7 => "hl7",
        F::Json => "json",
        F::Xml => "xml",
        F::Text | F::Fixed => "txt",
        F::Csv => "csv",
    };
    // Files holding many messages are split: HL7 per MSH, CSV and fixed-length per record.
    let split = if matches!(format, F::Hl7 | F::Csv | F::Fixed) { ", split: records" } else { "" };
    let port = port.unwrap_or_default();
    let source_yaml = match source {
        S::Mllp => format!("source: {{ mllp: {{ port: {port} }} }}"),
        S::Http => {
            format!("source: {{ http: {{ port: {port}, path: \"/{name}/{{id}}\", method: GET, mode: respond }} }}")
        }
        S::File => format!("source: {{ file: {{ dir: in/{name}, pattern: \"*.{ext}\"{split} }} }}"),
        S::Sftp => format!(
            "source: {{ sftp: {{ server: {}, dir: outbound/{name}, pattern: \"*.{ext}\"{split} }} }}",
            server.unwrap_or_default()
        ),
        S::Schedule => "source: { schedule: { every: 1h } }   # or cron: \"0 2 * * *\"".to_string(),
    };
    let format_yaml = match format {
        F::Hl7 => String::new(),
        F::Fixed => "format:\n  fixed:\n    fields:\n      - { name: mrn, width: 10 }\n      - { name: last_name, width: 20 }\n      - { name: first_name, width: 10 }\n".into(),
        F::Json => "format: json\n".into(),
        F::Xml => "format: xml\n".into(),
        F::Text => "format: text\n".into(),
        F::Csv => "format: csv\n".into(),
    };
    let destination = if source == S::Http {
        "# The transform's result is the HTTP response.\n".to_string()
    } else {
        format!(
            "destination: {{ file: {{ dir: out/{name} }} }}\n\
             # or: destination: {{ mllp: {{ host: receiver.example.org, port: 5000 }} }}\n\
             # or: destination: {{ http: {{ url: https://api.example.org/messages }} }}\n"
        )
    };
    let yaml = format!(
        "# Generated by `ductus new interface`.\nname: {name}\n{source_yaml}\n{format_yaml}script: transform.ts\n{destination}"
    );

    let (transform, test, fixture) = match (source, format) {
        (S::Http, _) => http_code(name),
        (S::Schedule, f) => schedule_code(name, f),
        (_, F::Hl7) => (
            format!(
                "import type {{ Message }} from \"@ductus/sdk\";\n\n\
                 /**\n * {name}: called once per message.{OUTCOMES}\n */\n\
                 export function transform(msg: Message) {{\n  \
                 // Example: set the receiving application.\n  \
                 msg.set(\"MSH-5\", \"RECEIVER\");\n  return msg;\n}}\n"
            ),
            "import { expect, input, type Message, test } from \"@ductus/sdk\";\n\
             import { transform } from \"./transform.ts\";\n\n\
             test(\"sets the receiving application and keeps the patient\", () => {\n  \
             const out = transform(input<Message>(\"sample.hl7\"));\n  \
             expect(out.get(\"MSH-5\")).toBe(\"RECEIVER\");\n  \
             expect(out.get(\"PID-3.1\")).toBe(\"123456\");\n});\n"
                .to_string(),
            Some((
                "sample.hl7",
                "MSH|^~\\&|EPIC|HOSP|LAB|HOSP|20261006120000||ADT^A01|MSG0001|P|2.5\n\
                 EVN|A01|20261006120000\n\
                 PID|1||123456^^^MRN||Smith^John||19800101|M\n\
                 PV1|1|I|4W^401^1\n"
                    .to_string(),
            )),
        ),
        (_, F::Xml) => (
            format!(
                "import type {{ XmlElement }} from \"@ductus/sdk\";\n\n\
                 /**\n * {name}: called once per XML document.{OUTCOMES}\n */\n\
                 export function transform(doc: XmlElement) {{\n  \
                 doc.set(\"Name/Last\", doc.get(\"Name/Last\").toUpperCase());\n  return doc;\n}}\n"
            ),
            "import { expect, input, test, type XmlElement } from \"@ductus/sdk\";\n\
             import { transform } from \"./transform.ts\";\n\n\
             test(\"upper-cases the last name\", () => {\n  \
             const out = transform(input<XmlElement>(\"sample.xml\"));\n  \
             expect(out.get(\"Name/Last\")).toBe(\"SMITH\");\n  \
             expect(out.get(\"MRN\")).toBe(\"123456\");\n});\n"
                .to_string(),
            Some((
                "sample.xml",
                "<Patient>\n  <MRN>123456</MRN>\n  <Name><Last>Smith</Last><First>John</First></Name>\n</Patient>\n"
                    .to_string(),
            )),
        ),
        (_, F::Text) => (
            format!(
                "/**\n * {name}: called once per text message.{OUTCOMES}\n */\n\
                 export function transform(text: string) {{\n  return text.toUpperCase();\n}}\n"
            ),
            "import { expect, input, test } from \"@ductus/sdk\";\n\
             import { transform } from \"./transform.ts\";\n\n\
             test(\"upper-cases the text\", () => {\n  \
             expect(transform(input<string>(\"sample.txt\"))).toBe(\"MRN 123456 SMITH JOHN\\n\");\n});\n"
                .to_string(),
            Some(("sample.txt", "MRN 123456 Smith John\n".to_string())),
        ),
        (_, F::Json) => record_code(name, "JSON document", "json", None),
        (_, F::Csv) => record_code(name, "CSV row", "csv", Some("mrn,last_name,first_name\n123456,Smith,John\n")),
        (_, F::Fixed) => {
            let line = format!("{:<10}{:<20}{:<10}\n", "123456", "Smith", "John"); // the layout above
            record_code(name, "fixed-length record", "txt", Some(&line))
        }
    };
    let mut files = vec![
        ("interface.yaml".to_string(), yaml),
        ("transform.ts".into(), transform),
        ("transform.test.ts".into(), test),
    ];
    if let Some((file, content)) = fixture {
        files.push((format!("fixtures/{file}"), content));
    }
    files
}

type Code = (String, String, Option<(&'static str, String)>);

/// JSON documents, CSV rows and fixed-length records: an object with named fields.
fn record_code(name: &str, what: &str, ext: &'static str, sample: Option<&str>) -> Code {
    let (fixture, content) = match sample {
        Some(text) => (if ext == "csv" { "sample.csv" } else { "sample.txt" }, text.to_string()),
        None => {
            ("sample.json", "{ \"mrn\": \"123456\", \"last_name\": \"Smith\", \"first_name\": \"John\" }\n".to_string())
        }
    };
    let transform = format!(
        "/** One incoming {what}. Name its fields here for type checking and editor help. */\n\
         export interface Patient {{\n  mrn: string;\n  last_name: string;\n  first_name: string;\n}}\n\n\
         /**\n * {name}: called once per {what}.{OUTCOMES}\n */\n\
         export function transform(patient: Patient) {{\n  \
         return {{ ...patient, last_name: patient.last_name.toUpperCase() }};\n}}\n"
    );
    let test = format!(
        "import {{ expect, input, test }} from \"@ductus/sdk\";\n\
         import {{ type Patient, transform }} from \"./transform.ts\";\n\n\
         test(\"upper-cases the last name\", () => {{\n  \
         const out = transform(input<Patient>(\"{fixture}\"));\n  \
         expect(out.last_name).toBe(\"SMITH\");\n  \
         expect(out.mrn).toBe(\"123456\");\n}});\n"
    );
    (transform, test, Some((fixture, content)))
}

fn http_code(name: &str) -> Code {
    let transform = format!(
        "import type {{ HttpRequest }} from \"@ductus/sdk\";\n\n\
         /**\n * {name}: answers GET /{name}/{{id}}. The returned object is sent as JSON; return\n \
         * response({{ status, body }}) for another status, or null for 404.\n \
         * Look things up with db(\"name\").one(...) or fetch(...) (make transform async).\n */\n\
         export function transform(req: HttpRequest) {{\n  \
         return {{ id: req.params.id, message: \"hello from {name}\" }};\n}}\n"
    );
    let test = format!(
        "import {{ expect, type HttpRequest, test }} from \"@ductus/sdk\";\n\
         import {{ transform }} from \"./transform.ts\";\n\n\
         function get(id: string): HttpRequest {{\n  \
         return {{ method: \"GET\", path: `/{name}/${{id}}`, params: {{ id }}, query: {{}}, headers: {{}}, text: \"\", body: undefined }};\n}}\n\n\
         test(\"answers with the id\", () => {{\n  \
         expect(transform(get(\"42\"))).toEqual({{ id: \"42\", message: \"hello from {name}\" }});\n}});\n"
    );
    (transform, test, None)
}

fn schedule_code(name: &str, format: FormatKind) -> Code {
    let (body, check) = if format == FormatKind::Csv {
        (
            "  // e.g. return await db(\"patient_db\").query(\"SELECT ... WHERE updated_at > $1\", [run.previous]);\n  \
             return [{ due: run.due, previous: run.previous ?? \"\" }];",
            "toEqual([{ due: \"2026-10-06T02:00:00\", previous: \"\" }])",
        )
    } else {
        (
            "  // e.g. const rows = await db(\"patient_db\").query(\"SELECT ... WHERE updated_at > $1\", [run.previous]);\n  \
             return { due: run.due, previous: run.previous };",
            "toEqual({ due: \"2026-10-06T02:00:00\", previous: null })",
        )
    };
    let what = if format == FormatKind::Csv { "rows (written as CSV)" } else { "a JSON document" };
    let transform = format!(
        "import type {{ ScheduledRun }} from \"@ductus/sdk\";\n\n\
         /**\n * {name}: runs every hour and returns {what} to send, or null when there's nothing.\n \
         * `run.previous` is when the last run was due: pick up what changed since then.\n */\n\
         export function transform(run: ScheduledRun) {{\n{body}\n}}\n"
    );
    let test = format!(
        "import {{ expect, test }} from \"@ductus/sdk\";\n\
         import {{ transform }} from \"./transform.ts\";\n\n\
         test(\"reports the run\", () => {{\n  \
         expect(transform({{ due: \"2026-10-06T02:00:00\", previous: null }})).{check};\n}});\n"
    );
    (transform, test, None)
}
