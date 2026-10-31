//! The `ductus` command run as a user would: on a project folder, checking output and exit code.

use ductus_store::testing::TestUsers;
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

/// The docs' reference pages are made by `ductus reference docs` and committed: a change to a
/// command, an API operation, a setting, an Event Log ID, a metric or an audit action that isn't
/// written into them fails here.
#[test]
fn the_reference_pages_are_current() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(docs.join("SUMMARY.md"), dir.path().join("SUMMARY.md")).unwrap();
    let (ok, out) = ductus(dir.path(), &["reference", dir.path().to_str().unwrap()]);
    assert!(ok, "{out}");
    let read = |p: &Path| std::fs::read_to_string(p).unwrap().replace("\r\n", "\n");
    let mut stale = Vec::new();
    if read(&dir.path().join("SUMMARY.md")) != read(&docs.join("SUMMARY.md")) {
        stale.push("SUMMARY.md".to_string());
    }
    fn files(root: &Path, at: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { files(root, &p, out) } else { out.push(p.strip_prefix(root).unwrap().to_path_buf()) }
        }
    }
    let (mut made, mut committed) = (Vec::new(), Vec::new());
    files(&dir.path().join("reference"), &dir.path().join("reference"), &mut made);
    files(&docs.join("reference"), &docs.join("reference"), &mut committed);
    committed.retain(|p| p != Path::new("libraries.md")); // scripts/dev/dependencies.sh's
    made.sort();
    committed.sort();
    if made != committed {
        stale.push(format!("the pages differ: made {made:?}, committed {committed:?}"));
    }
    for page in made.iter().filter(|p| committed.contains(p)) {
        if read(&dir.path().join("reference").join(page)) != read(&docs.join("reference").join(page)) {
            stale.push(format!("reference/{}", page.display()));
        }
    }
    assert!(stale.is_empty(), "run `ductus reference docs` (check.sh does): stale {stale:?}");
}

/// The libraries page (docs/reference/libraries.md) is made by `scripts/dev/dependencies.sh` from
/// Cargo.lock and the web UI's lock file, and records their checksums: a dependency added, bumped
/// or dropped fails here until the page is made again, so each release lists what it contains.
#[test]
fn the_libraries_page_is_current() {
    use sha2::Digest;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let page = std::fs::read_to_string(repo.join("docs/reference/libraries.md")).unwrap();
    let made_from = page.lines().find_map(|l| l.strip_prefix("<!-- made from: ")).unwrap_or_default();
    // Hashed as Git checks them out (.gitattributes: LF everywhere), so Windows agrees with Linux.
    let sha = |file: &str| {
        let text = std::fs::read_to_string(repo.join(file)).unwrap().replace("\r\n", "\n");
        sha2::Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let now = format!("Cargo.lock {} ui/package-lock.json {} -->", sha("Cargo.lock"), sha("ui/package-lock.json"));
    assert_eq!(made_from, now, "a lock file changed: run `bash scripts/dev/dependencies.sh` and commit the page");
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

/// A CA installed on this server counts for the engine's database as for MLLP and HTTPS: sqlx
/// trusts the system's certificates (sqlx-core's internal `rustls-native-certs` feature; without
/// it, only bundled Mozilla roots). `SSL_CERT_FILE` stands in for the system's store. Guards that
/// feature name across sqlx bumps. A throwaway TLS Postgres (`scripts/dev/pgtls.sh`).
#[test]
fn the_system_s_trusted_certificates_count() {
    let Some(pg) = ductus_store::testing::PgTls::start() else { return };
    let parent = tempfile::tempdir().unwrap();
    assert!(ductus(parent.path(), &["init", "p"]).0);
    let validate = |system: Option<String>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ductus"));
        command
            .arg("validate")
            .current_dir(parent.path().join("p"))
            .env("DUCTUS_DATABASE_URL", pg.url("127.0.0.1", "sslmode=verify-full"))
            .env_remove("SSL_CERT_DIR");
        match system {
            Some(file) => command.env("SSL_CERT_FILE", file),
            None => command.env_remove("SSL_CERT_FILE"),
        };
        String::from_utf8_lossy(&command.output().unwrap().stdout).into_owned()
    };
    let out = validate(Some(pg.file("ca.pem")));
    assert!(out.contains("✓ database: TLS 1.3, verified (sslmode=verify-full)"), "{out}");
    let out = validate(None);
    assert!(out.contains("✗ database: connecting to PostgreSQL over TLS: the database server's certificate isn't from a CA this server trusts"), "{out}");
}

/// SQL Server's protection, said before anything connects: `trust_server_certificate` on another
/// machine is warned about, refused next to `ca`, and `ports` says what each connection will be.
#[test]
fn validate_and_ports_say_how_sql_server_is_protected() {
    let dir = project(&[
        (
            "interfaces/adt/interface.yaml",
            "name: adt\nsource: { mllp: { port: 6661 } }\ndestination: { file: { dir: out } }\n",
        ),
        (
            "resources.yaml",
            "certificates:\n  hospital-ca: { ca: CA }\ndatabases:\n  billing: { sqlserver: { host: sql1.hospital.org, database: b, user: u, trust_server_certificate: true } }\n  claims: { sqlserver: { host: sql2.hospital.org, database: c, user: u, ca: hospital-ca, encrypt: strict } }\n",
        ),
    ]);
    let (_, out) = ductus(dir.path(), &["validate"]);
    assert!(
        out.contains("⚠ database `billing`: `trust_server_certificate: true` doesn't check its certificate"),
        "{out}"
    );
    assert!(!out.contains("`claims`"), "verified: nothing to say: {out}");
    let (ok, out) = ductus(dir.path(), &["ports"]);
    assert!(ok, "{out}");
    assert!(out.contains("TLS, not verified (trust_server_certificate)"), "{out}");
    assert!(out.contains("TDS 8 strict, TLS verified: hospital-ca trusted"), "{out}");

    std::fs::write(
        dir.path().join("resources.yaml"),
        "databases:\n  billing: { sqlserver: { host: h, database: b, user: u, ca: x, trust_server_certificate: true } }\n",
    )
    .unwrap();
    let (ok, out) = ductus(dir.path(), &["validate"]);
    assert!(!ok && out.contains("so `ca` would never be checked"), "{out}");
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

/// Copy a folder into another, recursively.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().map(|e| e.unwrap()) {
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// The code on the docs' "Write interfaces" pages is `examples/guides`' (the CLI crate's
/// `the_guides_code_is_the_example_projects` checks the pages against its files), so the project
/// must pass its own tests and `validate`, as the pages say it does.
#[test]
fn the_guides_example_project_passes_its_tests_and_validate() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/guides");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&source, dir.path());
    let (ok, out) = ductus(dir.path(), &["test"]);
    assert!(ok && out.contains(" passed, 0 failed"), "{out}");

    // Its resources name secrets: give them values, as `ductus secret set` would for a real one.
    let run = |args: &[&str], key: &str, stdin: &str| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(args)
            .current_dir(dir.path())
            .env("DUCTUS_SECRET_KEY", key)
            .env_remove("DUCTUS_DATABASE_URL")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let (ok, key) = run(&["secret", "init"], "", "");
    assert!(ok, "{key}");
    let key = key.lines().next().unwrap().trim().to_string();
    for name in ["BILLING_TOKEN", "EMPI_TOKEN", "FHIR_TOKEN", "PROVIDER_DB_PASSWORD"] {
        let (ok, out) = run(&["secret", "set", name], &key, "a-test-value\n");
        assert!(ok, "{out}");
    }
    let (ok, out) = run(&["validate"], &key, "");
    assert!(ok && out.contains("valid"), "{out}");
    assert!(!out.contains('⚠'), "the guides' examples are written to pass without warnings: {out}");
}

/// A test file that reads a fixture as it loads (outside `test(...)`) is told why it can't,
/// instead of "not found (available: none)".
#[test]
fn a_fixture_read_as_the_test_file_loads_says_to_read_it_in_a_test() {
    let dir = project(&[
        (
            "interfaces/a/interface.yaml",
            "name: a\nsource: { mllp: { port: 6661 } }\nscript: t.ts\ndestination: { file: { dir: out } }\n",
        ),
        (
            "interfaces/a/t.ts",
            "import type { Message } from \"@ductus/sdk\";\nexport function transform(m: Message) { return m; }\n",
        ),
        ("interfaces/a/fixtures/one.hl7", "MSH|^~\\&|A|B|C|D|20260101||ADT^A01|1|P|2.5\r"),
        (
            "interfaces/a/t.test.ts",
            "import { hl7, test, expect } from \"@ductus/sdk\";\nconst msg = hl7(\"one.hl7\");\ntest(\"x\", () => expect(msg.MSH[9][1]).toBe(\"ADT\"));\n",
        ),
    ]);
    let (ok, out) = ductus(dir.path(), &["test"]);
    assert!(!ok, "{out}");
    assert!(out.contains("fixtures can be read only inside a test"), "{out}");
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
  clarity: { postgres: { host: 10.20.30.9, database: clarity, user: u, tls: verify-full, application: Clarity, vendor: Epic Systems } }
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
    // (Owners are warned only with a database: their texts aren't emails or teams.)
    let warned: Vec<&str> = out.lines().filter(|l| l.starts_with('⚠') && !l.starts_with("⚠ owner `")).collect();
    assert!(warned.is_empty(), "every side names its system: {out}");

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
    let pad = |n: usize| " ".repeat(n);
    assert_eq!(
        out,
        format!(
            "environment: prod

Listening
  PORT  PROTOCOL  HOST       INTERFACE          ROUTE{}TLS   ALLOWED
  7661  MLLP      0.0.0.0    adt{}none  anyone
  7700  HTTP      127.0.0.1  (engine REST API)  /api/v1{}none  anyone
  8080  HTTP      0.0.0.0    api                GET /patients/{{mrn}} (queue)  none  anyone

Connects to
  HOST        PORT  PROTOCOL  USED BY{}TLS
  db1         5432  Postgres  resources.yaml: pdb  TLS if offered, not verified (prefer)
  epic.local  5000  MLLP      adt{}none

✓ no port conflicts
",
            pad(24),
            pad(45),
            pad(22),
            pad(14),
            pad(18)
        )
    );

    // TLS in and out: what each listener presents and asks for, what each connection trusts and
    // presents, and the API over HTTPS.
    std::fs::write(
        dir.path().join("resources.yaml"),
        "certificates:\n  web: { cert: C, key: K }\n  hie-ca: { ca: CA }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("interfaces/adt/interface.yaml"),
        "name: adt\nsource: { mllp: { port: ${ADT_PORT}, tls: { certificate: web, clients: { ca: hie-ca } }, allow: [10.1.20.0/24, epic-bridge01] } }\ndestination: { mllp: { host: ${EPIC}, port: 5000, tls: { ca: hie-ca } } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ductus.yaml"),
        "api: { listen: '0.0.0.0:7700', tls: { certificate: web }, allow: [127.0.0.1] }\n",
    )
    .unwrap();
    let (ok, out) = ductus(dir.path(), &["ports", "--env", "prod"]);
    assert!(ok, "{out}");
    let line = |start: &str| out.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_default().to_string();
    let words =
        |l: String| l.split("  ").map(str::trim).filter(|w| !w.is_empty()).map(String::from).collect::<Vec<_>>();
    assert_eq!(
        words(line("7661")),
        [
            "7661",
            "MLLP over TLS",
            "0.0.0.0",
            "adt",
            "presents web, clients need a certificate from hie-ca",
            "10.1.20.0/24, epic-bridge01"
        ],
        "{out}"
    );
    assert_eq!(
        words(line("7700")),
        ["7700", "HTTPS", "0.0.0.0", "(engine REST API)", "/api/v1", "presents web", "127.0.0.1"]
    );
    assert_eq!(words(line("epic.local")), ["epic.local", "5000", "MLLP over TLS", "adt", "TLS: hie-ca trusted"]);
    std::fs::remove_file(dir.path().join("ductus.yaml")).unwrap();
    std::fs::write(
        dir.path().join("interfaces/adt/interface.yaml"),
        "name: adt\nsource: { mllp: { port: ${ADT_PORT} } }\ndestination: { mllp: { host: ${EPIC}, port: 5000 } }\n",
    )
    .unwrap();

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
        (
            "resources.yaml",
            "apis:\n  portal: { url: \"https://portal\", owner: Ann Smith }\n  spare: { url: \"https://spare\" }\n",
        ),
    ]);
    let (ok, out) = without_a_database(dir.path(), &["owners"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("Ann Smith (3)\n  adt\n  orders\n  api portal\n\nBilling team (1)\n  adt → billing\n\nno owner (1)\n  nightly"),
        "{out}"
    );
    assert!(out.contains("owners aren't matched to accounts without the database"), "{out}");
    let (ok, out) = without_a_database(dir.path(), &["owners", "--check"]);
    assert!(!ok && out.contains("no database: set DUCTUS_DATABASE_URL"), "{out}");

    // Split Ann's work: orders to Bob first, as a dry run, then for real; then the rest to Carol.
    let (ok, out) = ductus(
        dir.path(),
        &["owners", "reassign", "--from", "Ann Smith", "--to", "Bob", "--interface", "orders", "--dry-run"],
    );
    assert!(ok && out.contains("- about: { owner: ann smith }\n") && out.contains("+ about: { owner: Bob }"), "{out}");
    assert!(out.contains("dry run: would change 1 owner value(s) in 1 file(s)"), "{out}");
    assert!(without_a_database(dir.path(), &["owners"]).1.contains("Ann Smith (3)"), "a dry run writes nothing");
    let (ok, out) =
        ductus(dir.path(), &["owners", "reassign", "--from", "Ann Smith", "--to", "Bob", "--interface", "orders"]);
    assert!(ok && out.contains("changed 1 owner value(s)"), "{out}");
    // Naming an interface leaves resources.yaml alone; naming a resource moves only it.
    assert!(std::fs::read_to_string(dir.path().join("resources.yaml")).unwrap().contains("owner: Ann Smith"));
    let (ok, out) =
        ductus(dir.path(), &["owners", "reassign", "--from", "Ann Smith", "--to", "Dee", "--resource", "portal"]);
    assert!(
        ok && out.contains("resources.yaml\n  2  - portal: { url: \"https://portal\", owner: Ann Smith }   (api portal)\n     + portal: { url: \"https://portal\", owner: Dee }"),
        "{out}"
    );
    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "ann smith", "--to", "Carol"]);
    assert!(ok && out.contains("interfaces/adt/interface.yaml\n  4  - owner: Ann Smith   # on call: weekdays\n     + owner: Carol   # on call: weekdays"), "{out}");
    let written = std::fs::read_to_string(dir.path().join("interfaces/adt/interface.yaml")).unwrap();
    assert_eq!(written, adt.replace("Ann Smith", "Carol"), "only the value changed");
    let (_, out) = without_a_database(dir.path(), &["owners"]);
    assert!(
        out.contains("Billing team (1)")
            && out.contains("Bob (1)\n  orders")
            && out.contains("Carol (1)\n  adt")
            && out.contains("Dee (1)\n  api portal"),
        "{out}"
    );

    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "Ann Smith", "--to", "X"]);
    assert!(!ok && out.contains("no `owner: Ann Smith` found (owners: Billing team, Bob, Carol, Dee)"), "{out}");
    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "Bob", "--to", "X", "--interface", "nope"]);
    assert!(!ok && out.contains("no interface named nope"), "{out}");
    let (ok, out) = ductus(dir.path(), &["owners", "reassign", "--from", "Bob", "--to", "X", "--resource", "nope"]);
    assert!(!ok && out.contains("no resource named nope in resources.yaml"), "{out}");
}

/// With the database, `ductus owners` says which owners are accounts (by username, email or
/// display name), teams in `owners:`, or nobody; `--check` fails until each reaches someone.
#[test]
fn owners_are_matched_to_accounts() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // What this test makes goes when the guard drops at the end, pass or fail (a plain test: no
    // runtime to block); the audit rows stay, as they would for anyone.
    let people = TestUsers::new(&db);
    let (olaf, gone) = (people.name("test-user-owner"), people.name("test-user-gone"));
    let (olaf_name, gone_name, nobody) =
        (format!("Olaf Night {nanos}"), format!("Gone {nanos}"), format!("Nobody {nanos}"));
    let iface = |name: &str, owner: &str| {
        (
            format!("interfaces/{name}/interface.yaml"),
            format!(
                "name: {name}\nsource: {{ mllp: {{ port: {} }} }}\nabout: {{ owner: \"{owner}\" }}\ndestination: {{ file: {{ dir: out }} }}\n",
                free_port()
            ),
        )
    };
    let files = [iface("a", &olaf_name), iface("b", &gone_name), iface("c", "lab systems"), iface("d", &nobody)];
    let mut files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let resources = format!("apis:\n  portal: {{ url: \"https://portal\", owner: \"{gone_name}\" }}\n");
    files.push(("resources.yaml", &resources));
    files.push(("ductus.yaml", "owners:\n  Lab Systems: { email: lab@example.org }\n"));
    let dir = project(&files);
    let run = |args: &[&str], stdin: &str| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(args)
            .current_dir(dir.path())
            .env("DUCTUS_DATABASE_URL", &db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let password = "correct horse staple\n";
    let email = format!("olaf-{nanos}@example.org");
    for (user, name, email) in [(&olaf, &olaf_name, Some(email.as_str())), (&gone, &gone_name, None)] {
        let mut args = vec!["user", "create", user, "--role", "operator", "--display-name", name, "--password-stdin"];
        args.extend(email.map(|e| ["--email", e]).into_iter().flatten());
        let (ok, out) = run(&args, password);
        assert!(ok, "{out}");
    }
    assert!(run(&["user", "disable", &gone], "").0);

    let (ok, out) = run(&["owners"], "");
    assert!(ok, "without --check it only reports: {out}");
    assert!(
        out.contains(&format!(
            "⚠ {olaf_name} (1): user {olaf} ({olaf_name}, {email})\n  write their email: {email}\n  fix: ductus owners reassign --from '{olaf_name}' --to {email}\n  a"
        )),
        "a display name works, but should be the email: {out}"
    );
    assert!(
        out.contains(&format!("✗ {gone_name} (2): account {gone} ({gone_name}) is disabled\n  b\n  api portal")),
        "{out}"
    );
    assert!(out.contains("✓ lab systems (1): team (`owners:` in ductus.yaml)\n  c"), "{out}");
    assert!(out.contains(&format!("✗ {nobody} (1): not a user, not in `owners:`, not an address\n  d")), "{out}");
    assert!(out.contains("2 owner(s) reach nobody or the wrong person"), "{out}");
    let (ok, _) = run(&["owners", "--check"], "");
    assert!(!ok);

    // Gone's interface and resource move to Olaf (by email), the unknown owner to the team.
    let (ok, out) = run(&["owners", "reassign", "--from", &gone_name, "--to", &email], "");
    assert!(ok && out.contains("changed 2 owner value(s) in 2 file(s)"), "{out}");
    assert!(run(&["owners", "reassign", "--from", &nobody, "--to", "Lab Systems"], "").0);
    let (ok, out) = run(&["owners", "--check"], "");
    assert!(ok, "{out}");
    assert!(out.contains(&format!("✓ {email} (2): user {olaf}")), "{out}");
    assert!(out.contains("1 owner(s) reach their person but should be written as their email"), "{out}");

    // Olaf is renamed (an admin's edit, as the API makes it): his old display name still reaches
    // him, with the fix; a warning, so --check passes.
    let renamed = format!("Olaf N {nanos}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(ductus_store::Store::connect(&db, 1)).unwrap();
    let account = rt.block_on(store.user_by_name(&olaf)).unwrap().unwrap();
    let audit = ductus_store::Audit::new(ductus_store::By::cli("test"), "test");
    let change = ductus_store::UserChange { display_name: Some(renamed.clone()), ..Default::default() };
    rt.block_on(store.update_user(account.id, &change, &audit)).unwrap();
    let (ok, out) = run(&["owners", "--check"], "");
    assert!(ok, "renamed is a warning: {out}");
    let today = chrono::Utc::now().format("%Y-%m-%d");
    assert!(
        out.contains(&format!(
            "⚠ {olaf_name} (1): user {olaf} ({renamed}, {email}), by a name they had before\n  renamed: {olaf}'s display name until {today} (now {renamed}): write their email: {email}\n  fix: ductus owners reassign --from '{olaf_name}' --to {email}"
        )),
        "{out}"
    );

    // Someone else is given the old name now: it's theirs (one rule), flagged as reused.
    let other = people.name("test-user-other");
    let (ok, out) = run(
        &["user", "create", &other, "--display-name", &olaf_name, "--email", "other@example.org", "--password-stdin"],
        password,
    );
    assert!(ok, "{out}");
    let (ok, out) = run(&["owners", "--check"], "");
    assert!(ok, "reused is a warning: {out}");
    assert!(
        out.contains(&format!(
            "⚠ {olaf_name} (1): user {other} ({olaf_name}, other@example.org) (the name was {olaf}'s before)\n  reused: {other} has it now; it was {olaf}'s display name until {today}: alerts reach {other}; write the email of the one meant (other@example.org now, {email} before)\n  fix: ductus owners reassign --from '{olaf_name}' --to other@example.org"
        )),
        "{out}"
    );
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
        let (ok, out) =
            run(&["token", "create", &format!("{name}-{role}"), "--role", role, "--permission", "payloads"]);
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
    assert_eq!((&owners[0]["reach"], &owners[0]["you"]), (&"nobody".into(), &false.into()), "not an account: {owners}");
    assert!(json("/owners/people", &viewer).await.is_array());
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
    assert_eq!(p["to_reach"], "nobody");
    assert!(
        p["warning"]
            .as_str()
            .unwrap()
            .contains("isn't an account or a team in `owners:`: alerts for these would reach nobody"),
        "{p}"
    );
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
        "/owners/people",
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
        let (ok, out) =
            run(&["token", "create", &format!("{name}-{role}"), "--role", role, "--permission", "payloads"]);
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
    // The receiver has it (its ACK came back); the engine records it as sent a moment later.
    assert!(
        eventually(async || get(&format!("/messages/{copy}")).await["status"] == "sent").await,
        "the copy is recorded as sent"
    );
    let made = get(&format!("/messages/{copy}")).await;
    assert_eq!((&made["copy_of"], &made["control_id"]), (&c2.into(), &"C2-FIX".into()));
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

    // Dismissing: viewers can't, a reason is required, and the quarantine empties (the archive had
    // sent both, so the messages count as sent). Each request is one row in the audit log.
    let (c4, c5) = (ids[0], ids[1]);
    assert_eq!(post(&format!("/messages/{c4}/dismiss"), &viewer, serde_json::json!({"reason": "x"})).await.0, 403);
    let (status, body) = post(&format!("/messages/{c4}/dismiss"), &ops, serde_json::json!({"reason": "  "})).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("say why"), "{body}");
    assert_eq!(quarantined().await.len(), 2, "nothing was dismissed without a reason");
    let (status, body) =
        post(&format!("/messages/{c4}/dismiss"), &ops, serde_json::json!({"reason": "lab retired the unit"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dismissed"][0]["destinations"][0]["destination"], "lab");
    let (status, body) =
        post("/messages/dismiss", &ops, serde_json::json!({"ids": [c5, c1], "reason": "same unit"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!((body["dismissed"].as_array().unwrap().len(), &body["skipped"]), (1, &serde_json::json!([c1])));
    assert!(eventually(async || quarantined().await.is_empty()).await, "dismissed deliveries leave the quarantine");
    let dismissed = get(&format!("/messages/{c4}")).await;
    assert_eq!(dismissed["status"], "sent", "{dismissed}");
    let lab = dismissed["deliveries"].as_array().unwrap().iter().find(|d| d["destination"] == "lab").unwrap();
    assert_eq!(
        (&lab["status"], &lab["last_error"]),
        (&"filtered".into(), &format!("dismissed by {name}-operator: lab retired the unit").into())
    );
    let action = dismissed["actions"].as_array().unwrap().iter().find(|a| a["action"] == "dismiss").unwrap();
    assert_eq!(
        (&action["previous_status"], &action["reason"]),
        (&"quarantined".into(), &"lab retired the unit".into())
    );
    let (status, body) = post(&format!("/messages/{c4}/dismiss"), &ops, serde_json::json!({"reason": "again"})).await;
    assert_eq!(status, 409, "nothing is quarantined any more: {body}");

    // The audit log: only with the `audit` permission; it says who, how and from where.
    let auditor = {
        let (ok, out) =
            run(&["token", "create", &format!("{name}-auditor"), "--role", "viewer", "--permission", "audit"]);
        assert!(ok, "{out}");
        out.lines().next().unwrap().to_string()
    };
    let get_as = async |token: &str, path: &str| -> (u16, serde_json::Value) {
        let r = http.get(format!("{base}{path}")).bearer_auth(token).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    };
    assert_eq!(get_as(&viewer, "/audit").await.0, 403, "payloads isn't audit");
    assert_eq!(get_as(&ops, "/audit").await.0, 403, "nor is a role below admin");
    let (status, page) = get_as(&auditor, &format!("/audit?interface={name}&action=message.dismiss")).await;
    assert_eq!(status, 200, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "one row per request, newest first: {page}");
    let (bulk, single) = (&entries[0], &entries[1]);
    assert_eq!(
        (&bulk["actor"], &bulk["actor_kind"], &bulk["via"], &bulk["ip"], &bulk["message"]),
        (&format!("{name}-operator").into(), &"token".into(), &"token".into(), &"127.0.0.1".into(), &c5.into())
    );
    assert_eq!(
        (&bulk["detail"]["reason"], &bulk["detail"]["messages"]),
        (&"same unit".into(), &serde_json::json!([c5]))
    );
    assert_eq!((&single["message"], &single["detail"]["reason"]), (&c4.into(), &"lab retired the unit".into()));
    assert_eq!(single["detail"]["destinations"], serde_json::json!([format!("{name}/lab")]));

    // Reads of a payload are rows too, one per reader and message within the window (the viewer
    // looked at C2 many times above).
    let (_, reads) =
        get_as(&auditor, &format!("/audit?interface={name}&action=message.payload.read&message={c2}")).await;
    let reads = reads["entries"].as_array().unwrap();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(reads[0]["actor"], format!("{name}-viewer"));
    // The engine's own start is in it, with the version.
    let (_, started) = get_as(&auditor, "/audit?action=engine.start&limit=1").await;
    let start = &started["entries"][0];
    assert_eq!((&start["actor_kind"], &start["via"]), (&"system".into(), &"engine".into()), "{started}");
    assert!(start["detail"]["instance"].is_string() && start["detail"]["version"].is_string(), "{start}");
    assert!(["first", "stopped", "unclean"].contains(&start["detail"]["previous"].as_str().unwrap()), "{start}");
    // An export is oldest first, one JSON object per line, and logged.
    let export = http.get(format!("{base}/audit?format=ndjson&interface={name}")).bearer_auth(&auditor);
    let export = export.send().await.unwrap();
    assert_eq!(export.headers()["content-type"], "application/x-ndjson");
    let lines: Vec<serde_json::Value> =
        export.text().await.unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines.len() >= 5 && lines.windows(2).all(|w| w[0]["id"].as_i64() < w[1]["id"].as_i64()), "{lines:?}");
    let (_, exports) = get_as(&auditor, "/audit?action=audit.export&limit=1").await;
    assert_eq!(exports["entries"][0]["actor"], format!("{name}-auditor"));
    // The same from the server's command line, without a token.
    let (ok, out) = run(&["audit", "export", "--interface", &name, "--action", "message.dismiss"]);
    assert!(ok, "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with('{')).count(), 2, "{out}");
    let (_, cli) = get_as(&auditor, "/audit?action=audit.export&limit=1").await;
    assert_eq!((&cli["entries"][0]["via"], &cli["entries"][0]["actor_kind"]), (&"cli".into(), &"system".into()));

    let spec: serde_json::Value = http.get(format!("{base}/openapi.json")).send().await.unwrap().json().await.unwrap();
    for path in [
        "/messages/{id}/resend",
        "/messages/resend",
        "/messages/{id}/edit",
        "/messages/{id}/dismiss",
        "/audit",
        "/quarantine",
    ] {
        assert!(spec["paths"][path].is_object(), "{path} missing from the OpenAPI document");
    }
    for role in ["operator", "viewer", "auditor"] {
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

/// People sign in with a username and password: wrong ones are refused the same way whoever
/// they name, too many lock the account until an admin unlocks it, a password an admin chose has
/// to be changed before anything else works, users make tokens that never go above them,
/// appearance is kept per user, and disabling a user ends their sessions.
#[tokio::test(flavor = "multi_thread")]
async fn people_sign_in_with_passwords_and_are_kept_in_check() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let api = free_port();
    // Declared before the engine, so a failed run drops it after the engine has stopped.
    let people = TestUsers::new(&db);
    let admin = people.name("test-user-cli");
    let dir = project(&[(
        "interfaces/people/interface.yaml",
        format!("name: people\nsource: {{ mllp: {{ host: 127.0.0.1, port: {} }} }}\ndestination: {{ file: {{ dir: out }} }}\n", free_port()).as_str(),
    )]);
    let root = dir.path().to_string_lossy().to_string();
    let run = |args: &[&str], stdin: Option<&str>| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(args)
            .env("DUCTUS_DATABASE_URL", &db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text.trim().to_string())
    };

    // The first admin is made on the server, with a password the policy accepts.
    let (ok, out) =
        run(&["user", "create", &admin, "--role", "admin", "--password-stdin", "--root", &root], Some("short\n"));
    assert!(!ok && out.contains("at least 12"), "{out}");
    let (ok, out) = run(
        &["user", "create", &admin, "--role", "admin", "--email", "a@example.org", "--password-stdin", "--root", &root],
        Some("tulip river lantern\n"),
    );
    assert!(ok, "{out}");
    let (ok, out) = run(
        &["user", "create", &admin.to_uppercase(), "--password-stdin", "--root", &root],
        Some("tulip river lantern\n"),
    );
    assert!(!ok && out.contains("already a user"), "names ignore case: {out}");
    let (ok, out) = run(&["user", "list"], None);
    assert!(ok && out.contains(&admin) && out.contains("admin"), "{out}");

    let _engine = Child(
        Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["run", "--root", &root, "--threads", "1"])
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

    let sign_in =
        |body: serde_json::Value| http.post(format!("{base}/session")).header("X-Ductus", "1").json(&body).send();
    let password = |user: &str, password: &str| sign_in(serde_json::json!({ "username": user, "password": password }));
    let cookie_of =
        |r: &reqwest::Response| r.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    let call = |method: reqwest::Method, cookie: &str, path: &str, body: Option<serde_json::Value>| {
        let mut r = http.request(method, format!("{base}{path}")).header("cookie", cookie).header("X-Ductus", "1");
        if let Some(b) = body {
            r = r.json(&b);
        }
        r.send()
    };
    use reqwest::Method as M;

    // Wrong passwords: the same answer for a real name and one nobody has.
    let wrong = password(&admin, "not the password").await.unwrap();
    assert_eq!(wrong.status(), 401);
    let wrong = wrong.text().await.unwrap();
    let nobody = password("nobody-has-this-name", "not the password").await.unwrap();
    assert_eq!(nobody.status(), 401);
    assert_eq!(nobody.text().await.unwrap(), wrong, "no hint which names exist");
    assert_eq!(sign_in(serde_json::json!({})).await.unwrap().status(), 400);

    let ok = password(&admin.to_uppercase(), "tulip river lantern").await.unwrap();
    assert_eq!(ok.status(), 200, "usernames ignore case");
    let cookie = cookie_of(&ok);
    let me: serde_json::Value = ok.json().await.unwrap();
    assert_eq!(
        (&me["kind"], &me["name"], &me["role"]),
        (&"user".into(), &admin.clone().into(), &"admin".into()),
        "{me}"
    );
    assert_eq!(me["permissions"], serde_json::json!(["payloads", "audit"]), "admins have both");
    assert_eq!(me["must_change_password"], false);

    // An admin makes a viewer with a temporary password; the viewer can do nothing until they change it.
    let viewer = people.name("test-user-cli-v");
    let bad = call(M::POST, &cookie, "/users", Some(serde_json::json!({"username": viewer, "password": "weak"})))
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let made: serde_json::Value = call(
        M::POST,
        &cookie,
        "/users",
        Some(serde_json::json!({"username": viewer, "display_name": "Vi Ewer", "email": "v@example.org", "password": "temporary pass phrase"})),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!((&made["role"], &made["must_change_password"]), (&"viewer".into(), &true.into()), "{made}");
    let viewer_id = made["id"].as_i64().unwrap();

    let signed = password(&viewer, "temporary pass phrase").await.unwrap();
    assert_eq!(signed.status(), 200);
    let vcookie = cookie_of(&signed);
    assert_eq!(signed.json::<serde_json::Value>().await.unwrap()["must_change_password"], true);
    let blocked = call(M::GET, &vcookie, "/interfaces", None).await.unwrap();
    assert_eq!(blocked.status(), 403);
    assert!(blocked.text().await.unwrap().contains("password_change_required"));
    assert_eq!(call(M::GET, &vcookie, "/session", None).await.unwrap().status(), 200, "who am I still works");
    let change = |current: &str, new: &str| {
        call(M::POST, &vcookie, "/me/password", Some(serde_json::json!({"current": current, "new": new})))
    };
    assert_eq!(change("wrong current", "a brand new phrase").await.unwrap().status(), 400);
    let weak = change("temporary pass phrase", "short").await.unwrap();
    assert_eq!(weak.status(), 400);
    assert!(weak.text().await.unwrap().contains("at least 12"));
    assert_eq!(change("temporary pass phrase", "a brand new phrase").await.unwrap().status(), 204);
    assert_eq!(call(M::GET, &vcookie, "/interfaces", None).await.unwrap().status(), 200, "now it works");

    // What a viewer may not do.
    assert_eq!(call(M::GET, &vcookie, "/users", None).await.unwrap().status(), 403);
    assert_eq!(
        call(M::POST, &vcookie, "/interfaces/people/destinations/default/pause", None).await.unwrap().status(),
        403
    );

    // Their own tokens never go above them.
    let too_high = call(M::POST, &vcookie, "/me/tokens", Some(serde_json::json!({"name": "ci", "role": "operator"})))
        .await
        .unwrap();
    assert_eq!(too_high.status(), 400);
    let no_perm =
        call(M::POST, &vcookie, "/me/tokens", Some(serde_json::json!({"name": "ci", "permissions": ["payloads"]})))
            .await
            .unwrap();
    assert_eq!(no_perm.status(), 400, "a viewer without payloads can't hand it to a token");
    let token: serde_json::Value = call(M::POST, &vcookie, "/me/tokens", Some(serde_json::json!({"name": "ci"})))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(token["token"]["name"], format!("{viewer}/ci"), "{token}");
    assert_eq!(token["token"]["owner"], viewer.as_str());
    let secret = token["secret"].as_str().unwrap().to_string();
    let with_token = http.get(format!("{base}/interfaces")).bearer_auth(&secret).send().await.unwrap();
    assert_eq!(with_token.status(), 200);

    // Appearance follows the user.
    let fresh: serde_json::Value = call(M::GET, &vcookie, "/me/settings", None).await.unwrap().json().await.unwrap();
    assert_eq!((&fresh["theme"], &fresh["saved"]), (&"system".into(), &false.into()), "{fresh}");
    let colors = serde_json::json!({"colors": {"accent": "#112233"}, "scheme": "dark"});
    let saved = call(M::PUT, &vcookie, "/me/settings", Some(serde_json::json!({"theme": "custom", "colors": colors})))
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(
        call(M::PUT, &vcookie, "/me/settings", Some(serde_json::json!({"theme": "neon"}))).await.unwrap().status(),
        400
    );
    let again: serde_json::Value =
        call(M::GET, &cookie_of(&password(&viewer, "a brand new phrase").await.unwrap()), "/me/settings", None)
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(
        (&again["theme"], &again["colors"], &again["saved"]),
        (&"custom".into(), &colors, &true.into()),
        "{again}"
    );
    // Tokens have no account to keep settings on.
    let tokens_me = http.get(format!("{base}/me/settings")).bearer_auth(&secret).send().await.unwrap();
    assert_eq!(tokens_me.status(), 403);

    // Too many wrong passwords lock the account, and an admin unlocks it.
    for _ in 0..5 {
        password(&viewer, "guess guess guess").await.unwrap();
    }
    let locked = password(&viewer, "a brand new phrase").await.unwrap();
    assert_eq!(locked.status(), 429, "even the right password waits");
    assert!(locked.text().await.unwrap().contains("try again in"));
    let listed: serde_json::Value = call(M::GET, &cookie, "/users", None).await.unwrap().json().await.unwrap();
    let row = listed.as_array().unwrap().iter().find(|u| u["username"] == viewer.as_str()).unwrap();
    assert!(row["locked_until"].is_string(), "{row}");
    assert_eq!(call(M::POST, &cookie, &format!("/users/{viewer_id}/unlock"), None).await.unwrap().status(), 204);
    let back = password(&viewer, "a brand new phrase").await.unwrap();
    assert_eq!(back.status(), 200);
    let vcookie = cookie_of(&back);

    // Disabling ends sessions and stops their tokens; they can't sign in; enabling brings them back.
    let off = call(M::PATCH, &cookie, &format!("/users/{viewer_id}"), Some(serde_json::json!({"disabled": true})))
        .await
        .unwrap();
    assert_eq!(off.status(), 200);
    assert_eq!(call(M::GET, &vcookie, "/interfaces", None).await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{base}/interfaces")).bearer_auth(&secret).send().await.unwrap().status(), 401);
    assert_eq!(password(&viewer, "a brand new phrase").await.unwrap().status(), 401);
    call(M::PATCH, &cookie, &format!("/users/{viewer_id}"), Some(serde_json::json!({"disabled": false})))
        .await
        .unwrap();
    assert_eq!(password(&viewer, "a brand new phrase").await.unwrap().status(), 200);

    // Sessions: list, and sign out everywhere else.
    let second = cookie_of(&password(&admin, "tulip river lantern").await.unwrap());
    let sessions: serde_json::Value = call(M::GET, &cookie, "/me/sessions", None).await.unwrap().json().await.unwrap();
    assert!(sessions.as_array().unwrap().len() >= 2, "{sessions}");
    assert_eq!(sessions.as_array().unwrap().iter().filter(|s| s["current"] == true).count(), 1);
    assert_eq!(call(M::DELETE, &cookie, "/me/sessions", None).await.unwrap().status(), 204);
    assert_eq!(call(M::GET, &second, "/interfaces", None).await.unwrap().status(), 401, "the other one ended");
    assert_eq!(call(M::GET, &cookie, "/interfaces", None).await.unwrap().status(), 200, "this one stays");

    // The audit log: admins read it (viewers don't), and it has all of the above.
    let audit = async |cookie: &str, query: &str| -> serde_json::Value {
        let r = call(M::GET, cookie, &format!("/audit?{query}&limit=100"), None).await.unwrap();
        assert_eq!(r.status(), 200, "{query}");
        r.json::<serde_json::Value>().await.unwrap()["entries"].clone()
    };
    let rows = |entries: &serde_json::Value| entries.as_array().unwrap().clone();
    let vcookie = cookie_of(&password(&viewer, "a brand new phrase").await.unwrap());
    assert_eq!(call(M::GET, &vcookie, "/audit", None).await.unwrap().status(), 403, "a viewer can't read the log");
    let failed = rows(&audit(&cookie, &format!("action=session.failed&actor={viewer}")).await);
    assert!(failed.len() >= 5, "{failed:?}");
    assert!(failed.iter().any(|e| e["detail"]["locked"] == true), "the fifth wrong password locked it");
    assert!(failed.iter().all(|e| e["outcome"] == "failed" && e["via"] == "session" && e["ip"] == "127.0.0.1"));
    let everything = audit(&cookie, "action=session").await;
    let anonymous: Vec<_> = rows(&everything).into_iter().filter(|e| e["actor_kind"] == "anonymous").collect();
    assert!(!anonymous.is_empty() && anonymous.iter().all(|e| e["actor"] == "(unknown username)"));
    assert!(!everything.to_string().contains("nobody-has-this-name"), "what a stranger typed isn't kept");
    let made = rows(&audit(&cookie, "action=user.create").await);
    let first = made.iter().find(|e| e["subject"] == admin.as_str()).expect("the first admin");
    assert_eq!((&first["via"], &first["actor_kind"]), (&"cli".into(), &"system".into()), "made at the command line");
    let viewer_made = made.iter().find(|e| e["subject"] == viewer.as_str()).expect("the viewer");
    assert_eq!((&viewer_made["actor"], &viewer_made["via"]), (&admin.clone().into(), &"session".into()));
    for action in ["user.disable", "user.unlock", "user.update"] {
        let found = rows(&audit(&cookie, &format!("action={action}")).await);
        assert!(found.iter().any(|e| e["subject"] == viewer.as_str() && e["actor"] == admin.as_str()), "{action}");
    }
    let tokens = rows(&audit(&cookie, "action=token.create").await);
    assert!(tokens.iter().any(|e| e["subject"] == format!("{viewer}/ci") && e["actor"] == viewer.as_str()));
    let passwords = rows(&audit(&cookie, &format!("actor={viewer}&action=password")).await);
    assert!(passwords.iter().any(|e| e["action"] == "password.change"), "{passwords:?}");
    // Everyone sees their own activity, and only that.
    let mine: serde_json::Value = call(M::GET, &vcookie, "/me/activity", None).await.unwrap().json().await.unwrap();
    let mine = rows(&mine["entries"]);
    assert!(mine.iter().any(|e| e["action"] == "password.change"));
    assert!(mine.iter().all(|e| e["actor"] == viewer.as_str() || e["actor"].as_str().unwrap().starts_with(&viewer)));
    assert!(mine.iter().any(|e| e["action"] == "token.create"), "their token's creation is theirs");

    // A token can't be an account, and `auth.token_sign_in` isn't touched here.
    let bearer_me = http.get(format!("{base}/me")).bearer_auth(&secret).send().await.unwrap();
    assert!(bearer_me.status().is_client_error());
    drop(_engine);
    people.forget().await.unwrap();
}

const DOCS_SCHEMA: &str = r#"
fields:
  data_classification: { type: enum, values: [PHI, Internal, Public], required: true, label: Data classification }
  baa_signed: { type: enum, values: ["yes", "no", n/a], required: true, applies_to: [interface, resource] }
  go_live: { type: date }
  runbook: { type: url, applies_to: [interface, destination] }
"#;

/// `ductus validate` reads `alerts:` with each environment's values and says what is wrong or
/// won't reach anyone; `ductus alerts test` sends a test notification without an engine (here to
/// `ductus sink --http`).
#[test]
fn validate_checks_alerts_and_a_channel_can_be_tested() {
    let dir = hospital();
    let root = dir.path();
    let port = free_port();
    std::fs::create_dir_all(root.join("environments")).unwrap();
    std::fs::write(root.join("environments/dev.yaml"), "variables: {}\n").unwrap();
    std::fs::write(root.join("environments/prod.yaml"), format!("variables:\n  HOOK: http://127.0.0.1:{port}/chat\n"))
        .unwrap();
    let settings = |smtp: &str| {
        format!(
            "alerts:\n{smtp}  channels:\n    chat: {{ webhook: {{ url: \"${{HOOK}}\", format: teams }} }}\n  rules:\n    - {{ when: [error, stalled], severity: critical, to: [chat, owner] }}\nowners:\n  Lab team: {{ email: lab@hospital.example }}\n"
        )
    };
    std::fs::write(root.join("ductus.yaml"), settings("")).unwrap();
    let (ok, out) = ductus(root, &["validate", "--env", "prod"]);
    assert!(!ok && out.contains("✗ in ductus.yaml: alerts: email channels and `owner` need `smtp:`"), "{out}");

    std::fs::write(
        root.join("ductus.yaml"),
        settings("  smtp: { host: smtp.hospital.example, from: ductus@hospital.example }\n"),
    )
    .unwrap();
    let (ok, out) = ductus(root, &["validate", "--env", "dev"]);
    assert!(!ok && out.contains("missing in environment `dev`: HOOK"), "{out}");
    let (ok, out) = ductus(root, &["validate", "--env", "prod"]);
    assert!(ok, "{out}");
    assert!(out.contains("✓ alerts: 1 channel(s), 1 rule(s)"), "{out}");
    assert!(out.contains("⚠ alerts: no rule names behind, stopped, silent, missed"), "{out}");
    // Without a database it can only say the text isn't a team or an address; with one (check.sh
    // and CI set it) it's matched to the accounts, as `ductus owners` does.
    let said = if std::env::var("DUCTUS_DATABASE_URL").is_ok_and(|u| !u.trim().is_empty()) {
        "⚠ owner `Integration team`: not an account, a team in `owners:` or an email address: write their email"
    } else {
        "⚠ owner `Integration team`: not in `owners:`"
    };
    assert!(out.contains(said), "{out}");
    assert!(!out.contains("owner `Lab team`"), "it is in `owners:`: {out}");

    edit(&root.join("interfaces/results/interface.yaml"), "source:", "alert: { to: [pager] }\nsource:");
    let (ok, out) = ductus(root, &["validate", "--env", "prod"]);
    assert!(!ok && out.contains("✗ results: alert.to names `pager`, but alerts.channels has no such channel"), "{out}");

    // A test notification, straight from ductus.yaml.
    let requests = root.join("requests");
    let sink = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["sink", "--http", &port.to_string(), "--dir"])
        .arg(&requests)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _sink = Child(sink);
    let (ok, out) = ductus(root, &["alerts", "test", "nope", "--env", "prod"]);
    assert!(!ok && out.contains("no channel named `nope` (defined: chat)"), "{out}");
    let mut sent = (false, String::new());
    for _ in 0..50 {
        sent = ductus(root, &["alerts", "test", "chat", "--env", "prod"]);
        if sent.0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100)); // the sink is starting
    }
    assert!(sent.0 && sent.1.contains("sent a test notification to `chat`"), "{}", sent.1);
    let request = std::fs::read_to_string(requests.join("request-1.txt")).unwrap();
    assert!(request.starts_with("POST /chat"), "{request}");
    assert!(request.contains("AdaptiveCard") && request.contains("[Ductus prod] test notification"), "{request}");
}

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

/// TLS on listeners: `ductus cert self-signed` makes a pair, validate reads it, a key that isn't
/// the certificate's stops the start before the database, and with a database the MLLP listener
/// takes only TLS and the API answers over HTTPS (with HSTS).
#[tokio::test(flavor = "multi_thread")]
async fn listeners_speak_tls_with_the_certificates_resources_yaml_names() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("tls-test-{nanos}");
    let (mllp, http, api) = (free_port(), free_port(), free_port());
    let config = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp}, tls: {{ certificate: engine }} }} }}\ndestination: {{ file: {{ dir: out }} }}\n"
    );
    let orders = format!(
        "name: {name}-orders\nsource: {{ http: {{ host: 127.0.0.1, port: {http}, path: /orders, tls: {{ certificate: engine }} }} }}\nformat: json\ndestination: {{ file: {{ dir: out }} }}\n"
    );
    let dir = project(&[
        (format!("interfaces/{name}/interface.yaml").as_str(), config.as_str()),
        (format!("interfaces/{name}-orders/interface.yaml").as_str(), orders.as_str()),
        (
            "resources.yaml",
            "certificates:\n  engine: { cert_file: tls/engine.pem, key_file: tls/engine.key, owner: Infrastructure }\n",
        ),
        ("ductus.yaml", "api: { tls: { certificate: engine } }\n"),
    ]);
    let root = dir.path();
    let (ok, out) = ductus(root, &["cert", "self-signed", "tls/engine", "--name", "localhost", "--name", "127.0.0.1"]);
    assert!(ok && out.contains("cert_file: tls/engine.pem"), "{out}");
    let (ok, out) = ductus(root, &["cert", "self-signed", "tls/engine", "--name", "localhost"]);
    assert!(!ok && out.contains("exists already"), "never overwrites: {out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("tls/engine.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the key is readable by its owner only");
    }

    let (ok, out) = without_a_database(root, &["validate"]);
    assert!(ok && out.contains("✓ certificate `engine`: CN=localhost (localhost, 127.0.0.1), expires"), "{out}");
    edit(&root.join("ductus.yaml"), "certificate: engine", "certificate: nope");
    let (ok, out) = without_a_database(root, &["validate"]);
    assert!(!ok && out.contains("✗ api (ductus.yaml): tls.certificate: no certificate named `nope`"), "{out}");
    edit(&root.join("ductus.yaml"), "certificate: nope", "certificate: engine");

    // Another pair's key: refused at start, before the database (this one doesn't exist).
    let (_, _) = ductus(root, &["cert", "self-signed", "other", "--name", "localhost"]);
    std::fs::rename(root.join("tls/engine.key"), root.join("engine.key.good")).unwrap();
    std::fs::copy(root.join("other.key"), root.join("tls/engine.key")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["run", "--root", &root.to_string_lossy(), "--api-listen", "off"])
        .env("DUCTUS_DATABASE_URL", "postgres://nobody@127.0.0.1:1/none")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("certificate `engine` (resources.yaml)"), "{text}");
    assert!(text.contains("the private key isn't this certificate's key"), "{text}");
    std::fs::rename(root.join("engine.key.good"), root.join("tls/engine.key")).unwrap();

    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let (_engine, log) = start_engine(root, &db, Some(api), "engine.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    let pem = std::fs::read(root.join("tls/engine.pem")).unwrap();

    // MLLP over TLS: stored and ACKed.
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::pem::PemObject::pem_slice_iter(&pem) {
        roots.add(cert.unwrap()).unwrap();
    }
    let tls =
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", mllp)).await.unwrap();
    let server = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls)).connect(server, tcp).await.unwrap();
    let ack = mllp_exchange(&mut stream, "MSH|^~\\&|A|B|C|D|20261020||ADT^A01|TLS1|P|2.5\rPID|1||123\r").await;
    assert!(ack.contains("MSA|AA|TLS1"), "{ack}");

    // A plain sender on the TLS port gets no answer, and the log says why (once).
    let mut plain = tokio::net::TcpStream::connect(("127.0.0.1", mllp)).await.unwrap();
    let ack = mllp_exchange(&mut plain, "MSH|^~\\&|A|B|C|D|20261020||ADT^A01|PLAIN1|P|2.5\r").await;
    assert!(!ack.contains("MSA"), "{ack}");
    assert!(wait_for_log(&log, "isn't speaking TLS", 5), "{}", std::fs::read_to_string(&log).unwrap());

    // The API over HTTPS, with HSTS; plain HTTP isn't served.
    let https =
        reqwest::Client::builder().tls_certs_only([reqwest::Certificate::from_pem(&pem).unwrap()]).build().unwrap();
    let health = https.get(format!("https://localhost:{api}/api/v1/health")).send().await.unwrap();
    assert!(health.status().is_success());
    assert_eq!(health.headers()["strict-transport-security"], "max-age=31536000");
    assert!(reqwest::get(format!("http://127.0.0.1:{api}/api/v1/health")).await.is_err());

    // An HTTPS source in queue mode stores what it's sent.
    let posted = https.post(format!("https://localhost:{http}/orders")).body(r#"{"order": 1}"#).send().await.unwrap();
    assert_eq!(posted.status(), reqwest::StatusCode::ACCEPTED);
    assert!(reqwest::Client::new().post(format!("http://127.0.0.1:{http}/orders")).body("{}").send().await.is_err());
}

/// Outbound TLS through the engine: an MLLP destination sends over TLS, trusting the receiver's
/// certificate through `ca` and presenting a client certificate, to a listener that requires
/// one (the same engine's). Wrong names and kinds are refused by validate and at start.
#[tokio::test(flavor = "multi_thread")]
async fn mllp_destinations_send_over_tls_with_a_client_certificate() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("tls-out-{nanos}");
    let (inbound, relay) = (free_port(), free_port());
    let sender = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {inbound} }} }}\ndestination: {{ mllp: {{ host: localhost, port: {relay}, tls: {{ ca: engine-ca, certificate: client }} }} }}\n"
    );
    let receiver = format!(
        "name: {name}-relay\nsource: {{ mllp: {{ host: 127.0.0.1, port: {relay}, tls: {{ certificate: engine, clients: {{ ca: client-ca, subjects: ['CN=ductus-client'] }} }} }} }}\ndestination: {{ file: {{ dir: out }} }}\n"
    );
    let dir = project(&[
        (format!("interfaces/{name}/interface.yaml").as_str(), sender.as_str()),
        (format!("interfaces/{name}-relay/interface.yaml").as_str(), receiver.as_str()),
        (
            "resources.yaml",
            "certificates:\n  engine: { cert_file: tls/engine.pem, key_file: tls/engine.key }\n  engine-ca: { ca_file: tls/engine.pem }\n  client: { cert_file: tls/client.pem, key_file: tls/client.key }\n  client-ca: { ca_file: tls/client.pem }\n",
        ),
    ]);
    let root = dir.path();
    for (file, cn) in [("tls/engine", "localhost"), ("tls/client", "ductus-client")] {
        let (ok, out) = ductus(root, &["cert", "self-signed", file, "--name", cn]);
        assert!(ok, "{out}");
    }
    let (ok, out) = without_a_database(root, &["validate"]);
    assert!(ok, "{out}");

    // A CA where the client certificate belongs, a pair where the CA belongs, a name that isn't there.
    let iface = root.join(format!("interfaces/{name}/interface.yaml"));
    edit(&iface, "ca: engine-ca, certificate: client", "ca: client, certificate: engine-ca");
    let (ok, out) = without_a_database(root, &["validate"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains(&format!(
            "✗ interface `{name}` destination `default`: ca: `client` is a certificate with its key"
        )),
        "{out}"
    );
    assert!(out.contains("certificate: `engine-ca` is a CA; a client certificate needs its key"), "{out}");
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["run", "--root", &root.to_string_lossy(), "--api-listen", "off"])
        .env("DUCTUS_DATABASE_URL", "postgres://nobody@127.0.0.1:1/none")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && text.contains("ca: `client` is a certificate with its key"),
        "refused before the database: {text}"
    );
    edit(&iface, "ca: client, certificate: engine-ca", "ca: nope, certificate: client");
    let (ok, out) = without_a_database(root, &["validate"]);
    assert!(!ok && out.contains("ca: no certificate named `nope` in resources.yaml"), "{out}");
    edit(&iface, "ca: nope, certificate: client", "ca: engine-ca, certificate: client");

    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let (_engine, log) = start_engine(root, &db, None, "engine.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", inbound)).await.unwrap();
    let ack = mllp_exchange(&mut tcp, "MSH|^~\\&|A|B|C|D|20261020||ADT^A01|OUT1|P|2.5\rPID|1||123\r").await;
    assert!(ack.contains("MSA|AA|OUT1"), "{ack}");
    let out = root.join(format!("interfaces/{name}-relay/out"));
    let arrived = || std::fs::read_dir(&out).map(|d| d.count()).unwrap_or(0) >= 1;
    let started = std::time::Instant::now();
    while !arrived() && started.elapsed() < std::time::Duration::from_secs(20) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(arrived(), "relayed over TLS: {}", std::fs::read_to_string(&log).unwrap());
}

/// `ductus cert request` writes a private key and a CSR and never overwrites them; `cert show`
/// judges a file or a resources.yaml entry and exits 1 when the engine would refuse the pair.
#[test]
fn cert_request_writes_a_key_and_a_request_and_show_judges_what_comes_back() {
    let dir = project(&[(
        "resources.yaml",
        "certificates:\n  engine: { cert_file: tls/engine.pem, key_file: tls/engine.key }\n  lab-ca: { ca_file: tls/engine.pem }\n",
    )]);
    let root = dir.path();
    let (ok, out) = ductus(root, &["cert", "request", "tls/mllp", "--dns", "mllp.hospital.org", "--ip", "10.1.2.3"]);
    assert!(ok, "{out}");
    assert!(out.contains("Send tls/mllp.csr to the hospital's CA"), "{out}");
    assert!(out.contains("a certificate for mllp.hospital.org, 10.1.2.3"), "{out}");
    assert!(out.contains("Never send tls/mllp.key"), "{out}");
    assert!(out.contains("cert_file: tls/mllp.pem\n      key_file: tls/mllp.key"), "{out}");
    let csr = std::fs::read_to_string(root.join("tls/mllp.csr")).unwrap();
    assert!(csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"), "{csr}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("tls/mllp.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the key is readable by its owner only");
    }
    let key = std::fs::read(root.join("tls/mllp.key")).unwrap();
    let (ok, out) = ductus(root, &["cert", "request", "tls/mllp", "--dns", "other"]);
    assert!(!ok && out.contains("tls/mllp.key exists already"), "never overwrites: {out}");
    assert_eq!(std::fs::read(root.join("tls/mllp.key")).unwrap(), key, "the key is as it was");
    let (ok, out) = ductus(root, &["cert", "request", "tls/x", "--dns", "10.0.0.1"]);
    assert!(!ok && out.contains("--dns 10.0.0.1 is an IP address: give it as --ip 10.0.0.1"), "{out}");

    // What comes back, judged: here a self-signed pair stands in for the CA's answer.
    let (ok, out) = ductus(root, &["cert", "self-signed", "tls/engine", "--name", "localhost"]);
    assert!(ok, "{out}");
    let (ok, out) = ductus(root, &["cert", "show", "tls/engine.pem"]);
    assert!(ok, "{out}");
    assert!(out.contains("subject  CN=localhost"), "{out}");
    assert!(out.contains("chain    self-signed: senders must trust this very certificate"), "{out}");
    assert!(out.contains("key      matches (tls/engine.key)"), "the .key beside it: {out}");
    let (ok, out) = ductus(root, &["cert", "show", "engine"]);
    assert!(ok && out.starts_with("✓ certificate `engine` (resources.yaml)"), "{out}");
    let (ok, out) = ductus(root, &["cert", "show", "lab-ca"]);
    assert!(ok && out.contains("a CA"), "{out}");
    let (ok, out) = ductus(root, &["cert", "show", "tls/engine.pem", "--key", "tls/mllp.key"]);
    assert!(!ok && out.contains("✗ key      the private key isn't this certificate's key (tls/mllp.key)"), "{out}");
    let (ok, out) = ductus(root, &["cert", "show", "tls/mllp.csr"]);
    assert!(!ok && out.contains("a certificate signing request (CSR), not a certificate"), "{out}");
    let (ok, out) = ductus(root, &["cert", "show", "nope"]);
    assert!(!ok && out.contains("no `certificates:` entry by that name in resources.yaml (engine, lab-ca)"), "{out}");
}

/// `ductus sink --tls` takes MLLP over TLS only, asking for a client certificate its CA issued
/// with an allowed subject; `ductus gen --tls` trusts `--ca` and presents `--cert`. A sender
/// without a certificate is told why, and nothing it sends arrives.
#[tokio::test(flavor = "multi_thread")]
async fn gen_and_sink_speak_tls_with_client_certificates() {
    let dir = project(&[]);
    let root = dir.path();
    // 127.0.0.1, not localhost: in WSL localhost is only ::1, and the sink listens on IPv4.
    for (file, name) in [("sink", "127.0.0.1"), ("sender", "sender.hospital.org"), ("stranger", "stranger")] {
        let (ok, out) = ductus(root, &["cert", "self-signed", file, "--name", name]);
        assert!(ok, "{out}");
    }
    let port = free_port().to_string();
    let log = root.join("sink.log");
    let file = std::fs::File::create(&log).unwrap();
    let sink = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["sink", "--port", &port, "--tls", "--cert", "sink.pem", "--key", "sink.key"])
        .args(["--clients-ca", "sender.pem", "--subject", "CN=sender.hospital.org"])
        .current_dir(root)
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();
    let _sink = Child(sink);
    assert!(wait_for_log(&log, "MLLP over TLS, senders need a certificate from sender.pem", 10));
    let to = format!("127.0.0.1:{port}");
    let (ok, out) = ductus(
        root,
        &[
            "gen",
            "--to",
            &to,
            "--count",
            "20",
            "--patients",
            "4",
            "--ca",
            "sink.pem",
            "--cert",
            "sender.pem",
            "--key",
            "sender.key",
        ],
    );
    assert!(ok && out.contains("sent 20 messages over 1 connection(s)"), "{out}");
    assert!(wait_for_log(&log, "received 20 | unique 20 | duplicates 0 | out of order 0", 10));

    // Refused senders retry forever: each runs a moment and says why.
    let refused = |args: &[&str]| {
        let child = Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["gen", "--to", &to, "--count", "1"])
            .args(args)
            .current_dir(root)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let mut child = child;
        child.kill().unwrap();
        String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).into_owned()
    };
    let out = refused(&["--ca", "sink.pem"]);
    assert!(out.contains("TLS: the receiver wants a client certificate (name one in --cert)"), "{out}");
    let out = refused(&["--tls"]);
    assert!(out.contains("isn't from a CA we trust (name its CA in --ca"), "{out}");
    let out = refused(&["--ca", "sink.pem", "--cert", "stranger.pem", "--key", "stranger.key"]);
    assert!(out.contains("the receiver refused our client certificate"), "{out}");
    assert!(wait_for_log(&log, "reason=the sender presented no client certificate", 5));
    std::thread::sleep(std::time::Duration::from_millis(5500));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains("received 21"), "nothing refused arrived: {text}");
}

/// Send one MLLP frame and read until the end of a frame or the connection closes.
async fn mllp_exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    message: &str,
) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut frame = vec![0x0b];
    frame.extend_from_slice(message.as_bytes());
    frame.extend_from_slice(&[0x1c, 0x0d]);
    if stream.write_all(&frame).await.is_err() {
        return String::new();
    }
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    let read = async {
        while !got.ends_with(&[0x1c, 0x0d]) {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), read).await;
    String::from_utf8_lossy(&got).into_owned()
}

/// `ductus run` on `dir` with its log in a file next to the project (the API only if `api`).
fn start_engine(dir: &Path, db: &str, api: Option<u16>, log: &str) -> (Child, std::path::PathBuf) {
    let path = dir.join(log);
    let file = std::fs::File::create(&path).unwrap();
    let api = api.map_or("off".to_string(), |p| format!("127.0.0.1:{p}"));
    let child = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["run", "--root", &dir.to_string_lossy(), "--threads", "1", "--api-listen", &api])
        .env("DUCTUS_DATABASE_URL", db)
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();
    (Child(child), path)
}

/// Wait up to `seconds` for `text` in a log file.
fn wait_for_log(path: &Path, text: &str, seconds: u64) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < until {
        if std::fs::read_to_string(path).unwrap_or_default().contains(text) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// `ductus run` to its end; returns its exit code and output.
fn run_to_end(dir: &Path, db: &str) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["run", "--root", &dir.to_string_lossy(), "--threads", "1", "--api-listen", "off"])
        .env("DUCTUS_DATABASE_URL", db)
        .output()
        .unwrap();
    (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// One engine per interface across engines on one database: a second engine with the same
/// interface is refused (exit 3) and told whose it is; one with a different interface runs
/// alongside and sees the first; once the first is killed, the refused one starts at once.
#[tokio::test(flavor = "multi_thread")]
async fn an_interface_runs_on_one_engine_at_a_time() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let (name, other) = (format!("owner-test-{nanos}"), format!("owner-other-{nanos}"));
    let interface = |name: &str| {
        format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {} }} }}\ndestination: {{ file: {{ dir: out }} }}\n",
            free_port()
        )
    };
    let dir = project(&[(format!("interfaces/{name}/interface.yaml").as_str(), interface(&name).as_str())]);
    let second = project(&[(format!("interfaces/{other}/interface.yaml").as_str(), interface(&other).as_str())]);

    let api = free_port();
    let (mut first, log) = start_engine(dir.path(), &db, Some(api), "first.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    let pid = first.0.id();

    // The same project again (a DR server booted with production's config, say): refused.
    let (code, out) = run_to_end(dir.path(), &db);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("another engine is running one of this engine's interfaces on this database"), "{out}");
    assert!(out.contains(&name) && out.contains(&format!("pid {pid}")), "{out}");
    assert!(out.contains("Stop that engine first"), "{out}");

    // Different interfaces share the database; each engine sees the other.
    let (_neighbour, neighbour_log) = start_engine(second.path(), &db, None, "neighbour.log");
    assert!(
        wait_for_log(&neighbour_log, "started interfaces=", 60),
        "{}",
        std::fs::read_to_string(&neighbour_log).unwrap()
    );
    let token_name = format!("{name}-viewer");
    let _ = ductus(dir.path(), &["token", "revoke", &token_name]);
    let (ok, token) = ductus(dir.path(), &["token", "create", &token_name, "--role", "viewer"]);
    assert!(ok, "{token}");
    let token = token.lines().next().unwrap().to_string();
    let engine: serde_json::Value = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{api}/api/v1/engine"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(engine["holds_interfaces"], true);
    let others = engine["others"].as_array().unwrap();
    assert!(others.iter().any(|e| e["interfaces"] == serde_json::json!([other])), "{engine}");
    let _ = ductus(dir.path(), &["token", "revoke", &token_name]);

    // The first dies without a word (kill -9): its lock goes with it, so the next start takes over.
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let (_again, log) = start_engine(dir.path(), &db, None, "again.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
}

/// A directory that is down never delays a start (0.6.7.3: the first refresh round runs right after
/// the start, in the background). Here one that takes connections and never answers, the worse
/// kind: the start is as quick as one without `auth.ldap`, and the round gives up after the
/// directory's timeout, logged, while the engine runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_directory_that_is_down_never_delays_the_start() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // Connections land in its backlog; nothing ever answers them.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let make = |name: &str, ldap: bool| {
        // An owner email that reaches nobody: the round has someone to look up, so it connects.
        let interface = format!(
            "name: {name}\nabout: {{ owner: owner-{nanos}@example.org }}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {} }} }}\ndestination: {{ file: {{ dir: out }} }}\n",
            free_port()
        );
        let mut files = vec![(format!("interfaces/{name}/interface.yaml"), interface)];
        if ldap {
            files.push((
                "ductus.yaml".into(),
                "auth:\n  ldap:\n    resource: corp\n    user_base: ou=people,dc=example,dc=org\n    user_filter: (uid={username})\n    groups: { viewer: 'cn=v,ou=groups,dc=example,dc=org' }\n".into(),
            ));
            files.push((
                "resources.yaml".into(),
                format!("ldap:\n  corp: {{ url: 'ldap://127.0.0.1:{port}', insecure: true, timeout: 8s }}\n"),
            ));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
        project(&files)
    };
    let without = make(&format!("dir-down-plain-{nanos}"), false);
    let with = make(&format!("dir-down-ldap-{nanos}"), true);
    let start = |dir: &Path, log: &str| {
        let t = std::time::Instant::now();
        let (child, path) = start_engine(dir, &db, None, log);
        assert!(wait_for_log(&path, "started interfaces=", 60), "{}", std::fs::read_to_string(&path).unwrap());
        (child, path, t.elapsed())
    };
    let (_plain, _, plain) = start(without.path(), "plain.log");
    let (_down, log, down) = start(with.path(), "down.log");
    println!("started in {plain:?} without auth.ldap, {down:?} with the directory down");
    assert!(down < std::time::Duration::from_secs(5), "the start waited for the directory: {down:?}");
    assert!(down < plain + std::time::Duration::from_secs(2), "{down:?} vs {plain:?}");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains("couldn't reach the directory"), "the round is still waiting for it: {text}");
    assert!(wait_for_log(&log, "couldn't reach the directory", 30), "{}", std::fs::read_to_string(&log).unwrap());
    drop(silent);
}

/// The lanes guard: lowering `ordering.partitions` (or removing `ordering.key`) with messages
/// waiting in lanes it no longer has refuses the start, and `ductus validate` says so first;
/// keeping them starts; raising them with messages waiting starts with a warning.
#[tokio::test(flavor = "multi_thread")]
async fn lanes_that_would_strand_messages_refuse_the_start() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("lanes-test-{nanos}");
    let (mllp, receiver) = (free_port(), free_port()); // nothing listens on `receiver`: messages wait
    let config = |ordering: &str| {
        format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\n{ordering}destination: {{ mllp: {{ host: 127.0.0.1, port: {receiver}, timeout_ms: 200 }} }}\n"
        )
    };
    let file = format!("interfaces/{name}/interface.yaml");
    let dir = project(&[(file.as_str(), config("ordering: { key: PID-3.1, partitions: 64 }\n").as_str())]);
    let yaml = dir.path().join(&file);

    let store = ductus_store::Store::connect(&db, 2).await.unwrap();
    let ids = store.destination_ids(&[(name.clone(), "default".to_string())]).await.unwrap();
    let mut waiting = Vec::new();
    for lane in [5, 40, 63] {
        let m = ductus_store::NewMessage {
            interface: &name,
            partition_key: lane,
            ordering_key: Some("100001"),
            control_id: None,
            payload: b"MSH|^~\\&|waiting",
            relaxed: false,
            destinations: &ids,
            tier: 49,
        };
        waiting.push(store.insert(m).await.unwrap());
    }

    std::fs::write(&yaml, config("ordering: { key: PID-3.1, partitions: 32 }\n")).unwrap();
    let (code, out) = run_to_end(dir.path(), &db);
    assert_eq!(code, Some(3), "{out}");
    let line = format!("{name} → default: 2 messages waiting in lanes 40–63; {name} now has 32 lanes");
    assert!(out.contains(&line) && out.contains("ductus drain --wait"), "{out}");
    let (ok, out) = ductus(dir.path(), &["validate", "--database-url", &db]);
    assert!(!ok && out.contains(&format!("✗ {line}")) && out.contains("would refuse to start"), "{out}");
    let (_, out) = without_a_database(dir.path(), &["validate"]);
    assert!(out.contains("note: not checked against a database"), "{out}");

    std::fs::write(&yaml, config("")).unwrap();
    let (code, out) = run_to_end(dir.path(), &db);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains(&format!("{name} has no `ordering.key` now: 1 lane")) && out.contains("lanes 5–63"), "{out}");

    // As it was: starts, and the lanes are recorded.
    std::fs::write(&yaml, config("ordering: { key: PID-3.1, partitions: 64 }\n")).unwrap();
    let (mut engine, log) = start_engine(dir.path(), &db, None, "64.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    engine.0.kill().unwrap();
    engine.0.wait().unwrap();
    let (ok, out) = ductus(dir.path(), &["validate", "--database-url", &db]);
    assert!(ok && out.contains("✓ database: every interface's lanes reach the messages waiting for it"), "{out}");

    // More lanes with messages waiting: a warning, in the log and the audit log; it starts.
    std::fs::write(&yaml, config("ordering: { key: PID-3.1, partitions: 128 }\n")).unwrap();
    let (ok, out) = ductus(dir.path(), &["validate", "--database-url", &db]);
    assert!(ok && out.contains(&format!("⚠ {name}: lanes go from 64 to 128 with 3 message(s) waiting")), "{out}");
    let (mut engine, log) = start_engine(dir.path(), &db, None, "128.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("lanes raised with messages waiting") && text.contains("before=64 now=128"), "{text}");
    engine.0.kill().unwrap();
    engine.0.wait().unwrap();

    // Leave nothing pending behind.
    let destination = ids[0];
    let done: Vec<_> = waiting.iter().map(|m| ductus_store::Done::sent(*m, None)).collect();
    store.finish(destination, &done).await.unwrap();
}

/// Stops a process this test didn't start itself (an engine a handover started) when dropped.
#[cfg(target_os = "linux")]
struct Terminate(u32);

#[cfg(target_os = "linux")]
impl Terminate {
    fn alive(&self) -> bool {
        self.kill(&["-0"])
    }

    fn kill(&self, how: &[&str]) -> bool {
        let quiet = std::process::Stdio::null;
        let status = Command::new("kill").args(how).arg(self.0.to_string()).stderr(quiet()).status();
        status.is_ok_and(|s| s.success())
    }
}

#[cfg(target_os = "linux")]
impl Drop for Terminate {
    fn drop(&mut self) {
        self.kill(&[]);
        for _ in 0..100 {
            if !self.alive() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// The handover (`ductus handover`, which `systemctl reload` runs) under traffic: a new engine
/// takes over with no connection refused and every message delivered once, in order; `GET
/// /engine` says how. A project that can't start, and one lowering lanes, are refused: the
/// running engine goes on and still takes messages.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_handover_refuses_nothing_and_a_failed_one_leaves_the_engine_running() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("handover-test-{nanos}");
    let (mllp, receiver, api) = (free_port(), free_port(), free_port());
    let config = |lanes: u32| {
        format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {mllp} }} }}\nordering: {{ key: PID-3.1, partitions: {lanes} }}\ndestination: {{ mllp: {{ host: 127.0.0.1, port: {receiver} }} }}\n"
        )
    };
    let file = format!("interfaces/{name}/interface.yaml");
    let dir = project(&[(file.as_str(), config(8).as_str())]);
    let yaml = dir.path().join(&file);
    let control = dir.path().join("control");
    let control = control.to_str().unwrap();
    let bin = env!("CARGO_BIN_EXE_ductus");
    let output = |name: &str| std::fs::File::create(dir.path().join(name)).unwrap();
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap_or_default();

    let sink = Child(
        Command::new(bin)
            .args(["sink", "--port", &receiver.to_string()])
            .stdout(output("sink.log"))
            .stderr(output("sink.err"))
            .spawn()
            .unwrap(),
    );
    let log = dir.path().join("engine.log");
    let file_log = output("engine.log");
    let mut old = Child(
        Command::new(bin)
            .args(["run", "--root", &dir.path().to_string_lossy(), "--threads", "1"])
            .args(["--api-listen", &format!("127.0.0.1:{api}")])
            .env("DUCTUS_DATABASE_URL", &db)
            .env("DUCTUS_CONTROL", control)
            .stdout(file_log.try_clone().unwrap())
            .stderr(file_log)
            .spawn()
            .unwrap(),
    );
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", read("engine.log"));
    let old_pid = old.0.id();

    // 600 messages at 200/s over two connections; the handover a second in.
    let sender = Command::new(bin)
        .args(["gen", "--to", &format!("127.0.0.1:{mllp}"), "--count", "600", "--patients", "60"])
        .args(["--connections", "2", "--rate", "200"])
        .stdout(output("gen.log"))
        .stderr(output("gen.err"))
        .spawn()
        .unwrap();
    let mut sender = Child(sender);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let (ok, out) = ductus(dir.path(), &["handover", "--control", control]);
    assert!(ok, "{out}\n{}", read("engine.log"));
    assert!(out.contains("is ready after") && out.contains("receiving never stopped, sending paused"), "{out}");
    let exited = tokio::task::spawn_blocking(move || old.0.wait()).await.unwrap().unwrap();
    assert!(exited.success(), "the old engine stops cleanly: {exited}");

    let store = ductus_store::Store::connect(&db, 2).await.unwrap();
    let engines = store.live_engines().await.unwrap();
    let running = engines.iter().find(|e| e.interfaces.contains(&name)).expect("an engine runs it");
    let new = Terminate(running.pid as u32);
    assert_ne!(new.0, old_pid);

    sender.0.wait().unwrap();
    let sent = read("gen.log");
    assert!(sent.contains("rejected 0, refused 0,"), "{sent}");
    let mut drained = false;
    for _ in 0..200 {
        let waiting = store.waiting_by_interface(std::slice::from_ref(&name)).await.unwrap();
        if waiting.iter().all(|(_, n)| *n == 0) {
            drained = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(drained, "everything delivered");

    // How it took over, as the API tells.
    let token_name = format!("{name}-viewer");
    let (ok, token) = ductus(dir.path(), &["token", "create", &token_name, "--role", "viewer"]);
    assert!(ok, "{token}");
    let token = token.lines().next().unwrap().to_string();
    let engine: serde_json::Value = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{api}/api/v1/engine"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _ = ductus(dir.path(), &["token", "revoke", &token_name]);
    assert_eq!(engine["handover"]["from_pid"], old_pid, "{engine}");
    assert!(engine["handover"]["sending_paused_ms"].as_u64().is_some(), "{engine}");

    // A project that can't start: refused, and the running engine goes on.
    std::fs::write(&yaml, format!("{}bogus: true\n", config(8))).unwrap();
    let (ok, out) = ductus(dir.path(), &["handover", "--control", control]);
    assert!(!ok, "{out}");
    assert!(out.contains("the new engine can't take over") && out.contains("unknown field `bogus`"), "{out}");
    assert!(out.contains(&format!("This engine (pid {}", new.0)) && out.contains("goes on."), "{out}");
    // Lanes going down: a restart's to judge.
    std::fs::write(&yaml, config(4)).unwrap();
    let (ok, out) = ductus(dir.path(), &["handover", "--control", control]);
    assert!(!ok && out.contains("a handover can't lower an interface's lanes"), "{out}");
    assert!(out.contains(&format!("{name}: 8 → 4 lanes")) && out.contains("systemctl restart ductus"), "{out}");
    assert!(new.alive());
    let (ok, out) =
        ductus(dir.path(), &["gen", "--to", &format!("127.0.0.1:{mllp}"), "--count", "1", "--offset", "1000"]);
    assert!(ok && out.contains("sent 1 messages") && out.contains("refused 0,"), "still takes messages: {out}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    drop(new); // SIGTERM: a clean stop
    // Its summary line comes at Ctrl-C.
    let _ = Command::new("kill").args(["-INT", &sink.0.id().to_string()]).status();
    for _ in 0..50 {
        if read("sink.log").contains("final:") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let received = read("sink.log");
    assert!(received.contains("| duplicates 0 | out of order 0 |"), "{received}");
    assert!(received.contains("unique 601 |"), "every message once (and the last one): {received}");
}

/// `ductus upgrade --check` on a new database, on one a migration behind (the migration and what
/// it does are listed; ready), with a project whose test fails (exit 1, the failure shown, the same
/// in `--json`), and on one where an applied migration differs from this build's (exit 1).
#[tokio::test(flavor = "multi_thread")]
async fn upgrade_check_reads_the_database_and_the_project() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    ductus_store::Store::connect(&db, 1).await.unwrap(); // the shared database: this build's migrations
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("p");
    assert!(ductus(parent.path(), &["init", "p"]).0);
    let _ = std::fs::remove_dir_all(root.join(".ductus")); // the check must write nothing there
    let check = |url: &str, json: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ductus"));
        command.args(["upgrade", "--check"]).current_dir(&root).env("DUCTUS_DATABASE_URL", url);
        command.env_remove("DUCTUS_PROJECT").env_remove("DUCTUS_ENV");
        if json {
            command.arg("--json");
        }
        let out = command.output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let last = ductus_store::migrations::embedded().unwrap().pop().unwrap();
    let listed = format!("✓ {:04} {}", last.version, last.what);

    // A schema of this test's own holds the migrations table (the shared one stays as it is).
    let schema = ductus_store::testing::TestSchema::new(&db, "upgrade").await.unwrap();
    let (ok, out) = check(&schema.url_alone(), false);
    assert!(ok, "{out}");
    assert!(out.contains("✓ a new database: the first start creates every table"), "{out}");
    assert!(out.contains("note: engines before 0.6.2 aren't recorded"), "{out}");
    assert!(out.contains("✓ validate: valid") && out.contains("✓ test: 1 passed, 0 failed"), "{out}");

    schema
        .run(
            "CREATE TABLE _sqlx_migrations AS SELECT * FROM public._sqlx_migrations;
             DELETE FROM _sqlx_migrations WHERE version = (SELECT max(version) FROM _sqlx_migrations)",
        )
        .await
        .unwrap();
    let (ok, out) = check(&schema.url(), false);
    assert!(ok, "{out}");
    assert!(out.contains("✓ migrations: the first start applies 1 "), "{out}");
    assert!(out.contains(&listed), "{out}");
    assert!(out.contains("✓ required stops: none") && out.contains("✓ deprecated settings: none used"), "{out}");
    assert!(out.contains(&format!("ready to upgrade to {}", env!("CARGO_PKG_VERSION"))), "{out}");

    // The new version's SDK fails one of the project's tests.
    let broken = root.join("interfaces/adt-in/broken.test.ts");
    std::fs::write(
        &broken,
        "import { expect, test } from \"@ductus/sdk\";\n\ntest(\"counts as before\", () => {\n  expect(1 + 1).toBe(3);\n});\n",
    )
    .unwrap();
    let (ok, out) = check(&schema.url(), false);
    assert!(!ok, "{out}");
    assert!(out.contains("✗ test: 1 passed, 1 failed") && out.contains("✗ counts as before"), "{out}");
    assert!(out.contains("1 problem(s): fix them before upgrading"), "{out}");
    let (ok, out) = check(&schema.url(), true);
    assert!(!ok);
    let report: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!((report["ok"].as_bool(), report["problems"].as_u64()), (Some(false), Some(1)), "{out}");
    assert_eq!(report["migrations"]["pending"][0]["version"], last.version, "{out}");
    assert_eq!(report["project"]["test"]["ok"], false, "{out}");
    assert_eq!(report["project"]["validate"]["ok"], true, "{out}");
    std::fs::remove_file(&broken).unwrap();

    // An applied migration whose file this build carries differently: the start would refuse.
    schema.run("UPDATE _sqlx_migrations SET checksum = '\\x00'::bytea WHERE version = 1").await.unwrap();
    let (ok, out) = check(&schema.url(), false);
    assert!(!ok, "{out}");
    assert!(out.contains("✗ migration 0001 (messages) was applied from a different file"), "{out}");
    schema.forget().await.unwrap();
    assert!(!root.join(".ductus").exists(), "the check wrote into the project");
}

const REPLAYED: &str = r#"import { db, kill, type Message } from "@ductus/sdk";

export function transform(m: Message) {
  if (m.PID[3][1] === "SKIP") return kill("test patient");
  m.set("PID-8", "U");
  return m;
}

// The time and a random number: a replay pins the clock to the message's arrival and seeds the random.
export async function toBilling(m: Message) {
  return { mrn: m.PID[3][1], at: new Date().toISOString(), draw: Math.random() };
}
"#;

/// `ductus replay` over traffic an engine recorded: unchanged code is identical with the kept outputs
/// and the same decision where nothing kept them (`upgrade --check --replay` warns when that's all it
/// could compare); a changed field and a changed filter show against an older version of the
/// project (a folder, or a Git revision), field by field, with the clock and the random pinned on
/// both sides; a write isn't made and makes the message not comparable; the HTML and JSON reports.
#[tokio::test(flavor = "multi_thread")]
async fn replay_compares_a_change_with_the_record_kept_outputs_and_the_version_before() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("replay-test-{nanos}");
    let port = free_port();
    let yaml = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nscript: transform.ts\ndestinations:\n  lab: {{ file: {{ dir: out/lab }}, save_output: true }}\n  billing: {{ file: {{ dir: out/billing }}, transform: toBilling, output: json }}\n"
    );
    let interface = format!("interfaces/{name}");
    let dir = project(&[
        (format!("{interface}/interface.yaml").as_str(), yaml.as_str()),
        (format!("{interface}/transform.ts").as_str(), REPLAYED),
        (
            "resources.yaml",
            "databases:\n  billing: { postgres: { host: 127.0.0.1, port: 1, database: b, user: b, password: b } }\n",
        ),
    ]);
    let root = dir.path().to_string_lossy().to_string();

    let (_engine, log) = start_engine(dir.path(), &db, None, "engine.log");
    assert!(wait_for_log(&log, "started interfaces=", 60), "{}", std::fs::read_to_string(&log).unwrap());
    let mut sender = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    for (i, mrn) in ["1", "2", "3", "SKIP"].iter().enumerate() {
        let message =
            format!("MSH|^~\\&|EPIC|HOSP|LAB|HOSP|20261029120000||ADT^A01|{i}|P|2.5\rPID|1||{mrn}^^^MRN||DOE^JOHN\r");
        let ack = mllp_exchange(&mut sender, &message).await;
        assert!(ack.contains("MSA|AA"), "{ack}");
    }
    let store = ductus_store::Store::connect_read_only(&db).await.unwrap();
    let done = eventually(async || {
        let counts = store.counts().await.unwrap();
        let mine: Vec<_> = counts.iter().filter(|(i, ..)| *i == name).collect();
        mine.iter().map(|(_, _, n)| n).sum::<i64>() == 4
            && mine.iter().all(|(_, s, _)| *s != ductus_store::Status::Pending)
    });
    assert!(done.await, "{}", std::fs::read_to_string(&log).unwrap());

    let replay = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
            .args(["replay", "--root", &root, "--since", "1h"])
            .args(args)
            .env("DUCTUS_DATABASE_URL", &db)
            .env_remove("DUCTUS_PROJECT")
            .env_remove("DUCTUS_ENV")
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // The code as it ran: the record and the kept outputs say the same. Billing keeps no output, so
    // only its decisions are known to match.
    let (code, out, err) = replay(&["--against", "stored"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("note: outputs compared with kept outputs only (--against stored)"), "{out}");
    assert!(
        out.contains("✓ lab: 4 compared, 4 identical")
            && out.contains("✓ billing: 4 compared, 4 same decision (outputs not compared)"),
        "{out}"
    );
    assert!(!out.contains("⚠ outputs not compared"), "lab's outputs were compared: {out}");

    // `upgrade --check --replay` over a project whose destinations keep no outputs: without an
    // installed version beside it, it passes with a ⚠ that says only decisions were compared, and how
    // to compare outputs.
    let billing_only = tempfile::tempdir().unwrap();
    let copy = billing_only.path().join(&interface);
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(dir.path().join(&interface).join("transform.ts"), copy.join("transform.ts")).unwrap();
    std::fs::copy(dir.path().join("resources.yaml"), billing_only.path().join("resources.yaml")).unwrap();
    std::fs::write(
        copy.join("interface.yaml"),
        format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nscript: transform.ts\ndestinations:\n  billing: {{ file: {{ dir: out/billing }}, transform: toBilling, output: json }}\n"
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ductus"))
        .args(["upgrade", "--check", "--replay", "1h", "--json", "--root"])
        .arg(billing_only.path())
        .env("DUCTUS_DATABASE_URL", &db)
        .env_remove("DUCTUS_PROJECT")
        .env_remove("DUCTUS_ENV")
        .output()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let check: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {said}"));
    let replayed = &check["replay"];
    let warning = check["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["text"].as_str().is_some_and(|t| t.starts_with("replay: outputs not compared")));
    if replayed["other"].is_null() {
        assert_eq!((replayed["identical"].as_u64(), replayed["same_decision"].as_u64()), (Some(0), Some(4)), "{said}");
        let warning = warning.unwrap_or_else(|| panic!("no ⚠ for outputs not compared: {said}"));
        assert_eq!(warning["level"], "warning", "{said}");
        assert!(warning["text"].as_str().unwrap().contains("--against PATH"), "{said}");
        let summary =
            check["checks"].as_array().unwrap().iter().find(|c| {
                c["text"].as_str().is_some_and(|t| t.starts_with("replay against the record and kept outputs"))
            });
        let summary = summary.unwrap_or_else(|| panic!("{said}"))["text"].as_str().unwrap();
        assert!(summary.ends_with("0 identical, 4 same decision (outputs not compared), 0 different"), "{summary}");
    } else {
        // An installed version ran beside it (a developer's machine with the package or MSI).
        assert!(warning.is_none(), "{said}");
    }
    assert!(err.contains(&format!("{name}: 4 message(s) replayed")), "{err}");

    // A new version of the code: PID-8 changes, MRN 2 is filtered now.
    let before = tempfile::tempdir().unwrap();
    for file in ["resources.yaml", &format!("{interface}/interface.yaml"), &format!("{interface}/transform.ts")] {
        let to = before.path().join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(dir.path().join(file), to).unwrap();
    }
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "as it ran"]);
    let code_file = dir.path().join(&interface).join("transform.ts");
    edit(&code_file, "m.set(\"PID-8\", \"U\")", "m.set(\"PID-8\", \"F\")");
    edit(&code_file, "=== \"SKIP\")", "=== \"SKIP\" || m.PID[3][1] === \"2\")");

    let page = dir.path().join("replay.html");
    let baseline = before.path().to_string_lossy().to_string();
    let (code, out, err) = replay(&["--baseline", &baseline, "--report", &page.to_string_lossy()]);
    assert_eq!(code, Some(1), "{out}{err}");
    let other = format!("the project at {baseline}");
    assert!(out.contains(&format!("ductus replay: version {} against {other}", env!("CARGO_PKG_VERSION"))), "{out}");
    assert!(out.contains("✗ lab: 4 compared, 1 identical, 3 different"), "{out}");
    assert!(out.contains(&format!("vs {other}: PID-8 differs in 2 message(s) (e.g. message")), "{out}");
    assert!(out.contains("\"U\" → \"F\""), "{out}");
    assert!(out.contains(&format!("vs {other}: decision differs in 1 message(s)")), "{out}");
    assert!(out.contains("\"sent\" → \"filtered (killed: test patient)\""), "{out}");
    assert!(out.contains("vs the kept output: PID-8 differs in 2"), "{out}");
    assert!(
        out.contains("✓ billing: 4 compared, 4 identical"),
        "the clock and the random were pinned on both sides: {out}"
    );
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("<h1>Replay report</h1>") && html.contains("sha256:"), "{html}");
    assert!(out.contains(&format!("report: {}", page.display())), "{out}");

    // The same against a Git revision of the project, as JSON.
    let (code, out, err) = replay(&["--baseline", "HEAD", "--json", "--hide-values"]);
    assert_eq!(code, Some(1), "{out}{err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}{err}"));
    assert_eq!(report["other"]["label"], "the project at HEAD", "{report:#}");
    let destinations = &report["interfaces"][0]["destinations"];
    let lab = destinations.as_array().unwrap().iter().find(|d| d["name"] == "lab").unwrap();
    assert_eq!((lab["identical"].as_u64(), lab["different"].as_u64()), (Some(1), Some(3)), "{report:#}");
    let pid8 = lab["differences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["against"] == "other" && g["what"] == "PID-8")
        .unwrap();
    assert_eq!((pid8["count"].as_u64(), pid8["examples"][0]["before"].as_str()), (Some(2), Some("")), "values hidden");
    assert!(report["checksum"].as_str().unwrap().starts_with("sha256:"));

    // Another binary on the same (changed) project: both versions agree, so what differs from the
    // record is drift, not this version's doing; it passes.
    let binary = before.path().join(if cfg!(windows) { "other-ductus.exe" } else { "other-ductus" });
    std::fs::copy(env!("CARGO_BIN_EXE_ductus"), &binary).unwrap();
    let (code, out, err) = replay(&["--against", &binary.to_string_lossy()]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(&format!("against version {} ({})", env!("CARGO_PKG_VERSION"), binary.display())), "{out}");
    assert!(out.contains("⚠ lab: 4 compared, 1 identical, 3 drift"), "{out}");
    assert!(out.contains("(drift: the other version does the same)"), "{out}");

    // A write isn't made in a replay: those messages can't be compared.
    edit(
        &code_file,
        "export async function toBilling(m: Message) {",
        "export async function toBilling(m: Message) {\n  await db(\"billing\").execute(\"INSERT INTO charges VALUES ($1)\", [m.PID[3][1]]);",
    );
    let (code, out, err) = replay(&["--against", "stored", "--interface", &name]);
    assert_eq!(code, Some(1), "lab still differs: {out}{err}");
    assert!(out.contains("⚠ billing: 4 compared, 4 not comparable"), "{out}");
    assert!(out.contains("writes to database `billing`: not made in a replay"), "{out}");
    let (code, out, _) = replay(&["--against", "stored", "--interface", "nope"]);
    assert_eq!(code, Some(1));
    assert!(out.is_empty());
}
