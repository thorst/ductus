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
  PORT  PROTOCOL  HOST     INTERFACE  ROUTE
  7661  MLLP      0.0.0.0  adt
  8080  HTTP      0.0.0.0  api        GET /patients/{mrn} (queue)

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
}
