//! The `ductus` command run as a user would: on a project folder, checking output and exit code.

use std::path::Path;
use std::process::Command;

/// Write files into a fresh project folder.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

/// Run `ductus <args>` in `dir`; returns (succeeded, stdout + stderr).
fn ductus(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ductus")).args(args).current_dir(dir).output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// `ductus <args>` with no database configured at all.
fn without_a_database(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(args)
        .current_dir(dir)
        .env_remove("DUCTUS_DATABASE_URL")
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn there_is_no_default_database() {
    let dir = tempfile::tempdir().unwrap();
    for args in [&["run"][..], &["status"], &["token", "list"], &["doctor"]] {
        let (ok, out) = without_a_database(dir.path(), args);
        assert!(!ok, "{args:?} must not guess a database: {out}");
        assert!(out.contains("no database: set DUCTUS_DATABASE_URL") && out.contains("ductus dev"), "{args:?}: {out}");
    }
    for args in [&["--help"][..], &["run", "--help"], &["status", "--help"], &["dev", "--help"]] {
        let (_, out) = without_a_database(dir.path(), args);
        assert!(!out.contains("todd") && !out.contains("ductus_dev"), "{args:?} shows a personal database: {out}");
    }
}

#[test]
fn an_env_file_supplies_settings_and_the_environment_wins() {
    let parent = tempfile::tempdir().unwrap();
    let (ok, out) = ductus(parent.path(), &["init", "p"]);
    assert!(ok, "{out}");
    let project = parent.path().join("p");
    std::fs::write(
        parent.path().join("ductus.env"),
        "# the service's settings
DUCTUS_ENV=\"prod\"
",
    )
    .unwrap();
    let file = parent.path().join("ductus.env");
    let ports = |env: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ductus"));
        command.args(["ports", "--env-file"]).arg(&file).current_dir(&project).env_remove("DUCTUS_ENV");
        if let Some(env) = env {
            command.env("DUCTUS_ENV", env);
        }
        String::from_utf8_lossy(&command.output().unwrap().stdout).into_owned()
    };
    assert!(ports(None).contains("environment: prod"), "the file's setting is used");
    assert!(ports(Some("dev")).contains("environment: dev"), "a variable that is already set wins");
    // A file that isn't there is an error, not a silent skip.
    let (ok, out) = ductus(&project, &["ports", "--env-file", "missing.env"]);
    assert!(!ok && out.contains("reading the env file"), "{out}");
}

#[test]
fn a_new_project_passes_test_and_validate_as_generated() {
    let parent = tempfile::tempdir().unwrap();
    let (ok, out) = ductus(parent.path(), &["init", "hospital"]);
    assert!(ok, "{out}");
    assert!(out.contains("created interfaces/adt-in/ (MLLP on port 6661)"), "{out}");
    assert!(out.contains("git init"), "says to make it a repository: {out}");
    let dir = parent.path().join("hospital");
    let (ok, out) = ductus(&dir, &["test"]);
    assert!(ok && out.contains("passed"), "{out}");
    let (ok, out) = ductus(&dir, &["validate"]);
    assert!(ok, "every environment (dev and prod): {out}");
    // A second interface takes the next port, from ductus.yaml's range.
    let (ok, out) = ductus(&dir, &["new", "interface", "orders"]);
    assert!(ok && out.contains("port 6662"), "{out}");
    let (ok, out) = ductus(&dir, &["init"]);
    assert!(!ok && out.contains("isn't empty"), "{out}");
}

#[test]
fn generated_interfaces_pass_test_and_validate_as_generated() {
    let dir = project(&[
        // An existing interface whose port differs per environment: 7001 is taken in prod only.
        ("environments/dev.yaml", "variables:\n  OLD_PORT: 7005\n"),
        ("environments/prod.yaml", "variables:\n  OLD_PORT: 7001\n"),
        ("ductus.yaml", "ports:\n  mllp: 7000-7002\n"),
        (
            "interfaces/old/interface.yaml",
            "name: old\nsource: { mllp: { port: ${OLD_PORT} } }\ndestination: { file: { dir: out } }\n",
        ),
        ("resources.yaml", "sftp:\n  lab: { host: sftp.lab, user: u, password: x, host_key: any }\n"),
    ]);
    let new = |args: &[&str]| ductus(dir.path(), &[&["new", "interface"], args].concat());

    let (ok, out) = new(&["adt-in"]);
    assert!(ok, "{out}");
    assert!(out.contains("created interfaces/adt-in/ (MLLP on port 7000)"), "{out}");
    let (ok, out) = new(&["adt-in-2"]);
    assert!(ok && out.contains("port 7002"), "skips 7001 (used in prod): {out}");
    let (ok, out) = new(&["adt-in-3"]);
    assert!(!ok && out.contains("no free port in 7000-7002 (widen ports.mllp in ductus.yaml or pass --port)"), "{out}");
    let (ok, out) = new(&["adt-in-3", "--port", "7001"]);
    assert!(!ok && out.contains("port 7001 is already used by `old` (prod)"), "{out}");
    assert!(new(&["adt-in-3", "--port", "7100"]).0);

    for (name, args) in [
        ("api", &["--source", "http"][..]),
        ("file-hl7", &["--source", "file"]),
        ("file-json", &["--source", "file", "--format", "json"]),
        ("file-xml", &["--source", "file", "--format", "xml"]),
        ("file-text", &["--source", "file", "--format", "text"]),
        ("file-csv", &["--source", "file", "--format", "csv"]),
        ("file-fixed", &["--source", "file", "--format", "fixed"]),
        ("lab-results", &["--source", "sftp", "--format", "csv"]),
        ("nightly", &["--source", "schedule"]),
        ("nightly-csv", &["--source", "schedule", "--format", "csv"]),
    ] {
        let (ok, out) = new(&[&[name], args].concat());
        assert!(ok, "{name}: {out}");
    }
    assert!(std::fs::read_to_string(dir.path().join("interfaces/api/interface.yaml")).unwrap().contains("port: 8080"));

    let (ok, out) = ductus(dir.path(), &["test"]);
    assert!(ok, "{out}");
    assert!(out.contains("13 passed, 0 failed"), "{out}");
    for env in ["dev", "prod"] {
        let (ok, out) = ductus(dir.path(), &["validate", "--env", env]);
        assert!(ok, "{env}: {out}");
        // The generated `about:` is there to fill in: until then, warnings.
        assert!(out.contains("⚠ adt-in: about.from: add the sending system's `application` or `vendor`"), "{out}");
        assert!(out.contains("⚠ api: about.from: add the calling system's"), "{out}");
        assert!(!out.contains("⚠ api: about.to"), "respond mode has no destination: {out}");
        assert!(!out.contains("⚠ nightly: about.from"), "a schedule has no sender: {out}");
    }
    let settings = std::fs::read_to_string(dir.path().join("ductus.yaml")).unwrap();
    std::fs::write(dir.path().join("ductus.yaml"), format!("{settings}require_about: true\n")).unwrap();
    let (ok, out) = ductus(dir.path(), &["validate"]);
    assert!(!ok && out.contains("✗ adt-in: about.from"), "{out}");
    std::fs::write(dir.path().join("ductus.yaml"), settings).unwrap();

    let (ok, out) = new(&["adt-in"]);
    assert!(!ok && out.contains("already exists"), "{out}");
    let (ok, out) = new(&["Bad Name"]);
    assert!(!ok && out.contains("lower-case letters"), "{out}");
    let (ok, out) = new(&["x", "--format", "json"]);
    assert!(!ok && out.contains("an MLLP source carries HL7"), "{out}");
    let (ok, out) = new(&["y", "--source", "sftp", "--server", "emr"]);
    assert!(!ok && out.contains("no sftp server named `emr`"), "{out}");
}

/// A small hospital: Epic sends ADT over a VPN subnet; results come from Quest over SFTP and go to
/// Epic and to billing (an API); a lookup library uses the EMPI and Epic's reporting database.
fn hospital() -> tempfile::TempDir {
    project(&[
        (
            "resources.yaml",
            r#"
databases:
  clarity: { postgres: { host: 10.20.30.9, database: clarity, user: u, application: Clarity, vendor: Epic Systems } }
sftp:
  quest: { host: sftp.quest.example.com, user: u, password: p, host_key: any, vendor: Quest }
apis:
  empi: { url: "https://empi.hospital.local/api", application: EMPI, vendor: Rhapsody }
  billing: { url: "https://billing.example.org/v2", headers: { Authorization: Bearer x }, vendor: Experian }
"#,
        ),
        (
            "interfaces/adt-in/interface.yaml",
            r#"name: adt-in
about:
  owner: Integration team
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
  to: { application: Sunquest Lab, vendor: Sunquest }
source: { mllp: { port: 6661 } }
script: transform.ts
destination: { mllp: { host: 10.50.0.7, port: 6662 } }
"#,
        ),
        (
            "interfaces/adt-in/transform.ts",
            r#"import type { Message } from "@ductus/sdk";
import { enrich } from "../../lib/lookups.ts";
export const transform = (msg: Message) => enrich(msg);
"#,
        ),
        (
            "lib/lookups.ts",
            r#"import { api, db, type Message } from "@ductus/sdk";
export async function enrich(msg: Message) {
  const id = (await api("empi").get(`/patients/${msg.PID[3][1]}`)).json().id;
  const row = await db("clarity").one("SELECT 1 AS x");
  msg.add("ZID", [id, String(row?.x)]);
  return msg;
}
"#,
        ),
        (
            "interfaces/results/interface.yaml",
            r#"name: results
about: { owner: Lab team, to: { application: Epic, vendor: Epic Systems } }
source: { sftp: { server: quest, dir: /out } }
script: transform.ts
destinations:
  emr: { mllp: { host: epic-int.local, port: 7000 } }
  billing: { http: { api: billing, path: /charges }, transform: toBilling, output: json, vendor: Experian }
"#,
        ),
        (
            "interfaces/results/transform.ts",
            r#"import type { Message } from "@ductus/sdk";
export const transform = (msg: Message) => msg;
export const toBilling = (msg: Message) => ({ mrn: msg.PID[3][1] });
"#,
        ),
    ])
}

#[test]
fn validate_checks_the_resources_code_names() {
    let dir = hospital();
    let (ok, out) = ductus(dir.path(), &["validate"]);
    assert!(ok, "{out}");
    assert!(out.contains("✓ resources: 1 database(s), 1 SFTP server(s), 2 API(s)"), "{out}");
    assert!(!out.contains('⚠'), "every side names its system: {out}");

    // A typo in a shared library: reported once, where it is, with the names that exist.
    let lib = dir.path().join("lib/lookups.ts");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap().replace("db(\"clarity\")", "db(\"clartiy\")")).unwrap();
    let (ok, out) = ductus(dir.path(), &["validate"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains(
            "✗ lib/lookups.ts:4:21 db(\"clartiy\"): no database named `clartiy` in resources.yaml (defined: clarity)"
        ),
        "{out}"
    );
    assert_eq!(out.matches("clartiy\"):").count(), 1, "{out}");

    // An HTTP destination's API must exist too (config, so the engine refuses to start as well).
    let config = dir.path().join("interfaces/results/interface.yaml");
    std::fs::write(&config, std::fs::read_to_string(&config).unwrap().replace("api: billing", "api: biling")).unwrap();
    let (_, out) = ductus(dir.path(), &["validate"]);
    assert!(
        out.contains("✗ interface `results`: destination `billing`: no api named `biling` in resources.yaml (defined: billing, empi)"),
        "{out}"
    );
}

#[test]
fn impact_finds_interfaces_by_vendor_application_and_host() {
    let dir = hospital();
    let (ok, out) = ductus(dir.path(), &["impact", "--vendor", "epic systems"]);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        r#"environment: dev
vendor "epic systems": 2 interface(s) affected, of 2

adt-in  (owner: Integration team)
  source            about.from: Epic (Epic Systems)
                    → listens on :6661 MLLP
  database clarity  database `clarity` in resources.yaml: Clarity (Epic Systems)
                    → 10.20.30.9:5432 Postgres

results  (owner: Lab team)
  destination emr   about.to: Epic (Epic Systems)
                    → epic-int.local:7000 MLLP
"#
    );

    // The EMPI is only used from a library adt-in's code imports.
    let (_, out) = ductus(dir.path(), &["impact", "--application", "EMPI"]);
    assert!(out.contains("1 interface(s) affected") && out.contains("api empi"), "{out}");
    assert!(out.contains("→ empi.hospital.local:443 HTTPS"), "{out}");

    // A VPN subnet: the senders configured on it and a database inside it.
    let (_, out) = ductus(dir.path(), &["impact", "--host", "10.20.30.0/24"]);
    assert!(out.contains("sender host (about.from.hosts): 10.20.30.0/24"), "{out}");
    assert!(out.contains("database `clarity` in resources.yaml at: 10.20.30.9"), "{out}");
    assert!(out.contains("add --resolve"), "{out}");

    let (ok, out) = ductus(dir.path(), &["impact", "--vendor", "Cerner"]);
    assert!(ok && out.contains("0 interface(s) affected"), "{out}");
    assert!(out.contains("known vendors: Epic Systems, Experian, Quest, Rhapsody, Sunquest"), "{out}");

    let (ok, out) = ductus(dir.path(), &["impact"]);
    assert!(!ok && out.contains("--vendor"), "one of the three is required: {out}");
}

/// `ductus ports | head -1`: when the reader goes away, a report ends quietly (as other Unix
/// tools do) instead of panicking with "failed printing to stdout".
#[cfg(unix)]
#[test]
fn reports_end_quietly_when_the_reader_stops() {
    use std::os::unix::process::ExitStatusExt;
    let dir = hospital();
    // The reader is gone before the child starts (closing it after spawning raced: a quick
    // child could write everything into the pipe's buffer first and exit normally).
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .arg("ports")
        .current_dir(dir.path())
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.signal(), Some(13), "ended by SIGPIPE: {:?}", out.status);
}

#[test]
fn ports_lists_listeners_and_connections_per_environment_and_flags_conflicts() {
    let dir = project(&[
        ("environments/dev.yaml", "variables:\n  ADT_PORT: 6661\n  EPIC: epic-test.local\n"),
        ("environments/prod.yaml", "variables:\n  ADT_PORT: 7661\n  EPIC: epic.local\n"),
        (
            "interfaces/adt/interface.yaml",
            "name: adt\nsource: { mllp: { port: ${ADT_PORT} } }\ndestination: { mllp: { host: ${EPIC}, port: 5000 } }\n",
        ),
        (
            "interfaces/api/interface.yaml",
            "name: api\nsource: { http: { port: 8080, path: '/patients/{mrn}', method: GET } }\ndestination: { file: { dir: out } }\n",
        ),
        ("resources.yaml", "databases:\n  pdb: { postgres: { host: db1, database: p, user: u } }\n"),
    ]);

    let (ok, out) = ductus(dir.path(), &["ports", "--env", "prod"]);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        "environment: prod

Listening
  PORT  PROTOCOL  HOST       INTERFACE          ROUTE
  7661  MLLP      0.0.0.0    adt
  7700  HTTP      127.0.0.1  (engine REST API)  /api/v1
  8080  HTTP      0.0.0.0    api                GET /patients/{mrn} (queue)

Connects to
  HOST        PORT  PROTOCOL  USED BY
  db1         5432  Postgres  resources.yaml: pdb
  epic.local  5000  MLLP      adt

✓ no port conflicts
"
    );

    // Dev's ADT port collides with another MLLP listener.
    std::fs::write(
        dir.path().join("interfaces/api/interface.yaml"),
        "name: api\nsource: { mllp: { port: 6661 } }\ndestination: { file: { dir: out } }\n",
    )
    .unwrap();
    let (ok, out) = ductus(dir.path(), &["ports"]);
    assert!(!ok, "{out}");
    assert!(out.contains("✗ port 6661: MLLP listeners for both `adt` and `api`"), "{out}");

    // The REST API's port is checked too.
    std::fs::write(dir.path().join("ductus.yaml"), "api: { listen: '0.0.0.0:6661' }\n").unwrap();
    let (ok, out) = ductus(dir.path(), &["ports"]);
    assert!(!ok && out.contains("✗ port 6661: the REST API (`api.listen` in ductus.yaml) and `adt`"), "{out}");
}

#[test]
fn owners_lists_and_reassigns_in_place() {
    let adt = "name: adt\nsource: { mllp: { port: 6661 } }\nabout:\n  owner: Ann Smith   # on call: weekdays\ndestinations:\n  lab: { mllp: { host: lab, port: 1 } }\n  billing: { file: { dir: out }, owner: Billing team }\n";
    let dir = project(&[
        ("interfaces/adt/interface.yaml", adt),
        (
            "interfaces/orders/interface.yaml",
            "name: orders\nsource: { mllp: { port: 6662 } }\nabout: { owner: ann smith }\ndestination: { file: { dir: out } }\n",
        ),
        (
            "interfaces/nightly/interface.yaml",
            "name: nightly\nsource: { schedule: { every: 1h } }\nscript: t.ts\ndestination: { file: { dir: out } }\n",
        ),
        ("interfaces/nightly/t.ts", "export function transform() { return {}; }\n"),
    ]);
    let (ok, out) = ductus(dir.path(), &["owners"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("Ann Smith (2)\n  adt\n  orders\n\nBilling team (1)\n  adt → billing\n\nno owner (1)\n  nightly"),
        "{out}"
    );

    // Split Ann's work: orders to Bob first, as a dry run, then for real; then the rest to Carol.
    let (ok, out) = ductus(
        dir.path(),
        &["owners", "reassign", "--from", "Ann Smith", "--to", "Bob", "--interface", "orders", "--dry-run"],
    );
    assert!(ok && out.contains("- about: { owner: ann smith }\n") && out.contains("+ about: { owner: Bob }"), "{out}");
    assert!(out.contains("dry run: would change 1 owner value(s) in 1 file(s)"), "{out}");
    assert!(ductus(dir.path(), &["owners"]).1.contains("Ann Smith (2)"), "a dry run writes nothing");
    let (ok, out) =
        ductus(dir.path(), &["owners", "reassign", "--from", "Ann Smith", "--to", "Bob", "--interface", "orders"]);
    assert!(ok && out.contains("changed 1 owner value(s)"), "{out}");
    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "ann smith", "--to", "Carol"]);
    assert!(ok && out.contains("interfaces/adt/interface.yaml\n  4  - owner: Ann Smith   # on call: weekdays\n     + owner: Carol   # on call: weekdays"), "{out}");
    let written = std::fs::read_to_string(dir.path().join("interfaces/adt/interface.yaml")).unwrap();
    assert_eq!(written, adt.replace("Ann Smith", "Carol"), "only the value changed");
    let (_, out) = ductus(dir.path(), &["owners"]);
    assert!(
        out.contains("Billing team (1)") && out.contains("Bob (1)\n  orders") && out.contains("Carol (1)\n  adt"),
        "{out}"
    );

    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "Ann Smith", "--to", "X"]);
    assert!(!ok && out.contains("no `owner: Ann Smith` found (owners: Billing team, Bob, Carol)"), "{out}");
    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "Bob", "--to", "X", "--interface", "nope"]);
    assert!(!ok && out.contains("no interface named nope"), "{out}");
}

#[test]
fn doctor_checks_what_the_project_uses_and_saves_a_report() {
    let receiver = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let up = receiver.local_addr().unwrap().port();
    let down = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let adt = format!(
        "name: adt\nsource: {{ mllp: {{ host: 127.0.0.1, port: {down} }} }}\ndestinations:\n  lab: {{ mllp: {{ host: 127.0.0.1, port: {up} }} }}\n  files: {{ file: {{ dir: out }} }}\n"
    );
    let lookup = format!(
        "name: lookup\nsource: {{ mllp: {{ host: 127.0.0.1, port: {} }} }}\nscript: t.ts\ndestination: {{ mllp: {{ host: 127.0.0.1, port: {up} }} }}\n",
        down + 1
    );
    let dir = project(&[
        ("interfaces/adt/interface.yaml", &adt),
        ("interfaces/lookup/interface.yaml", &lookup),
        (
            "interfaces/lookup/t.ts",
            "import { api } from \"@ductus/sdk\";\nexport async function transform(m: string) { await api(\"empi\").get(\"/x\"); return m; }\n",
        ),
        (
            "resources.yaml",
            &format!(
                "apis:\n  empi: {{ url: 'http://127.0.0.1:{down}/api' }}\n  spare: {{ url: 'http://127.0.0.1:{down}/spare' }}\n"
            ),
        ),
    ]);
    // The engine's database on a closed port: that check fails, so doctor exits 1.
    let db = format!("--database-url=postgres://u:p@127.0.0.1:{down}/d");
    let (ok, out) = ductus(dir.path(), &["doctor", &db, "--timeout", "2s", "--report", "r.json"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains(&format!("✓ MLLP destination  127.0.0.1:{up} MLLP\n    used by  adt → lab, lookup\n")),
        "one check for both: {out}"
    );
    assert!(out.contains("✗ api empi") && out.contains("used by  lookup (code)"), "{out}");
    assert!(out.contains("used by  nothing yet (defined in resources.yaml)"), "{out}");
    assert!(out.contains("doesn't exist yet; the engine creates it"), "{out}");
    assert!(out.contains(&format!("✓ listening port  127.0.0.1:{down} MLLP")), "{out}");
    assert!(out.contains("Ask its owner whether it's up on"), "refused explained: {out}");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("r.json")).unwrap()).unwrap();
    let checks = report["checks"].as_array().unwrap();
    assert_eq!((checks.len(), checks[0]["what"].as_str()), (7, Some("engine database")), "{report:#}");
    assert_eq!(checks[0]["outcome"], "fail");

    // Only one interface's connections (and the engine's database).
    let (_, out) = ductus(dir.path(), &["doctor", &db, "--interface", "adt"]);
    assert!(out.contains("checked 4 connection(s)") && !out.contains("empi"), "{out}");
    let (ok, out) = ductus(dir.path(), &["doctor", &db, "--interface", "nope"]);
    assert!(!ok && out.contains("no interface named nope"), "{out}");
}

/// A free local port (bound, then released).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Ends the engine if the test fails half-way.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait up to 2.5 s for `check` to hold.
async fn eventually(mut check: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..50 {
        if check().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// The engine with its REST API: tokens and roles, status and queues, the message browser, the
/// OpenAPI document, and drain mode end to end (the MLLP listener closes, what's stored is
/// delivered once the receiver is back, then the engine stops).
#[tokio::test(flavor = "multi_thread")]
async fn the_api_shows_the_engine_and_drains_it() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let (mllp, receiver, api) = (free_port(), free_port(), free_port());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("api-test-{nanos}");
    let config = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\nabout: {{ owner: Lab team }}\nmonitor: {{ behind: 2 }}\ndestinations:\n  lab: {{ mllp: {{ host: 127.0.0.1, port: {receiver}, timeout_ms: 500 }} }}\n"
    );
    let dir = project(&[(format!("interfaces/{name}/interface.yaml").as_str(), config.as_str())]);
    let run = |args: &[&str]| {
        let out =
            Command::new(env!("CARGO_BIN_EXE_ductus")).args(args).env("DUCTUS_DATABASE_URL", &db).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    };
    let token = |role: &str| {
        let (ok, out) = run(&["token", "create", &format!("{name}-{role}"), "--role", role]);
        assert!(ok, "{out}");
        out.lines().next().unwrap().to_string()
    };
    let (admin, viewer) = (token("admin"), token("viewer"));

    let mut engine = Child(
        Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["run", "--root", &dir.path().to_string_lossy(), "--threads", "1"])
            .args(["--api-listen", &format!("127.0.0.1:{api}")])
            .env("DUCTUS_DATABASE_URL", &db)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let api_url = format!("http://127.0.0.1:{api}");
    let base = format!("{api_url}/api/v1");
    let http = reqwest::Client::new();
    let get = |path: &str, token: &str| http.get(format!("{base}{path}")).bearer_auth(token).send();
    let json =
        async |path: &str, token: &str| -> serde_json::Value { get(path, token).await.unwrap().json().await.unwrap() };
    let mut up = false;
    for _ in 0..100 {
        if http.get(format!("{base}/health")).send().await.is_ok_and(|r| r.status().is_success()) {
            up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(up, "the API never came up");

    // The receiver is down: three messages wait.
    let mut sender = ductus_mllp::Client::new(format!("127.0.0.1:{mllp}"), std::time::Duration::from_secs(5));
    for n in 1..=3 {
        let msg = format!("MSH|^~\\&|EPIC|H|LAB|H|20260101||ADT^A01|C{n}|P|2.5\rPID|1||MRN{n}\r");
        let ack = sender.send(msg.as_bytes()).await.unwrap();
        assert!(String::from_utf8_lossy(&ack).contains("MSA|AA"));
    }

    // Tokens and roles.
    assert_eq!(http.get(format!("{base}/interfaces")).send().await.unwrap().status(), 401);
    assert_eq!(get("/interfaces", "dux_nope").await.unwrap().status(), 401);
    let forbidden = http.post(format!("{base}/drain")).bearer_auth(&viewer).send().await.unwrap();
    assert_eq!(forbidden.status(), 403);
    assert!(forbidden.text().await.unwrap().contains("needs operator"));

    let interfaces = json("/interfaces", &viewer).await;
    let mine = interfaces.as_array().unwrap().iter().find(|i| i["name"] == name.as_str()).unwrap();
    assert_eq!((&mine["state"], &mine["owner"]), (&"running".into(), &"Lab team".into()), "{mine}");
    assert_eq!(mine["source"], format!("mllp 127.0.0.1:{mllp}"));
    assert_eq!(mine["destinations"][0]["connector"], format!("mllp 127.0.0.1:{receiver}"));
    assert_eq!(mine["destinations"][0]["pending"], 3);
    assert_eq!(mine["since_start"]["received"], 3, "{mine}");
    // Past `behind: 2`, retrying: said in operations' words, with the last error.
    let health = &mine["destinations"][0]["health"];
    assert_eq!((&health["state"], &health["behind"]), (&"behind".into(), &"messages".into()), "{health}");
    assert!(health["summary"].as_str().unwrap().starts_with("behind 3"), "{health}");
    assert!(health["oldest_waiting_s"].is_u64(), "{health}");
    let failed = eventually(async || {
        let interfaces = json("/interfaces", &viewer).await;
        let mine = interfaces.as_array().unwrap().iter().find(|i| i["name"] == name.as_str()).unwrap().clone();
        mine["destinations"][0]["health"]["last_error"]["error"].is_string()
    });
    assert!(failed.await, "the receiver is down: the last error shows");
    assert_eq!(mine["health"]["state"], "ok", "no expectation of what it receives: {mine}");
    assert!(mine["health"]["summary"].as_str().unwrap().starts_with("ok, received"), "{mine}");

    // Prometheus metrics (a token too).
    assert_eq!(http.get(format!("{base}/metrics")).send().await.unwrap().status(), 401);
    let metrics = get("/metrics", &viewer).await.unwrap().text().await.unwrap();
    let labels = format!("interface=\"{name}\",destination=\"lab\"");
    for line in [
        format!(
            "ductus_messages_received_total{{interface=\"{name}\"}} 3
"
        ),
        format!(
            "ductus_queue_pending{{{labels}}} 3
"
        ),
        format!(
            "ductus_deliveries_total{{{labels},outcome=\"sent\"}} 0
"
        ),
        format!(
            "ductus_interface_stopped{{interface=\"{name}\"}} 0
"
        ),
        format!("ductus_destination_state{{{labels}}} 1\n"),
        format!("ductus_interface_state{{interface=\"{name}\"}} 0\n"),
        format!("ductus_queue_oldest_seconds{{{labels}}} "),
        "# TYPE ductus_delivery_latency_seconds histogram
"
        .to_string(),
    ] {
        assert!(
            metrics.contains(&line),
            "missing {line:?} in:
{metrics}"
        );
    }

    // The message browser.
    let page = json(&format!("/interfaces/{name}/messages?limit=2"), &viewer).await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 2);
    assert_eq!(page["messages"][0]["control_id"], "C3", "newest first: {page}");
    let rest = json(&format!("/interfaces/{name}/messages?before={}", page["next_before"]), &viewer).await;
    assert_eq!(rest["messages"][0]["control_id"], "C1");
    assert!(rest["next_before"].is_null(), "{rest}");
    let one = json(&format!("/messages/{}", rest["messages"][0]["id"]), &viewer).await;
    assert!(one["payload"].as_str().unwrap().contains("PID|1||MRN1"), "{one}");
    assert_eq!(
        (&one["deliveries"][0]["destination"], &one["deliveries"][0]["status"]),
        (&"lab".into(), &"pending".into())
    );
    assert_eq!(get(&format!("/interfaces/{name}/messages?status=lost"), &viewer).await.unwrap().status(), 400);
    assert_eq!(get("/interfaces/nope", &viewer).await.unwrap().status(), 404);

    let hot = json(&format!("/hotspots?interface={name}"), &viewer).await;
    assert_eq!((&hot[0]["destination"], &hot[0]["waiting"]), (&"lab".into(), &3.into()), "{hot}");
    assert_eq!(hot[0]["lanes"].as_array().map(Vec::len), hot[0]["lanes_waiting"].as_u64().map(|n| n as usize));
    assert_eq!(get("/hotspots?interface=nope", &viewer).await.unwrap().status(), 404);
    let (ok, out) = ductus(dir.path(), &["hotspots", &name, "--api", &api_url, "--token", &viewer]);
    assert!(ok && out.contains("3 waiting in"), "{out}");

    let owners = json("/owners", &viewer).await;
    assert!(owners.to_string().contains("Lab team"), "{owners}");
    // Reassigning is previewed, as a patch for the project's repository: nothing is written.
    let file = dir.path().join(format!("interfaces/{name}/interface.yaml"));
    let preview = async |body: serde_json::Value| -> (u16, serde_json::Value) {
        let r = http.post(format!("{base}/owners/reassign/preview")).bearer_auth(&viewer).json(&body).send().await;
        let r = r.unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    };
    let (status, p) = preview(serde_json::json!({ "from": "lab TEAM", "to": "Integration team" })).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(
        (&p["changed"], &p["files"][0]["path"]),
        (&1.into(), &format!("interfaces/{name}/interface.yaml").into())
    );
    assert_eq!(p["files"][0]["lines"][0]["after"], "about: { owner: Integration team }", "{p}");
    assert!(p["patch"].as_str().unwrap().contains("\n+about: { owner: Integration team }\n"), "{p}");
    assert_eq!(p["command"], "ductus owners reassign --from 'lab TEAM' --to 'Integration team'");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), config, "a preview writes nothing");
    let (_, p) = preview(serde_json::json!({ "from": "Nobody", "to": "X" })).await;
    assert_eq!((&p["changed"], &p["known"]), (&0.into(), &serde_json::json!(["Lab team"])), "{p}");
    assert_eq!(p["patch"], "");
    let (status, _) = preview(serde_json::json!({ "from": "Lab team", "to": "X", "interfaces": ["nope"] })).await;
    assert_eq!(status, 400);
    let spec: serde_json::Value = http.get(format!("{base}/openapi.json")).send().await.unwrap().json().await.unwrap();
    for path in [
        "/interfaces",
        "/interfaces/{name}/messages",
        "/messages/{id}",
        "/drain",
        "/events",
        "/doctor",
        "/tokens",
        "/metrics",
        "/hotspots",
        "/owners/reassign/preview",
    ] {
        assert!(spec["paths"][path].is_object(), "{path} missing from the OpenAPI document");
    }

    // Drain: the MLLP listener closes (senders keep their messages); nothing can be delivered.
    let (ok, out) = ductus(dir.path(), &["drain", "--api", &api_url, "--token", &admin]);
    assert!(ok && out.contains("draining: 3 pending"), "{out}");
    let refused = eventually(async || tokio::net::TcpStream::connect(("127.0.0.1", mllp)).await.is_err()).await;
    assert!(refused, "the MLLP listener should close while draining");
    let health: serde_json::Value = http.get(format!("{base}/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["drain"], "draining");

    // Resume: the listener comes back.
    let (ok, out) = ductus(dir.path(), &["resume", "--api", &api_url, "--token", &admin]);
    assert!(ok && out.contains("accepting"), "{out}");
    let back = eventually(async || tokio::net::TcpStream::connect(("127.0.0.1", mllp)).await.is_ok()).await;
    assert!(back, "the MLLP listener should open again on resume");

    // The receiver comes up; `drain --wait --stop` returns once all three are delivered, and the
    // engine exits by itself.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", receiver)).await.unwrap();
    let delivered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = delivered.clone();
    tokio::spawn(ductus_mllp::serve(listener, move |_m: bytes::Bytes| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async { bytes::Bytes::from_static(b"MSH|^~\\&|LAB|H|EPIC|H|20260101||ACK|1|P|2.5\rMSA|AA|1\r") }
    }));
    let all_there = eventually(async || delivered.load(std::sync::atomic::Ordering::SeqCst) == 3);
    assert!(all_there.await, "delivered once the receiver is up");
    // Counted, with how long each took since it was received (the first waited for the receiver).
    let counted = eventually(async || {
        let interfaces = json("/interfaces", &viewer).await;
        let mine = interfaces.as_array().unwrap().iter().find(|i| i["name"] == name.as_str()).unwrap().clone();
        mine["destinations"][0]["since_start"]["sent"] == 3
    });
    assert!(counted.await, "three sent");
    let interfaces = json("/interfaces", &viewer).await;
    let lab = &interfaces.as_array().unwrap().iter().find(|i| i["name"] == name.as_str()).unwrap()["destinations"][0];
    let counts = &lab["since_start"];
    assert_eq!((&counts["latency"]["count"], &counts["send"]["count"]), (&3.into(), &3.into()), "{lab}");
    assert!(counts["retries"].as_u64().unwrap() >= 1, "the receiver was down at first: {lab}");
    assert!(counts["latency"]["p50_ms"].as_f64().unwrap() > 0.0, "{lab}");
    let metrics = get("/metrics", &viewer).await.unwrap().text().await.unwrap();
    assert!(
        metrics.contains(&format!(
            "ductus_deliveries_total{{{labels},outcome=\"sent\"}} 3
"
        )),
        "{metrics}"
    );
    assert!(
        metrics.contains(&format!(
            "ductus_send_seconds_count{{{labels}}} 3
"
        )),
        "{metrics}"
    );

    let root = dir.path().to_path_buf();
    let (ok, out) = tokio::task::spawn_blocking(move || {
        ductus(&root, &["drain", "--wait", "--stop", "--api", &api_url, "--token", &admin])
    })
    .await
    .unwrap();
    assert!(ok && out.contains("drained"), "{out}");
    let exit = engine.0.wait().unwrap();
    assert!(exit.success(), "the engine exits cleanly once drained: {exit}");
    assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 3);

    for role in ["admin", "viewer"] {
        run(&["token", "revoke", &format!("{name}-{role}")]);
    }
}

/// Pausing a destination (act first, explain after): nothing is sent to it while the others
/// carry on, a drain doesn't wait for it, the pause holds across a restart, and resuming sends
/// the backlog in order.
#[tokio::test(flavor = "multi_thread")]
async fn a_paused_destination_waits_until_resumed() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let (mllp, receiver, api) = (free_port(), free_port(), free_port());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("pause-test-{nanos}");
    let config = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\ndestinations:\n  lab: {{ mllp: {{ host: 127.0.0.1, port: {receiver}, timeout_ms: 500 }} }}\n  archive: {{ file: {{ dir: out }} }}\n"
    );
    let dir = project(&[(format!("interfaces/{name}/interface.yaml").as_str(), config.as_str())]);
    let out_dir = dir.path().join(format!("interfaces/{name}/out"));
    let run = |args: &[&str]| {
        let out =
            Command::new(env!("CARGO_BIN_EXE_ductus")).args(args).env("DUCTUS_DATABASE_URL", &db).output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let (ok, out) = run(&["token", "create", &format!("{name}-ops"), "--role", "operator"]);
    assert!(ok, "{out}");
    let ops = out.lines().next().unwrap().to_string();
    let api_url = format!("http://127.0.0.1:{api}");
    let base = format!("{api_url}/api/v1");
    let http = reqwest::Client::new();
    let start = || async {
        let engine = Child(
            Command::new(env!("CARGO_BIN_EXE_ductus"))
                .args(["run", "--root", &dir.path().to_string_lossy(), "--threads", "1"])
                .args(["--api-listen", &format!("127.0.0.1:{api}")])
                .env("DUCTUS_DATABASE_URL", &db)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        for _ in 0..100 {
            if http.get(format!("{base}/health")).send().await.is_ok_and(|r| r.status().is_success()) {
                return engine;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("the API never came up");
    };
    let lab = async || -> serde_json::Value {
        let detail: serde_json::Value =
            http.get(format!("{base}/interfaces/{name}")).bearer_auth(&ops).send().await.unwrap().json().await.unwrap();
        detail["destinations"].as_array().unwrap().iter().find(|d| d["name"] == "lab").unwrap().clone()
    };
    let cli = |args: &[&str]| {
        let mut all = args.to_vec();
        all.extend(["--api", &api_url, "--token", &ops]);
        ductus(dir.path(), &all)
    };

    // The receiver is up, and counts what it gets, in order.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", receiver)).await.unwrap();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let got = received.clone();
    tokio::spawn(ductus_mllp::serve(listener, move |m: bytes::Bytes| {
        let text = String::from_utf8_lossy(&m).to_string();
        let control = text.split('|').nth(9).unwrap_or("").to_string();
        got.lock().unwrap().push(control);
        async { bytes::Bytes::from_static(b"MSH|^~\\&|LAB|H|EPIC|H|20260101||ACK|1|P|2.5\rMSA|AA|1\r") }
    }));
    let count = || received.lock().unwrap().len();

    let mut engine = start().await;
    // Pause first, no questions; the reason comes later.
    let (ok, out) = cli(&["pause", &name, "lab"]);
    assert!(ok && out.contains("paused by") && out.contains("no reason yet"), "{out}");
    let (ok, out) = cli(&["pause", &name]);
    assert!(!ok && out.contains("no destination `default` (it has archive, lab)"), "{out}");

    let mut sender = ductus_mllp::Client::new(format!("127.0.0.1:{mllp}"), std::time::Duration::from_secs(5));
    for n in 1..=3 {
        let msg = format!("MSH|^~\\&|EPIC|H|LAB|H|20260101||ADT^A01|C{n}|P|2.5\rPID|1||MRN{n}\r");
        assert!(String::from_utf8_lossy(&sender.send(msg.as_bytes()).await.unwrap()).contains("MSA|AA"));
    }
    // The other destination carries on; the paused one gets nothing.
    let archived = eventually(async || std::fs::read_dir(&out_dir).map(|d| d.count()).unwrap_or(0) == 3);
    assert!(archived.await, "archive isn't paused");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(count(), 0, "nothing sent while paused");
    let (ok, out) = cli(&["pause", &name, "lab", "--reason", "lab vendor breach: hold until cleared"]);
    assert!(ok && out.contains("reason: lab vendor breach"), "{out}");
    let status = lab().await;
    assert_eq!(
        (&status["pending"], &status["paused"]["reason"]),
        (&3.into(), &"lab vendor breach: hold until cleared".into())
    );
    assert_eq!(status["paused"]["by"], format!("{name}-ops"));
    let metrics = http.get(format!("{base}/metrics")).bearer_auth(&ops).send().await.unwrap().text().await.unwrap();
    assert!(
        metrics.contains(&format!("ductus_destination_paused{{interface=\"{name}\",destination=\"lab\"}} 1")),
        "{metrics}"
    );
    // Its health says paused (not stalled), by whom and why; the other destination is fine.
    assert_eq!(status["health"]["state"], "paused", "{status}");
    let summary = status["health"]["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(&format!("paused by {name}-ops at "))
            && summary.ends_with(": lab vendor breach: hold until cleared, 3 waiting"),
        "{summary}"
    );
    assert!(metrics.contains(&format!("ductus_destination_state{{interface=\"{name}\",destination=\"lab\"}} 4")));
    let (ok, out) = run(&["status", "--api", &api_url, "--token", &ops]);
    assert!(ok && out.contains("health:"), "{out}");
    assert!(out.lines().any(|l| l.contains("→ lab") && l.contains("paused by")), "{out}");
    assert!(out.lines().any(|l| l.contains("→ archive") && l.contains("ok, sent")), "{out}");

    // A drain doesn't wait for a paused destination.
    let (ok, out) = cli(&["drain", "--wait"]);
    assert!(ok && out.contains("drained"), "{out}");
    assert!(cli(&["resume"]).0);

    // A restart (or a failover) keeps the pause.
    drop(engine);
    engine = start().await;
    let status = lab().await;
    assert_eq!(status["paused"]["reason"], "lab vendor breach: hold until cleared", "{status}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(count(), 0, "still paused after the restart");

    // Resume: the backlog goes, in order.
    let (ok, out) = cli(&["resume", &name, "lab"]);
    assert!(ok && out.contains("resumed") && out.contains("sending 3 waiting"), "{out}");
    assert!(eventually(async || count() == 3).await, "sent once resumed");
    assert_eq!(*received.lock().unwrap(), ["C1", "C2", "C3"]);
    let (ok, out) = cli(&["resume", &name, "lab"]);
    assert!(ok && out.contains("wasn't paused"), "{out}");
    assert!(lab().await["paused"].is_null());

    drop(engine);
    run(&["token", "revoke", &format!("{name}-ops")]);
}

/// Acting on messages through the API: the quarantine across interfaces, an edited copy that
/// replaces a rejected message, a replay to one destination of what it was already sent, and a
/// bulk resend; each recorded with who did it and what the delivery was before.
#[tokio::test(flavor = "multi_thread")]
async fn messages_are_resent_and_corrected_through_the_api() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let (mllp, receiver, api) = (free_port(), free_port(), free_port());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("actions-test-{nanos}");
    let config = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\nordering: {{ key: PID-3 }}\ndestinations:\n  lab: {{ mllp: {{ host: 127.0.0.1, port: {receiver}, timeout_ms: 500 }} }}\n  archive: {{ file: {{ dir: out }} }}\n"
    );
    let dir = project(&[(format!("interfaces/{name}/interface.yaml").as_str(), config.as_str())]);
    let out_dir = dir.path().join(format!("interfaces/{name}/out"));
    let run = |args: &[&str]| {
        let out =
            Command::new(env!("CARGO_BIN_EXE_ductus")).args(args).env("DUCTUS_DATABASE_URL", &db).output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let token = |role: &str| {
        let (ok, out) = run(&["token", "create", &format!("{name}-{role}"), "--role", role]);
        assert!(ok, "{out}");
        out.lines().next().unwrap().to_string()
    };
    let (ops, viewer) = (token("operator"), token("viewer"));

    // The lab rejects (AR) anything with BAD in it, and keeps the control ids it accepted.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", receiver)).await.unwrap();
    let accepted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let got = accepted.clone();
    tokio::spawn(ductus_mllp::serve(listener, move |m: bytes::Bytes| {
        let text = String::from_utf8_lossy(&m).to_string();
        let code = if text.contains("BAD") { "AR" } else { "AA" };
        if code == "AA" {
            got.lock().unwrap().push(text.split('|').nth(9).unwrap_or("").to_string());
        }
        async move { bytes::Bytes::from(format!("MSH|^~\\&|LAB|H|EPIC|H|20260101||ACK|1|P|2.5\rMSA|{code}|1\r")) }
    }));
    let accepted_ids = || accepted.lock().unwrap().clone();

    let _engine = Child(
        Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["run", "--root", &dir.path().to_string_lossy(), "--threads", "1"])
            .args(["--api-listen", &format!("127.0.0.1:{api}")])
            .env("DUCTUS_DATABASE_URL", &db)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let base = format!("http://127.0.0.1:{api}/api/v1");
    let http = reqwest::Client::new();
    let mut up = false;
    for _ in 0..100 {
        if http.get(format!("{base}/health")).send().await.is_ok_and(|r| r.status().is_success()) {
            up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(up, "the API never came up");
    let get = async |path: &str| -> serde_json::Value {
        http.get(format!("{base}{path}")).bearer_auth(&viewer).send().await.unwrap().json().await.unwrap()
    };
    let post = async |path: &str, token: &str, body: serde_json::Value| -> (u16, serde_json::Value) {
        let r = http.post(format!("{base}{path}")).bearer_auth(token).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    };
    let quarantined = async || -> Vec<String> {
        let page = get(&format!("/quarantine?interface={name}")).await;
        page["deliveries"].as_array().unwrap().iter().map(|d| d["control_id"].as_str().unwrap().to_string()).collect()
    };

    let mut sender = ductus_mllp::Client::new(format!("127.0.0.1:{mllp}"), std::time::Duration::from_secs(5));
    let mut send = async |control: &str, mrn: &str, note: &str| {
        let msg = format!("MSH|^~\\&|EPIC|H|LAB|H|20260101||ADT^A01|{control}|P|2.5\rPID|1||{mrn}\rNTE|1||{note}\r");
        assert!(String::from_utf8_lossy(&sender.send(msg.as_bytes()).await.unwrap()).contains("MSA|AA"));
    };
    send("C1", "MRN1", "fine").await;
    send("C2", "MRN2", "BAD unit").await;
    send("C3", "MRN3", "fine").await;
    assert!(eventually(async || quarantined().await == ["C2"]).await, "the lab rejected C2");
    assert!(eventually(async || accepted_ids().len() == 2).await);
    let archived = || std::fs::read_dir(&out_dir).map(|d| d.count()).unwrap_or(0);
    assert!(eventually(async || archived() == 3).await, "the archive has all three");
    let page = get(&format!("/interfaces/{name}/messages")).await;
    let id = |control: &str| {
        page["messages"].as_array().unwrap().iter().find(|m| m["control_id"] == control).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let (c1, c2) = (id("C1"), id("C2"));
    let entry = &get("/quarantine").await["deliveries"].as_array().unwrap().iter().find(|d| d["id"] == c2).cloned();
    let entry = entry.as_ref().expect("across interfaces too");
    assert_eq!((&entry["destination"], &entry["interface"]), (&"lab".into(), &name.as_str().into()), "{entry}");

    // Viewers can't act; unknown messages and destinations are said plainly.
    assert_eq!(post(&format!("/messages/{c2}/resend"), &viewer, serde_json::json!({})).await.0, 403);
    assert_eq!(post("/messages/999999999999/resend", &ops, serde_json::json!({})).await.0, 404);
    let (status, body) =
        post(&format!("/messages/{c2}/resend"), &ops, serde_json::json!({"destinations": ["billing"]})).await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("no destination `billing` (it has archive, lab)"), "{body}");

    // Correct C2: a new message, with its own keys, for the lab only; the original's rejected
    // delivery is replaced, so the original counts as done.
    let fixed = "MSH|^~\\&|EPIC|H|LAB|H|20260101||ADT^A01|C2-FIX|P|2.5\rPID|1||MRN2\rNTE|1||unit corrected\r";
    let (status, edit) =
        post(&format!("/messages/{c2}/edit"), &ops, serde_json::json!({"payload": fixed, "reason": "unit typo"})).await;
    assert_eq!(status, 201, "{edit}");
    assert_eq!((&edit["destinations"], &edit["replaced"]), (&serde_json::json!(["lab"]), &serde_json::json!(["lab"])));
    let copy = edit["id"].as_i64().unwrap();
    assert!(eventually(async || accepted_ids().contains(&"C2-FIX".to_string())).await, "the copy is sent");
    assert!(eventually(async || quarantined().await.is_empty()).await);
    let original = get(&format!("/messages/{c2}")).await;
    assert_eq!(original["status"], "sent", "{original}");
    let lab = original["deliveries"].as_array().unwrap().iter().find(|d| d["destination"] == "lab").unwrap();
    assert_eq!(
        (&lab["status"], &lab["last_error"]),
        (&"filtered".into(), &format!("replaced by edited copy {copy}").into())
    );
    let action = &original["actions"][0];
    assert_eq!(
        (&action["action"], &action["by"], &action["reason"]),
        (&"edit".into(), &format!("{name}-operator").into(), &"unit typo".into())
    );
    assert_eq!((&action["previous_status"], &action["copy"]), (&"quarantined".into(), &copy.into()));
    let made = get(&format!("/messages/{copy}")).await;
    assert_eq!(
        (&made["copy_of"], &made["control_id"], &made["status"]),
        (&c2.into(), &"C2-FIX".into(), &"sent".into())
    );
    assert_eq!(made["deliveries"].as_array().unwrap().len(), 1, "for the lab only: {made}");
    assert_eq!(std::fs::read_dir(&out_dir).unwrap().count(), 3, "the archive isn't sent the copy");

    // A replay: the lab says it lost C1. Without a destination, nothing is quarantined to resend.
    let (status, body) = post(&format!("/messages/{c1}/resend"), &ops, serde_json::json!({})).await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("name the destination"), "{body}");
    let (status, body) =
        post(&format!("/messages/{c1}/resend"), &ops, serde_json::json!({"destinations": ["lab"]})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        (&body["destinations"][0]["destination"], &body["destinations"][0]["previous_status"]),
        (&"lab".into(), &"sent".into())
    );
    assert!(eventually(async || accepted_ids().iter().filter(|c| *c == "C1").count() == 2).await, "C1 again");
    assert_eq!(std::fs::read_dir(&out_dir).unwrap().count(), 3, "only the lab gets it again");

    // Bulk: two rejected again (still BAD), and one with nothing quarantined is skipped.
    send("C4", "MRN4", "BAD").await;
    send("C5", "MRN5", "BAD").await;
    assert!(eventually(async || quarantined().await.len() == 2).await);
    let page = get(&format!("/interfaces/{name}/messages")).await;
    let ids: Vec<i64> = ["C4", "C5"]
        .iter()
        .map(|c| {
            page["messages"].as_array().unwrap().iter().find(|m| m["control_id"] == *c).unwrap()["id"].as_i64().unwrap()
        })
        .collect();
    let (status, body) = post("/messages/resend", &ops, serde_json::json!({"ids": [ids[0], ids[1], c1]})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!((body["resent"].as_array().unwrap().len(), &body["skipped"]), (2, &serde_json::json!([c1])));
    let resent =
        eventually(async || get(&format!("/messages/{}", ids[0])).await["actions"].as_array().unwrap().len() == 1);
    assert!(resent.await);
    assert!(eventually(async || quarantined().await.len() == 2).await, "rejected again");
    assert_eq!(post("/messages/resend", &ops, serde_json::json!({"ids": []})).await.0, 400);

    // Live updates carry each interface's and destination's health.
    let mut events = http.get(format!("{base}/events")).bearer_auth(&viewer).send().await.unwrap();
    let mut text = String::new();
    while !text.contains("\n\n") {
        text.push_str(&String::from_utf8_lossy(&events.chunk().await.unwrap().unwrap()));
    }
    let data = text.lines().find_map(|l| l.strip_prefix("data:")).unwrap();
    let update: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
    let mine = update["interfaces"].as_array().unwrap().iter().find(|i| i["name"] == name.as_str()).unwrap();
    assert!(mine["health"]["summary"].is_string(), "{mine}");
    let lab = mine["destinations"].as_array().unwrap().iter().find(|d| d["name"] == "lab").unwrap();
    assert_eq!((&lab["health"]["state"], &lab["quarantined"]), (&"error".into(), &2.into()), "{lab}");
    drop(events);

    let spec: serde_json::Value = http.get(format!("{base}/openapi.json")).send().await.unwrap().json().await.unwrap();
    for path in ["/messages/{id}/resend", "/messages/resend", "/messages/{id}/edit", "/quarantine"] {
        assert!(spec["paths"][path].is_object(), "{path} missing from the OpenAPI document");
    }
    for role in ["operator", "viewer"] {
        run(&["token", "revoke", &format!("{name}-{role}")]);
    }
}

/// The web UI comes with the engine: any path outside `/api` is the app (its router shows the
/// page), with a CSP and the right caching. A browser signs in with a token and gets a session
/// cookie that stands in for it; changes made with the cookie need the `X-Ductus` header (so
/// other sites can't use someone's session); signing out or revoking the token ends it.
#[tokio::test(flavor = "multi_thread")]
async fn the_ui_is_served_and_a_browser_signs_in_with_a_session() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let api = free_port();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("session-test-{nanos}");
    let dir = project(&[(
        format!("interfaces/{name}/interface.yaml").as_str(),
        format!("name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {} }} }}\ndestination: {{ file: {{ dir: out }} }}\n", free_port()).as_str(),
    )]);
    let run = |args: &[&str]| {
        let out =
            Command::new(env!("CARGO_BIN_EXE_ductus")).args(args).env("DUCTUS_DATABASE_URL", &db).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let (ok, token) = run(&["token", "create", &format!("{name}-ops"), "--role", "operator"]);
    assert!(ok, "{token}");
    let _engine = Child(
        Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["run", "--root", &dir.path().to_string_lossy(), "--threads", "1"])
            .args(["--api-listen", &format!("127.0.0.1:{api}")])
            .env("DUCTUS_DATABASE_URL", &db)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{api}");
    let base = format!("{url}/api/v1");
    let http = reqwest::Client::new();
    let mut up = false;
    for _ in 0..100 {
        if http.get(format!("{base}/health")).send().await.is_ok_and(|r| r.status().is_success()) {
            up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(up, "the API never came up");

    // The app, at every path its router knows (or doesn't: it shows "nothing here" itself).
    for path in ["/", "/interfaces/adt", "/messages/12"] {
        let page = http.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(page.status(), 200, "{path}");
        let h = page.headers();
        assert!(h["content-type"].to_str().unwrap().starts_with("text/html"), "{path}");
        assert_eq!(h["cache-control"], "no-cache", "{path}: an upgrade must show at once");
        assert!(h["content-security-policy"].to_str().unwrap().contains("frame-ancestors 'none'"), "{path}");
        assert!(page.text().await.unwrap().contains("<title>Ductus</title>"), "{path}");
    }
    // A hashed file of another version is a 404, never HTML; unknown API paths answer in JSON.
    assert_eq!(http.get(format!("{url}/assets/index-gone.js")).send().await.unwrap().status(), 404);
    let missing = http.get(format!("{base}/nope")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
    assert!(missing.text().await.unwrap().contains("\"error\""));
    assert_eq!(http.post(format!("{url}/interfaces")).send().await.unwrap().status(), 405);

    // Signing in: needs the header, and a live token.
    let sign_in = |token: &str, header: bool| {
        let request = http.post(format!("{base}/session")).json(&serde_json::json!({ "token": token }));
        (if header { request.header("X-Ductus", "1") } else { request }).send()
    };
    assert_eq!(sign_in(&token, false).await.unwrap().status(), 403);
    assert_eq!(sign_in("dux_wrong", true).await.unwrap().status(), 401);
    let signed_in = sign_in(&token, true).await.unwrap();
    assert_eq!(signed_in.status(), 200);
    let set_cookie = signed_in.headers()["set-cookie"].to_str().unwrap().to_string();
    for part in ["ductus_session=dus_", "HttpOnly", "Secure", "SameSite=Strict", "Path=/api", "Max-Age=6047"] {
        assert!(set_cookie.contains(part), "{part} missing from {set_cookie}");
    }
    let me: serde_json::Value = signed_in.json().await.unwrap();
    assert_eq!((&me["name"], &me["role"]), (&format!("{name}-ops").into(), &"operator".into()), "{me}");
    assert!(me["ends_at"].is_string(), "{me}");
    let cookie = set_cookie.split(';').next().unwrap().to_string();

    // The cookie reads like the token; a change also needs the header.
    let get = |path: &str| http.get(format!("{base}{path}")).header("cookie", &cookie).send();
    let interfaces: serde_json::Value = get("/interfaces").await.unwrap().json().await.unwrap();
    assert!(interfaces.to_string().contains(&name), "{interfaces}");
    let pause = format!("{base}/interfaces/{name}/destinations/default/pause");
    let refused = http.post(&pause).header("cookie", &cookie).send().await.unwrap();
    assert_eq!(refused.status(), 403);
    assert!(refused.text().await.unwrap().contains("X-Ductus"));
    let paused = http.post(&pause).header("cookie", &cookie).header("X-Ductus", "1").send().await.unwrap();
    assert_eq!(paused.status(), 200);
    http.delete(&pause).header("cookie", &cookie).header("X-Ductus", "1").send().await.unwrap();
    // Live updates take the cookie too (a browser's EventSource sends it).
    let events = get("/events").await.unwrap();
    assert_eq!(events.status(), 200);
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    drop(events);

    // Signing out ends it at once; so does revoking the token.
    let out = http.delete(format!("{base}/session")).header("cookie", &cookie).send().await.unwrap();
    assert_eq!(out.status(), 204);
    assert!(out.headers()["set-cookie"].to_str().unwrap().contains("Max-Age=0"));
    assert_eq!(get("/interfaces").await.unwrap().status(), 401);
    let again = sign_in(&token, true).await.unwrap();
    let cookie = again.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    assert_eq!(http.get(format!("{base}/session")).header("cookie", &cookie).send().await.unwrap().status(), 200);
    run(&["token", "revoke", &format!("{name}-ops")]);
    let ended = http.get(format!("{base}/session")).header("cookie", &cookie).send().await.unwrap();
    assert_eq!(ended.status(), 401);
    assert!(ended.text().await.unwrap().contains("sign in again"));
}

const DOCS_SCHEMA: &str = r#"
fields:
  data_classification: { type: enum, values: [PHI, Internal, Public], required: true, label: Data classification }
  baa_signed: { type: enum, values: ["yes", "no", n/a], required: true, applies_to: [interface, resource] }
  go_live: { type: date }
  runbook: { type: url, applies_to: [interface, destination] }
"#;

fn edit(path: &std::path::Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from} not in {}", path.display());
    std::fs::write(path, text.replacen(from, to, 1)).unwrap();
}

#[test]
fn validate_checks_company_defined_fields_and_docs_lists_them() {
    let dir = hospital();
    let root = dir.path();
    let (ok, out) = ductus(root, &["validate"]);
    assert!(ok, "no schema, no values: nothing to say: {out}");
    let (ok, out) = ductus(root, &["docs"]);
    assert!(ok && out.contains("no fields are declared"), "{out}");

    // A value with no schema is undeclared.
    edit(&root.join("interfaces/adt-in/interface.yaml"), "source:", "custom: { go_live: 2026-10-13 }\nsource:");
    let (ok, out) = ductus(root, &["validate"]);
    assert!(!ok && out.contains("✗ adt-in: custom.go_live isn't declared in docs-schema.yaml"), "{out}");

    // With the schema: required fields are missing everywhere they apply.
    std::fs::write(root.join("docs-schema.yaml"), DOCS_SCHEMA).unwrap();
    let (ok, out) = ductus(root, &["validate"]);
    assert!(!ok, "{out}");
    for line in [
        "✗ adt-in: custom.data_classification is required (one of: PHI, Internal, Public)",
        "✗ adt-in: custom.baa_signed is required (one of: yes, no, n/a)",
        "✗ results: custom.baa_signed is required (one of: yes, no, n/a)",
        "✗ resources.yaml api empi: custom.baa_signed is required (one of: yes, no, n/a)",
        "✗ resources.yaml sftp server quest: custom.baa_signed is required",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    assert!(!out.contains("adt-in.default: custom.baa"), "a destination isn't asked for interface fields: {out}");

    // Wrong values say what's allowed; typos get a hint; fields stay where they apply.
    let adt = root.join("interfaces/adt-in/interface.yaml");
    edit(
        &adt,
        "custom: { go_live: 2026-10-13 }",
        "custom: { data_classification: phi, baa_signed: \"yes\", go_live: 13/10/2026, runbok: \"https://w/x\" }",
    );
    edit(
        &adt,
        "destination: { mllp: { host: 10.50.0.7, port: 6662 } }",
        "destination: { mllp: { host: 10.50.0.7, port: 6662 }, custom: { go_live: 2026-10-13, runbook: wiki } }",
    );
    let (ok, out) = ductus(root, &["validate"]);
    assert!(!ok, "{out}");
    for line in [
        "✗ adt-in: custom.data_classification is `phi`; allowed: PHI, Internal, Public (case matters: write `PHI`)",
        "✗ adt-in: custom.go_live `13/10/2026` isn't a date; write it as 2026-10-13",
        "✗ adt-in: custom.runbok isn't declared in docs-schema.yaml (did you mean `runbook`?)",
        "✗ adt-in.default: custom.go_live doesn't apply to a destination (it applies to: interface)",
        "✗ adt-in.default: custom.runbook `wiki` isn't an http(s) address",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }

    // Fix everything: validate passes, `docs` shows the values, --missing is quiet.
    edit(
        &adt,
        "custom: { data_classification: phi, baa_signed: \"yes\", go_live: 13/10/2026, runbok: \"https://w/x\" }",
        "custom: { data_classification: PHI, baa_signed: \"yes\", go_live: 2026-10-13, runbook: \"https://w/x\" }",
    );
    edit(&adt, "custom: { go_live: 2026-10-13, runbook: wiki }", "custom: { runbook: \"https://w/mllp\" }");
    edit(
        &root.join("interfaces/results/interface.yaml"),
        "source:",
        "custom: { data_classification: Internal, baa_signed: n/a }\nsource:",
    );
    let resources = root.join("resources.yaml");
    edit(
        &resources,
        "application: EMPI, vendor: Rhapsody }",
        "application: EMPI, vendor: Rhapsody, custom: { baa_signed: \"yes\" } }",
    );
    edit(&resources, "vendor: Experian }", "vendor: Experian, custom: { baa_signed: \"no\" } }");
    edit(&resources, "vendor: Quest }", "vendor: Quest, custom: { baa_signed: \"yes\" } }");
    edit(&resources, "vendor: Epic Systems } }", "vendor: Epic Systems, custom: { baa_signed: \"yes\" } } }");
    let (ok, out) = ductus(root, &["validate"]);
    assert!(ok && !out.contains("custom."), "{out}");

    let (ok, out) = ductus(root, &["docs"]);
    assert!(ok, "{out}");
    assert!(out.contains("adt-in (interface)") && out.contains("Data classification  PHI"), "{out}");
    assert!(
        out.contains("go_live                2026-10-13") || out.contains("go_live              2026-10-13"),
        "{out}"
    );
    assert!(out.contains("results (interface)") && out.contains("go_live") && out.contains("–"), "{out}");
    let (ok, out) = ductus(root, &["docs", "--missing"]);
    assert!(ok && out.contains("every required field has a value"), "{out}");

    // --missing lists what's not there and exits 1; --json is for scripts.
    edit(&root.join("interfaces/results/interface.yaml"), "baa_signed: n/a", "baa_signed: ' '");
    let (ok, out) = ductus(root, &["docs", "--missing"]);
    assert!(
        !ok && out.contains("results (interface)") && out.contains("⚠ missing") && !out.contains("adt-in"),
        "{out}"
    );
    let (ok, out) = ductus(root, &["docs", "--json"]);
    assert!(!ok);
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let results = rows.as_array().unwrap().iter().find(|r| r["name"] == "results").unwrap();
    assert_eq!(results["missing"], serde_json::json!(["baa_signed"]));
    assert_eq!(results["custom"]["data_classification"], "Internal");

    // A broken schema is reported by name.
    std::fs::write(root.join("docs-schema.yaml"), "fields: { owner: { type: text } }").unwrap();
    let (ok, out) = ductus(root, &["validate"]);
    assert!(!ok && out.contains("✗ docs-schema.yaml:") && out.contains("ductus already uses that name"), "{out}");
}
