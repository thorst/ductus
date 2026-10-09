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
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\nabout: {{ owner: Lab team }}\ndestinations:\n  lab: {{ mllp: {{ host: 127.0.0.1, port: {receiver}, timeout_ms: 500 }} }}\n"
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

    let owners = json("/owners", &viewer).await;
    assert!(owners.to_string().contains("Lab team"), "{owners}");
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
