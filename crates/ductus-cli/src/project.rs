//! Project commands: `types`, `test`, `validate`, `ports`, `secret`.

use anyhow::{Context, Result};
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::Isolate;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::findings::{Finding, Report, Severity};
use ductus_runtime::project::{TYPES_FILE, code_references, files_under, key, resource_kind};
use ductus_script::errors::Root;

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2023"],
    "strict": true,
    "noEmit": true,
    "allowImportingTsExtensions": true,
    "skipLibCheck": true,
    "types": []
  },
  "include": ["interfaces/**/*.ts", ".ductus/types/*.d.ts"]
}
"#;

/// An interface folder, loaded or not.
pub(crate) struct InterfaceDir {
    pub(crate) dir: PathBuf,
    pub(crate) config: Result<InterfaceConfig>,
}

impl InterfaceDir {
    pub(crate) fn label(&self) -> String {
        match &self.config {
            Ok(c) => c.name.clone(),
            Err(_) => self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

pub(crate) fn interface_dirs(root: &Path, env: &Environment) -> Result<Vec<InterfaceDir>> {
    let base = root.join("interfaces");
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .with_context(|| format!("reading {}", base.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("interface.yaml").is_file())
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().map(|dir| InterfaceDir { config: InterfaceConfig::load_in(&dir, env), dir }).collect())
}

fn is_test(p: &Path) -> bool {
    p.to_string_lossy().ends_with(".test.ts")
}

fn relative(root: &Path, file: &str) -> String {
    let root = key(root);
    file.strip_prefix(&root).map(|r| r.trim_start_matches('/').to_string()).unwrap_or_else(|| file.to_string())
}

/// Write the SDK typings and (if missing) a tsconfig.json, so VS Code gets IntelliSense and hover
/// docs in interface code; and the configuration files' schemas, for the same in the YAML files.
pub async fn write_types(root: &Path) -> Result<String> {
    let types = Isolate::generate_types().await?;
    let path = root.join(TYPES_FILE);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, &types).with_context(|| format!("writing {}", path.display()))?;
    let tsconfig = root.join("tsconfig.json");
    if !tsconfig.exists() {
        std::fs::write(&tsconfig, TSCONFIG)?;
    }
    write_schemas(root)?;
    Ok(types)
}

pub async fn types(root: &Path) -> Result<()> {
    write_types(root).await?;
    println!("wrote {} (and tsconfig.json if it was missing)", root.join(TYPES_FILE).display());
    println!("wrote {} (the configuration files' schemas)", root.join(SCHEMA_DIR).display());
    match vscode_schemas(root)? {
        Mapped::Written | Mapped::Already => println!("{VSCODE} maps them to the YAML files"),
        Mapped::Elsewhere(keys) => {
            println!("{VSCODE} already maps {} to other schemas: left as it is", keys.join(", "))
        }
        Mapped::Unreadable => println!(
            "{VSCODE} isn't plain JSON (comments?), so it wasn't changed: for completion in the YAML files, add\n  \"yaml.schemas\": {}",
            serde_json::to_string_pretty(&schema_mapping()).unwrap_or_default().replace('\n', "\n  ")
        ),
    }
    Ok(())
}

/// Where the configuration files' schemas go in a project.
const SCHEMA_DIR: &str = ".ductus/schema";
const VSCODE: &str = ".vscode/settings.json";

/// The schemas the installed version reads by, for editors (the binary carries them).
fn write_schemas(root: &Path) -> Result<()> {
    let dir = root.join(SCHEMA_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for schema in &ductus_core::schema::SCHEMAS {
        let file = dir.join(schema.name);
        std::fs::write(&file, schema.text).with_context(|| format!("writing {}", file.display()))?;
    }
    vscode_schemas(root).map(|_| ())
}

/// VS Code's `yaml.schemas` for the project: each schema file to the files it describes.
fn schema_mapping() -> serde_json::Value {
    let mapping: serde_json::Map<String, serde_json::Value> = ductus_core::schema::SCHEMAS
        .iter()
        .map(|s| (format!("./{SCHEMA_DIR}/{}", s.name), serde_json::json!(s.files)))
        .collect();
    serde_json::Value::Object(mapping)
}

/// What became of VS Code's settings.
enum Mapped {
    Written,
    Already,
    /// The project's settings map these file patterns to schemas of their own.
    Elsewhere(Vec<String>),
    /// Not plain JSON (VS Code allows comments): not touched.
    Unreadable,
}

/// Point VS Code's YAML extension at the schemas: `yaml.schemas` in `.vscode/settings.json`,
/// added to what's there, never replacing a mapping the project made for the same files.
fn vscode_schemas(root: &Path) -> Result<Mapped> {
    let path = root.join(VSCODE);
    let ours = schema_mapping();
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(settings)) => settings,
            _ => return Ok(Mapped::Unreadable),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let existing = settings.entry("yaml.schemas").or_insert_with(|| serde_json::json!({}));
    let Some(existing) = existing.as_object_mut() else { return Ok(Mapped::Unreadable) };
    let patterns_of = |v: &serde_json::Value| -> Vec<String> {
        match v {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a.iter().filter_map(|p| p.as_str().map(String::from)).collect(),
            _ => Vec::new(),
        }
    };
    let (mut changed, mut elsewhere) = (false, Vec::new());
    for (key, patterns) in ours.as_object().expect("a map") {
        if existing.get(key) == Some(patterns) {
            continue;
        }
        let taken: Vec<String> = existing
            .iter()
            .filter(|(k, _)| *k != key)
            .flat_map(|(_, v)| patterns_of(v))
            .filter(|p| patterns_of(patterns).contains(p))
            .collect();
        if taken.is_empty() {
            existing.insert(key.clone(), patterns.clone());
            changed = true;
        } else {
            elsewhere.extend(taken);
        }
    }
    if changed {
        std::fs::create_dir_all(path.parent().expect("a folder"))?;
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(settings))? + "\n";
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(match (changed, elsewhere.is_empty()) {
        (_, false) => Mapped::Elsewhere(elsewhere),
        (true, true) => Mapped::Written,
        (false, true) => Mapped::Already,
    })
}

/// What `ductus test` runs and how it reports.
#[derive(Default)]
pub struct TestOptions<'a> {
    /// Only this interface's tests.
    pub interface: Option<&'a str>,
    /// Only tests whose name contains this.
    pub filter: Option<&'a str>,
    /// List the tests (and where they are) without running them.
    pub list: bool,
    /// JSON Lines instead of text.
    pub json: bool,
    /// Write no editor typings (`ductus upgrade --check` runs a newer version's tests on the
    /// project as it is).
    pub read_only: bool,
}

/// A test's report from the SDK's runner (`TestResult` in testing.ts).
#[derive(serde::Deserialize)]
struct TestReport {
    name: String,
    ok: Option<bool>,
    error: Option<String>,
    ms: Option<u64>,
    at: Option<Where>,
    #[serde(rename = "failedAt")]
    failed_at: Option<Where>,
    expected: Option<String>,
    actual: Option<String>,
    output: Option<String>,
}

/// A test's `console` lines under it, marked as its own (`│`) so they read apart from ours.
fn output_lines(output: &str) -> String {
    output.lines().map(|l| format!("      │ {l}")).collect::<Vec<_>>().join("\n")
}

#[derive(serde::Deserialize)]
struct Where {
    url: String,
    line: u32,
    column: u32,
}

impl Where {
    fn json(&self, root: &Root) -> serde_json::Value {
        serde_json::json!({ "file": root.relative(&self.url), "line": self.line, "column": self.column })
    }

    fn text(&self, root: &Root) -> String {
        format!("{}:{}:{}", root.relative(&self.url), self.line, self.column)
    }
}

/// Run each interface's `*.test.ts` files (or list them). Returns false if anything failed, or
/// a filter matched nothing.
pub async fn test(root: &Path, env: &str, options: TestOptions<'_>) -> Result<bool> {
    let TestOptions { interface: only, filter, list, json, read_only } = options;
    if !read_only {
        write_types(root).await?;
    }
    let env = Environment::load(root, env)?;
    let project = &Root::new(root);
    let (mut passed, mut failed, mut found) = (0, 0, 0);
    let started = std::time::Instant::now();
    let say = |line: String| {
        if !json {
            println!("{line}");
        }
    };
    let emit = |value: serde_json::Value| {
        if json {
            println!("{value}");
        }
    };
    // Every test's place only where it's shown (writing a stack costs ~30 µs a test).
    let runner_options = serde_json::json!({ "filter": filter, "list": list, "places": json }).to_string();

    for iface in interface_dirs(root, &env)? {
        let label = iface.label();
        if only.is_some_and(|o| o != label) {
            continue;
        }
        let tests = files_under(&iface.dir, &is_test);
        if tests.is_empty() {
            if filter.is_none() {
                say(format!("{label}: no tests"));
            }
            continue;
        }
        let fixtures_dir = iface.dir.join("fixtures");
        let mut fixtures = BTreeMap::new();
        for f in files_under(&fixtures_dir, &|_| true) {
            let name = f.strip_prefix(&fixtures_dir).unwrap().to_string_lossy().replace('\\', "/");
            fixtures.insert(name, std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?);
        }

        let paths: Vec<&Path> = tests.iter().map(PathBuf::as_path).collect();
        let formats = iface
            .config
            .as_ref()
            .map(|c| c.format_spec())
            .unwrap_or_else(|_| ductus_script::DEFAULT_FORMATS.to_string());
        // What the test files and the code they import wrote to `console` while loading.
        let (report, loading) = match Isolate::tests(&paths, &serde_json::to_string(&fixtures)?, &formats).await {
            Ok(mut isolate) => {
                let loading = isolate.take_console();
                (isolate.run_tests_with(&runner_options).await, loading)
            }
            Err(failed) => (Err(failed.error), failed.output),
        };
        let report = match report {
            Ok(r) => r,
            Err(e) => {
                let error = project.readable(&format!("{e:#}"));
                let mut text = format!("{label}\n  ✗ failed to load tests\n");
                if !loading.is_empty() {
                    text.push_str(&format!("{}\n", output_lines(&loading)));
                }
                text.push_str(&indent(&error, 6));
                say(text);
                let mut line = serde_json::json!({ "type": "load", "interface": label, "error": error });
                if !loading.is_empty() {
                    line["output"] = loading.into();
                }
                emit(line);
                failed += 1;
                continue;
            }
        };
        let results: Vec<TestReport> = serde_json::from_str(&report)?;
        if results.is_empty() && loading.is_empty() {
            continue; // nothing matched the filter here
        }
        say(label.clone());
        if !loading.is_empty() {
            say(format!("  while loading\n{}", output_lines(&loading)));
            emit(serde_json::json!({ "type": "output", "interface": label, "output": loading }));
        }
        for r in results {
            found += 1;
            let place = r.at.as_ref().map(|w| w.text(project)).unwrap_or_default();
            let mut line = serde_json::json!({ "type": "test", "interface": label, "name": r.name });
            if let Some(at) = &r.at {
                line["file"] = project.relative(&at.url).into();
                (line["line"], line["column"]) = (at.line.into(), at.column.into());
            }
            // Its output first: it was written before the failure (if any) below.
            let output = r.output.as_deref().map(|o| format!("\n{}", output_lines(o))).unwrap_or_default();
            if let Some(o) = &r.output {
                line["output"] = o.as_str().into();
            }
            match r.ok {
                None => say(format!("  {}  ({place})", r.name)),
                Some(true) => {
                    passed += 1;
                    say(format!("  ✓ {} ({}ms){output}", r.name, r.ms.unwrap_or(0)));
                }
                Some(false) => {
                    failed += 1;
                    // A thrown error's stack says where (readable); a failed `expect` has none.
                    let error = project.readable(r.error.as_deref().unwrap_or(""));
                    let mut shown = error.clone();
                    if let Some(w) = &r.failed_at
                        && !error.lines().any(|l| l.trim_start().starts_with("at "))
                    {
                        shown.push_str(&format!("\nat {}", w.text(project)));
                    }
                    say(format!("  ✗ {}  ({place}){output}\n{}", r.name, indent(&shown, 6)));
                    line["error"] = error.into();
                    if let Some(w) = &r.failed_at {
                        line["failure"] = w.json(project);
                    }
                    if let (Some(expected), Some(actual)) = (&r.expected, &r.actual) {
                        line["expected"] = expected.as_str().into();
                        line["actual"] = actual.as_str().into();
                    }
                }
            }
            if let Some(ok) = r.ok {
                line["ok"] = ok.into();
                line["ms"] = r.ms.unwrap_or(0).into();
            }
            emit(line);
        }
    }

    let unmatched = filter.filter(|_| found == 0 && failed == 0);
    if let Some(text) = unmatched {
        say(format!("no test's name contains `{text}`"));
    }
    if list {
        say(format!("\n{found} test(s)"));
        emit(serde_json::json!({ "type": "summary", "tests": found }));
    } else {
        say(format!("\n{passed} passed, {failed} failed"));
        let ms = started.elapsed().as_millis() as u64;
        emit(serde_json::json!({ "type": "summary", "passed": passed, "failed": failed, "ms": ms }));
    }
    Ok(failed == 0 && unmatched.is_none())
}

/// Project files by interface name (`interfaces/adt/interface.yaml`), with each one's config, to
/// place findings about an interface.
struct Places<'a> {
    files: BTreeMap<String, (String, Option<&'a InterfaceConfig>)>,
}

impl<'a> Places<'a> {
    fn new(root: &Path, interfaces: &'a [InterfaceDir]) -> Self {
        let files = interfaces
            .iter()
            .map(|i| (i.label(), (relative_path(root, &i.dir.join("interface.yaml")), i.config.as_ref().ok())))
            .collect();
        Self { files }
    }

    fn file(&self, interface: &str) -> Option<&str> {
        self.files.get(interface).map(|(f, _)| f.as_str())
    }

    /// A check's finding placed in the interface.yaml and at the key it names.
    fn problem(&self, report: &Report, finding: Finding, problem: &ductus_core::yaml::Problem) -> Finding {
        match problem.interface.as_deref().and_then(|i| self.file(i).map(|f| (i, f))) {
            Some((interface, file)) => report.at_key(finding.interface(interface), file, &problem.key),
            None => finding,
        }
    }

    /// Where a TLS check's place (`interface `adt` destination `lab``, `database `x`
    /// (resources.yaml)`, `alerts.smtp`) is written: the file and key.
    fn tls(&self, text: &str) -> Option<(String, Vec<String>, Option<String>)> {
        let quoted = |s: &str| s.split('`').nth(1).map(str::to_string);
        if let Some(rest) = text.strip_prefix("interface `") {
            let name = rest.split('`').next()?;
            let (file, config) = self.files.get(name)?;
            let key = match rest.split_once("destination `").and_then(|(_, d)| d.split('`').next()) {
                Some(d) => {
                    config.map(|c| c.destination_key(d)).unwrap_or_default().into_iter().map(String::from).collect()
                }
                None => vec!["source".to_string()],
            };
            return Some((file.clone(), key, Some(name.to_string())));
        }
        if text.starts_with("api (ductus.yaml)") {
            return Some(("ductus.yaml".into(), vec!["api".into(), "tls".into()], None));
        }
        if let Some(alerts) = text.strip_prefix("alerts.") {
            let path = alerts.split(':').next()?;
            let key = std::iter::once("alerts").chain(path.split('.')).map(String::from).collect();
            return Some(("ductus.yaml".into(), key, None));
        }
        let (prefix, _) = text.split_once(" (resources.yaml)")?;
        let section = match prefix.split(' ').next()? {
            "api" => "apis",
            "ldap" => "ldap",
            "database" => "databases",
            _ => return None,
        };
        Some(("resources.yaml".into(), vec![section.to_string(), quoted(prefix)?], None))
    }
}

/// A path under the project, relative to it, with `/`.
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// `tls:` on the listeners, `ca` and `certificate` on what the engine connects to, and the
/// `certificates:` entries: names, kinds, files, keys, expiry, and the API on the network without
/// TLS.
fn check_tls<'a>(
    report: &mut Report,
    places: &Places,
    root: &Path,
    env: &Environment,
    resources: &ductus_core::Resources,
    configs: impl IntoIterator<Item = &'a InterfaceConfig> + Clone,
) {
    let settings = ductus_core::settings::Settings::load(root).ok();
    let api = settings.as_ref().map(|s| &s.api);
    // Broken alert settings are reported below; their TLS names are checked once they load.
    let alerts = settings.as_ref().and_then(|s| s.alerts(env).ok().flatten());
    let mut wrong = ductus_core::tls::check(
        &ductus_core::tls::listeners(configs.clone(), api.and_then(|a| a.tls.as_ref())),
        resources,
    );
    wrong.extend(ductus_core::tls::check_outbound(
        &ductus_core::tls::outbound(configs, resources, alerts.as_ref()),
        resources,
    ));
    for problem in wrong {
        let finding = Finding::error("DUC210", &problem);
        let finding = match places.tls(&problem) {
            Some((file, key, interface)) => {
                let finding = report.at_key(finding, &file, &key);
                if let Some(i) = interface { finding.interface(&i) } else { finding }
            }
            None => finding,
        };
        report.add(finding);
    }
    for (name, entry) in &resources.certificates {
        let key = ["certificates", name.as_str()];
        // One at a time, so every broken one is reported.
        match ductus_runtime::tls::Certificates::load(&BTreeMap::from([(name.clone(), entry.clone())])) {
            Err(e) => {
                let finding = report.at_key(Finding::error("DUC211", format!("{e:#}")), "resources.yaml", &key);
                report.add(finding);
            }
            Ok(loaded) => match loaded.pairs().next().map(|p| p.about()) {
                None => report.ok(format!("certificate `{name}`: a CA")),
                Some(about) => {
                    let days = (about.not_after - chrono::Utc::now()).num_days();
                    let line = format!(
                        "certificate `{name}`: {} ({}), expires {} ({days} days)",
                        about.subject,
                        about.names.join(", "),
                        about.not_after.format("%Y-%m-%d")
                    );
                    if days < 30 {
                        let finding = Finding::warning("DUC212", format!("{line}: renew it soon")).hint("renew it");
                        let finding = report.at_key(finding, "resources.yaml", &key);
                        report.add(finding);
                    } else {
                        report.ok(line)
                    }
                }
            },
        }
    }
    if let Some(api) = api
        && api.tls.is_none()
        && let Ok(Some((host, _))) = api.address(None)
        && !ductus_core::settings::loopback(&host)
    {
        let finding = Finding::warning(
            "DUC213",
            format!(
                "api.listen `{}` is on the network without TLS: passwords and tokens cross it readable, and browsers won't keep the session cookie; add `tls: {{ certificate: <name> }}` to `api`",
                api.listen
            ),
        )
        .hint("add `tls: { certificate: <name> }` to `api`");
        let finding = report.at_key(finding, "ductus.yaml", &["api", "listen"]);
        report.add(finding);
    }
}

/// What `validate` warns about directory sign-in: settings under which a renamed person and a
/// new person given a departed one's name can't be told apart.
fn directory_warnings(auth: &ductus_core::ldap::LdapAuth) -> Vec<String> {
    let mut out = Vec::new();
    if auth.attributes.id_source() == ductus_core::ldap::IdSource::None {
        out.push(
            "auth.ldap.attributes.id is `none`: accounts are known by sign-in name only, so a person renamed in the directory gets a second account and someone given a departed person's name gets theirs (tokens included, if not revoked); leave it out (objectGUID or entryUUID) or name the attribute that has an id".to_string(),
        );
    }
    if auth.attributes.username.is_none() && auth.username_attribute().is_none() {
        out.push(format!(
            "auth.ldap.user_filter `{}` doesn't match {{username}} against one attribute: a refresh can't see a sign-in name change; set attributes.username",
            auth.user_filter
        ));
    }
    out
}

/// What `validate` says about `allow:` lists: a summary, names that don't resolve from here,
/// senders `about.from.hosts` names that the list would refuse, and an API list that leaves
/// out this machine. Warnings only: DNS seen from here may not be the engine's.
async fn check_allow<'a>(
    report: &mut Report,
    places: &Places<'_>,
    configs: impl Iterator<Item = &'a InterfaceConfig>,
    api: Option<&ductus_core::allow::Allow>,
) {
    const API: &str = "api (ductus.yaml)";
    let lists: Vec<(String, &ductus_core::allow::Allow, &[String])> = configs
        .filter_map(|c| c.source.allow().map(|a| (c.name.clone(), a, c.about.from.hosts.as_slice())))
        .chain(api.map(|a| (API.to_string(), a, &[][..])))
        .collect();
    if lists.is_empty() {
        return;
    }
    // Every name once (lists, and the hosts `about` names).
    let mut names: Vec<String> = lists
        .iter()
        .flat_map(|(_, a, hosts)| {
            let listed = a.rules().map(|r| r.names).unwrap_or_default();
            let about = hosts.iter().filter(|h| matches!(ductus_core::about::Network::parse(h), Ok(None)));
            listed.into_iter().chain(about.map(|h| h.trim().to_ascii_lowercase()))
        })
        .collect();
    names.sort();
    names.dedup();
    let mut found: BTreeMap<String, Vec<std::net::IpAddr>> = BTreeMap::new();
    for name in names {
        let lookup =
            tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((name.as_str(), 0)));
        let ips = match lookup.await {
            Ok(Ok(a)) => a.map(|a| ductus_core::allow::canonical(a.ip())).collect(),
            _ => Vec::new(),
        };
        found.insert(name, ips);
    }
    let resolve = |name: &str| found.get(name).cloned().unwrap_or_default();

    report.ok(format!(
        "who may connect: {} listener(s) with an `allow:` list ({})",
        lists.len(),
        lists.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>().join(", ")
    ));
    for (name, allow, hosts) in &lists {
        let Ok(rules) = allow.rules() else { continue };
        // The list's place: the interface's source, or the API's in ductus.yaml.
        let place = |report: &Report, finding: Finding, key: &[&str]| match places.file(name) {
            Some(file) => report.at_key(finding.interface(name), file, key),
            None => report.at_key(finding, "ductus.yaml", &["api", "allow"]),
        };
        for listed in &rules.names {
            if resolve(listed).is_empty() {
                let finding = Finding::warning(
                    "DUC220",
                    format!(
                        "{name}: `allow` names `{listed}`, which doesn't resolve from here: nothing is let in for it until it does (the engine looks again every 5 minutes)"
                    ),
                );
                let finding = place(report, finding, &["source"]);
                report.add(finding);
            }
        }
        for host in hosts.iter() {
            let finding = match rules.covers(host, &resolve) {
                Some(true) => continue,
                Some(false) => Finding::warning(
                    "DUC221",
                    format!(
                        "{name}: about.from.hosts `{host}` isn't covered by `allow`: connections from it will be refused"
                    ),
                ),
                None => Finding::warning(
                    "DUC222",
                    format!(
                        "{name}: can't tell whether about.from.hosts `{host}` is allowed (it doesn't resolve from here)"
                    ),
                ),
            };
            let finding = place(report, finding, &["about", "from", "hosts"]);
            report.add(finding);
        }
        let local = [std::net::IpAddr::from([127, 0, 0, 1]), std::net::Ipv6Addr::LOCALHOST.into()];
        let here = |ip| rules.permits(ip, |n, ip| resolve(n).contains(&ip));
        if name == API && !local.into_iter().any(here) {
            let finding = Finding::warning(
                "DUC223",
                "api.allow leaves out 127.0.0.1: `ductus status`, `drain` and the web UI on this machine will be refused (add 127.0.0.1 if you use them here)",
            )
            .hint("add 127.0.0.1 to api.allow if you use them here");
            let finding = report.at_key(finding, "ductus.yaml", &["api", "allow"]);
            report.add(finding);
        }
    }
}

/// Check configs, module loading and types. Returns false if anything is wrong. `json`: one JSON
/// document with every finding instead of the text.
pub async fn validate(
    root: &Path,
    env: &str,
    database_url: Option<String>,
    read_only: bool,
    json: bool,
) -> Result<bool> {
    let mut report = Report::new(root, json);
    check_project(&mut report, root, env, database_url, read_only).await?;
    report.finish(env)
}

/// Everything `validate` checks, into `report`.
async fn check_project(
    report: &mut Report,
    root: &Path,
    env: &str,
    database_url: Option<String>,
    read_only: bool,
) -> Result<()> {
    let types = if read_only { Isolate::generate_types().await? } else { write_types(root).await? };
    let mut files = BTreeMap::new();
    files.insert(key(&root.join(TYPES_FILE)), types);

    report.say(format!("environment: {env}"));
    let env = match Environment::load(root, env) {
        Ok(e) => e,
        Err(e) => {
            let file = format!("environments/{env}.yaml");
            let finding = report.load_error(100, Severity::Error, format!("{e:#}"), &e, &file);
            report.add(finding);
            return Ok(());
        }
    };
    const RESOURCES: &str = "resources.yaml";
    let resources = match ductus_core::Resources::load(root, &env) {
        Ok(r) => {
            if !r.databases.is_empty()
                || !r.sftp.is_empty()
                || !r.apis.is_empty()
                || !r.ldap.is_empty()
                || !r.certificates.is_empty()
            {
                let directories =
                    if r.ldap.is_empty() { String::new() } else { format!(", {} directory(ies)", r.ldap.len()) };
                let certificates = if r.certificates.is_empty() {
                    String::new()
                } else {
                    format!(", {} certificate(s)", r.certificates.len())
                };
                report.ok(format!(
                    "resources: {} database(s), {} SFTP server(s), {} API(s){directories}{certificates}",
                    r.databases.len(),
                    r.sftp.len(),
                    r.apis.len()
                ));
            }
            for (name, written, read) in &r.without_drive {
                let finding = Finding::warning(
                    "DUC115",
                    format!(
                        "certificate `{name}`: `{}` has no drive letter, so Windows reads it from the project's drive: {}. Write the whole path (C:\\... or \\\\server\\share\\...), or one relative to the project folder",
                        written.display(),
                        read.display()
                    ),
                )
                .hint("write the whole path (C:\\... or \\\\server\\share\\...), or one relative to the project folder");
                report.add(report.at_key(finding, RESOURCES, &["certificates", name.as_str()]));
            }
            for name in r.insecure_ldap() {
                let finding = Finding::warning(
                    "DUC116",
                    format!(
                        "ldap `{name}`: plain ldap:// (`insecure: true`) sends passwords readable; use ldaps:// or `starttls: true` outside a test directory"
                    ),
                )
                .hint("use ldaps:// or `starttls: true` outside a test directory");
                report.add(report.at_key(finding, RESOURCES, &["ldap", name]));
            }
            for (name, mode) in r.unverified_postgres() {
                let finding = Finding::warning(
                    "DUC117",
                    format!(
                        "database `{name}`: `tls: {mode}` doesn't check its certificate (whoever answers at its address is believed, and sees every query): set `tls: verify-full`, with `ca:` for a private CA"
                    ),
                )
                .hint("set `tls: verify-full`, with `ca:` for a private CA");
                report.add(report.at_key(finding, RESOURCES, &["databases", name]));
            }
            for name in r.unverified_sqlserver() {
                let finding = Finding::warning(
                    "DUC118",
                    format!(
                        "database `{name}`: `trust_server_certificate: true` doesn't check its certificate (whoever answers at its address is believed, and sees every query): remove it, with `ca:` for a private CA (SQL Server's own generated certificate can't be verified: ask the DBA for one from your CA)"
                    ),
                )
                .hint("remove `trust_server_certificate`, with `ca:` for a private CA");
                report.add(report.at_key(finding, RESOURCES, &["databases", name]));
            }
            for name in r.ntlm_sqlserver() {
                let finding = Finding::warning(
                    "DUC119",
                    format!(
                        "database `{name}`: a named Windows account logs in from Linux with NTLM, never Kerberos: Microsoft is retiring NTLM and some domains block it (the login is then refused); a SQL login, or the engine on Windows, avoids it"
                    ),
                );
                report.add(report.at_key(finding, RESOURCES, &["databases", name]));
            }
            for note in crate::service::notes(Some(&r), false) {
                match note.strip_prefix("⚠ ") {
                    Some(warning) => report.add(Finding::warning("DUC150", warning)),
                    None => report.say(note),
                }
            }
            Some(r)
        }
        Err(e) => {
            let finding = report.load_error(110, Severity::Error, format!("resources: {e:#}"), &e, RESOURCES);
            report.add(finding);
            None
        }
    };

    let interfaces = interface_dirs(root, &env)?;
    let places = Places::new(root, &interfaces);
    for iface in &interfaces {
        let label = iface.label();
        let file = places.file(&label).unwrap_or_default().to_string();
        match &iface.config {
            Err(e) => {
                let finding = report.load_error(120, Severity::Error, format!("{label}: {e:#}"), e, &file);
                report.add(finding.interface(&label));
                continue;
            }
            Ok(config) => {
                if let Some(script) = config.script_path()
                    && let Err(e) = load_functions(config, &script).await
                {
                    let text = format!("{e:#}");
                    let finding = Finding::error("DUC125", format!("{label}: {}", first_line(&text))).interface(&label);
                    report.add(report.at_key(finding, &file, &function_key(config, &text)));
                    continue;
                }
                report.ok(format!("{label}: config and code load"));
            }
        }
    }

    let configs = || interfaces.iter().filter_map(|i| i.config.as_ref().ok());
    let mut wiring: Vec<(&str, ductus_core::yaml::Problem)> =
        ductus_core::config::check_endpoints(configs()).into_iter().map(|p| ("DUC200", p)).collect();
    wiring.extend(ductus_core::config::check_routes(configs()).into_iter().map(|p| ("DUC201", p)));
    if let Some(r) = &resources {
        wiring.extend(r.missing_references(configs()).into_iter().map(|p| ("DUC202", p)));
    }
    for (code, problem) in wiring {
        let finding = places.problem(report, Finding::error(code, &problem.text), &problem);
        report.add(finding);
    }
    for name in ductus_core::config::unrouted(configs()) {
        let finding = Finding::warning(
            "DUC204",
            format!(
                "{name}: has `source: internal` but no interface routes to it (`destination: {{ interface: {name} }}`)"
            ),
        )
        .hint(format!("route to it from another interface: `destination: {{ interface: {name} }}`"))
        .interface(name);
        let finding = match places.file(name) {
            Some(file) => report.at_key(finding, file, &["source"]),
            None => finding,
        };
        report.add(finding);
    }

    let shared = project_files(root, &interfaces, &mut files)?;
    let analysis = ductus_script::analyze(&files, &shared, true)?;

    // Databases and APIs named in code must exist (each place reported once, even in a shared
    // library many interfaces import).
    if let Some(r) = &resources {
        let mut seen = std::collections::BTreeSet::new();
        for config in configs() {
            for reference in code_references(config, &analysis) {
                if let Some(kind) = resource_kind(&reference.kind)
                    && let Some(missing) = r.missing(kind, &reference.name)
                    && seen.insert((&reference.file, reference.line, reference.column))
                {
                    let at = relative(root, &reference.file);
                    let finding = Finding::error(
                        "DUC203",
                        format!(
                            "{at}:{}:{} {}(\"{}\"): {missing}",
                            reference.line, reference.column, reference.kind, reference.name
                        ),
                    )
                    .file(at)
                    .at(reference.line, reference.column)
                    .interface(&config.name)
                    .hint(format!("add `{}` to resources.yaml, or use one it defines", reference.name));
                    report.add(finding);
                }
            }
        }
    }

    // `console` lines this environment writes to its log for every message: fine for a while, to
    // look into something, but not to keep (an environment saying `console: debug` is one where
    // logging everything is the point). Tests' own lines only show in `ductus test`.
    let lowest = env.console();
    if lowest != ductus_core::ConsoleLevel::Debug {
        let setting = format!("environments/{}.yaml", env.name);
        let level = match env.console_written() {
            true => format!("`console: {}`", lowest.as_str()),
            false => "`console:` isn't set, so `info`".to_string(),
        };
        for call in analysis.console.iter().filter(|c| !c.file.ends_with(".test.ts")) {
            if !lowest.lets_through(ductus_core::ConsoleLevel::of_method(&call.method)) {
                continue;
            }
            let at = relative(root, &call.file);
            let how_often = if call.top_level { "at every start" } else { "for every message this code runs for" };
            let finding = Finding::warning(
                "DUC206",
                format!(
                    "{at}:{}:{} console.{}(): environment `{}` writes it to the log {how_often} ({level}); remove it once you've seen what you needed, or set `console: warn` (or `off`) in {setting}",
                    call.line, call.column, call.method, env.name
                ),
            )
            .file(at)
            .at(call.line, call.column)
            .hint(format!("remove it, or set `console: warn` (or `off`) in {setting}"));
            report.add(finding);
        }
    }

    // Which systems each interface connects, for `ductus impact`.
    let require_about = ductus_core::settings::Settings::load(root).map(|s| s.require_about).unwrap_or(false);
    let mut gaps = 0;
    for config in configs() {
        for gap in ductus_core::impact::about_gaps(config, resources.as_ref().unwrap_or(&Default::default())) {
            gaps += 1;
            let message = format!("{}: {gap}", config.name);
            let hint = gap.text.split_once(": ").map(|(_, fix)| fix.to_string()).unwrap_or_default();
            let finding =
                if require_about { Finding::error("DUC205", message) } else { Finding::warning("DUC205", message) };
            report.add(places.problem(report, finding.hint(hint), &gap));
        }
    }
    if !require_about && gaps > 0 {
        report.say(
            "  (warnings: `ductus impact` can't find what isn't named; `require_about: true` in ductus.yaml makes them errors)",
        );
    }

    // The company's own documentation fields (docs-schema.yaml) against their values.
    match ductus_core::docs::Schema::load(root) {
        Err(e) => {
            let finding = Finding::error("DUC250", format!("{}: {e:#}", ductus_core::docs::FILE));
            report.add(finding.file(ductus_core::docs::FILE));
        }
        Ok(schema) => {
            let none = Default::default();
            for line in ductus_core::docs::check_project(&schema, configs(), resources.as_ref().unwrap_or(&none)) {
                report.add(Finding::error("DUC251", line));
            }
        }
    }

    // Directory sign-in (`auth.ldap`): its directory must be in resources.yaml, with a usable `ca`.
    if let (Ok(settings), Some(r)) = (ductus_core::settings::Settings::load(root), &resources)
        && settings.auth.ldap.is_some()
    {
        // Its `ca` (a wrong name or kind, a CA that doesn't load) is reported with TLS below.
        let certificates = ductus_runtime::tls::Certificates::load_what_loads(&r.certificates);
        let ca_reported = settings
            .auth
            .ldap
            .as_ref()
            .and_then(|a| r.ldap.get(&a.resource))
            .is_some_and(|l| ductus_runtime::ldap::trust(l, &certificates).is_err());
        let ldap = ["auth", "ldap"];
        match ductus_runtime::ldap::Directory::new(settings.auth.ldap.as_ref(), r, &certificates) {
            Ok(Some(d)) => {
                report.ok(format!("sign-in with directory `{}` ({})", d.name, d.server.url));
                for line in directory_warnings(&d.auth) {
                    report.add(report.at_key(Finding::warning("DUC231", line), "ductus.yaml", &ldap));
                }
            }
            Ok(None) => {}
            Err(_) if ca_reported => {}
            Err(e) => report.add(report.at_key(Finding::error("DUC230", format!("{e:#}")), "ductus.yaml", &ldap)),
        }
    }

    // TLS: what each listener names, and each certificate loads as the engine would load it.
    if let Some(r) = &resources {
        check_tls(report, &places, root, &env, r, configs());
    }
    // Who may connect: warnings only (malformed lists are refused when the config loads).
    let api_allow = ductus_core::settings::Settings::load(root).ok().and_then(|s| s.api.allow);
    check_allow(report, &places, configs(), api_allow.as_ref()).await;

    // Alerts (`alerts:` in ductus.yaml), with this environment's variables and secrets.
    const SETTINGS: &str = "ductus.yaml";
    match ductus_core::settings::Settings::load(root) {
        Err(e) => {
            let finding = report.load_error(130, Severity::Error, format!("{e:#}"), &e, SETTINGS);
            report.add(finding);
        }
        Ok(settings) => match settings.alerts(&env) {
            Err(e) => {
                let finding = report.load_error(130, Severity::Error, format!("{e:#}"), &e, SETTINGS);
                report.add(finding);
            }
            Ok(None) => {
                for config in configs().filter(|c| !c.alert.to.is_empty()) {
                    let finding = Finding::warning(
                        "DUC240",
                        format!("{}: alert.to is set, but ductus.yaml has no `alerts:`", config.name),
                    )
                    .hint("add `alerts:` to ductus.yaml, or remove `alert.to`")
                    .interface(&config.name);
                    let file = places.file(&config.name).unwrap_or_default().to_string();
                    report.add(report.at_key(finding, &file, &["alert", "to"]));
                }
            }
            Ok(Some(alerts)) => {
                report.ok(format!("alerts: {} channel(s), {} rule(s)", alerts.channels.len(), alerts.rules.len()));
                for warning in alerts.warnings() {
                    report.add(report.at_key(Finding::warning("DUC241", warning), SETTINGS, &["alerts"]));
                }
                for problem in configs().flat_map(|c| alerts.check_interface(&c.name, &c.alert)) {
                    report.add(places.problem(report, Finding::error("DUC242", &problem.text), &problem));
                }
                // With the database, owners are matched to accounts below.
                let accounts_known = database_url.as_deref().is_some_and(|u| !u.trim().is_empty());
                for owner in
                    unreachable_owners(&alerts, &settings.owners, &resources.clone().unwrap_or_default(), configs())
                        .into_iter()
                        .filter(|_| !accounts_known)
                {
                    report.add(Finding::warning(
                        "DUC243",
                        format!(
                            "owner `{owner}`: not in `owners:` in ductus.yaml and not an email address: its alerts reach someone only if a user has that name and an email (the engine checks at start)"
                        ),
                    ).hint("write the owner's email, or add the team to `owners:` in ductus.yaml"));
                }
            }
        },
    }

    // Against the database the engines use, reading only: what the next start would refuse or
    // change. Without one, say what wasn't checked.
    let configs: Vec<&InterfaceConfig> = configs().collect();
    match database_url.filter(|u| !u.trim().is_empty()) {
        Some(url) => {
            let teams = ductus_core::settings::Settings::load(root).map(|s| s.owners).unwrap_or_default();
            check_database(report, &places, root, &url, &configs, &resources.clone().unwrap_or_default(), &teams).await
        }
        None => report.add(
            Finding::note(
                "DUC390",
                "not checked against a database (set DUCTUS_DATABASE_URL): lanes that would strand waiting messages, retention changes",
            )
            .hint("set DUCTUS_DATABASE_URL (or --database-url)"),
        ),
    }

    let diagnostics = &analysis.diagnostics;
    for d in diagnostics {
        report.add(Finding::diagnostic(d, root));
    }
    if diagnostics.is_empty() {
        report.ok("types: no errors");
    }
    Ok(())
}

/// Where in interface.yaml a code loading error points: the destination whose `transform` names
/// the function the module doesn't export, else `script`.
fn function_key<'a>(config: &'a InterfaceConfig, error: &str) -> Vec<&'a str> {
    let missing = error.split_once("must export a function named `").and_then(|(_, rest)| rest.split('`').next());
    let Some(missing) = missing else { return vec!["script"] };
    config
        .destinations()
        .find(|(_, d)| d.transform.as_deref() == Some(missing))
        .map(|(name, _)| [config.destination_key(name), vec!["transform"]].concat())
        .unwrap_or_else(|| vec!["script"])
}

/// `validate` against a database, reading only (it may run with a newer binary than the engines
/// there, before deploying it): the lanes guard the engine runs at start, and retention changes
/// running, waiting for a confirmation, or that the next start would begin.
async fn check_database(
    report: &mut Report,
    places: &Places<'_>,
    root: &Path,
    url: &str,
    configs: &[&InterfaceConfig],
    resources: &ductus_core::Resources,
    teams: &BTreeMap<String, ductus_core::alerts::OwnerContact>,
) {
    use ductus_runtime::ownership::{Lanes, check_lanes, stranded_lines};
    use std::collections::HashMap;
    let store = match ductus_store::Store::connect_read_only(url).await {
        Ok(s) => s,
        Err(e) => return report.add(Finding::error("DUC300", format!("database: {e:#}"))),
    };
    match ductus_runtime::database_security(&store, url).await {
        Ok(s) if s.warns() => report.add(
            Finding::warning(
                "DUC301",
                format!(
                    "database: {s}; whoever answers at its address is believed: add sslmode=verify-full to DUCTUS_DATABASE_URL (sslrootcert=/path/ca.pem for a private CA)"
                ),
            )
            .hint("add sslmode=verify-full to DUCTUS_DATABASE_URL (sslrootcert=/path/ca.pem for a private CA)"),
        ),
        Ok(s) if s.local && !s.verified() => report.ok(format!("database: {s} (on this machine)")),
        Ok(s) => report.ok(format!("database: {s}")),
        Err(e) => report.add(Finding::warning(
            "DUC302",
            format!("database: couldn't read how the connection is protected: {e:#}"),
        )),
    }
    // Before a first install (`ductus upgrade --check` runs this too): no tables to read yet.
    if !store.has_table("deliveries").await.unwrap_or(false) {
        report.ok("database: new (no engine has run on it yet): no messages wait, no retention is applied");
        return;
    }
    check_owners(report, &store, configs, resources, teams).await;
    let lanes: Vec<Lanes> = configs.iter().filter_map(|c| Lanes::of(c)).collect();
    let names: Vec<String> = lanes.iter().map(|l| l.interface.clone()).collect();
    let recorded = store.has_table("interface_applied").await.unwrap_or(false);
    let applied: HashMap<String, i32> = if recorded {
        store.applied_lanes(&names).await.map(|v| v.into_iter().collect()).unwrap_or_default()
    } else {
        HashMap::new()
    };
    // A finding about one interface, at a key of its interface.yaml.
    let about = |report: &Report, finding: Finding, interface: &str, key: &[&str]| match places.file(interface) {
        Some(file) => report.at_key(finding.interface(interface), file, key),
        None => finding.interface(interface),
    };
    match check_lanes(&store, &lanes, &applied).await {
        Err(e) => report.add(Finding::error("DUC304", format!("database: checking lanes: {e:#}"))),
        Ok(check) => {
            for (stranded, line) in check.stranded.iter().zip(stranded_lines(&check.stranded)) {
                let finding = Finding::error("DUC303", line)
                    .hint("keep `ordering` as it is until they're sent (`ductus drain --wait` stops new messages until nothing waits)");
                report.add(about(report, finding, &stranded.row.interface, &["ordering"]));
            }
            if !check.stranded.is_empty() {
                report.say(
                    "  (the engine would refuse to start: no worker reads those lanes. Keep `ordering` as it is until they're sent; `ductus drain --wait` stops new messages until nothing waits)",
                );
            }
            for r in &check.raised {
                let finding = Finding::warning(
                    "DUC305",
                    format!(
                        "{}: lanes go from {} to {} with {} message(s) waiting: a patient's (key's) new messages may be sent before its older ones until those are",
                        r.interface, r.before, r.now, r.waiting
                    ),
                );
                report.add(about(report, finding, &r.interface, &["ordering"]));
            }
            if check.stranded.is_empty() {
                report.ok("database: every interface's lanes reach the messages waiting for it");
            }
            if !recorded {
                report.add(Finding::note(
                    "DUC391",
                    "no engine of this version has run on this database yet: the lanes interfaces last ran with aren't recorded, so raised lanes can't be told (lowered ones are checked)",
                ));
            }
        }
    }
    // Retention: what the engine does with each interface's configured value at its next start.
    let settings = ductus_core::settings::Settings::load(root).ok();
    match store.retention().await {
        Err(e) => report.add(Finding::warning("DUC309", format!("database: reading retention: {e:#}"))),
        Ok(states) => {
            let states: HashMap<&str, &ductus_store::RetentionState> =
                states.iter().map(|s| (s.interface.as_str(), s)).collect();
            for c in configs.iter().filter(|c| !c.responds()) {
                let Some(state) = states.get(c.name.as_str()) else { continue };
                let default = settings.as_ref().map_or(ductus_core::Retention::DEFAULT, |s| s.retention);
                let wanted = c.retention.unwrap_or(default).days() as ductus_store::Tier;
                let window = |days: ductus_store::Tier| format!("{}w", days / 7);
                if let Some(from) = state.changing_from {
                    let left = state.rows_left.map(|n| format!(", ~{n} rows left")).unwrap_or_default();
                    let finding = Finding::warning(
                        "DUC306",
                        format!(
                            "{}: a retention change from {} to {} is still running{left}",
                            c.name,
                            window(from),
                            window(state.days)
                        ),
                    );
                    report.add(about(report, finding, &c.name, &["retention"]));
                }
                if wanted < state.days && state.confirmed != Some(wanted) {
                    let finding = Finding::warning(
                        "DUC307",
                        format!(
                            "{}: retention {} is shorter than the {} applied: it deletes messages, so it waits for `ductus retention confirm {}`",
                            c.name,
                            window(wanted),
                            window(state.days),
                            c.name
                        ),
                    )
                    .hint(format!("ductus retention confirm {}", c.name));
                    report.add(about(report, finding, &c.name, &["retention"]));
                } else if wanted != state.days && state.changing_from.is_none() {
                    let finding = Finding::warning(
                        "DUC308",
                        format!(
                            "{}: retention changes from {} to {} at the next start (its messages move in the background)",
                            c.name,
                            window(state.days),
                            window(wanted)
                        ),
                    );
                    report.add(about(report, finding, &c.name, &["retention"]));
                }
            }
        }
    }
}

/// Owners matched to the accounts in the database, as `ductus owners` does: warnings only (an
/// owner never makes a project invalid; `ductus owners --check` is the gate).
async fn check_owners(
    report: &mut Report,
    store: &ductus_store::Store,
    configs: &[&InterfaceConfig],
    resources: &ductus_core::Resources,
    teams: &BTreeMap<String, ductus_core::alerts::OwnerContact>,
) {
    use ductus_core::owners::{self, Reach};
    let accounts = match store.accounts().await {
        Ok(a) => a,
        Err(e) => return report.add(Finding::warning("DUC310", format!("owners: couldn't read the accounts: {e:#}"))),
    };
    let people = ductus_runtime::alerts::people(&accounts.users, &accounts.former);
    for (owner, _) in owners::by_owner(owners::owned(configs.iter().copied(), resources)) {
        if owner.is_empty() {
            continue;
        }
        let reach = owners::reach(&owner, &people, teams);
        let (line, hint) = match (&reach, owners::advice(&owner, &reach, &people)) {
            (_, Some(advice)) => match advice.to {
                Some(to) => {
                    let fix = owners::fix(&owner, &to);
                    (format!("{}: `{fix}`", advice.text), Some(fix))
                }
                None => (advice.text, None),
            },
            (Reach::Disabled(u), None) => (format!("the account {u} is disabled: move what they own"), None),
            (Reach::Several(n), None) => (format!("names {n} accounts: write the email of the one meant"), None),
            (Reach::Nobody, None) => (
                "not an account, a team in `owners:` or an email address: write their email, or add the team to `owners:` in ductus.yaml".into(),
                None,
            ),
            _ => continue,
        };
        let finding = Finding::warning("DUC244", format!("owner `{owner}`: {line}"));
        report.add(match hint {
            Some(h) => finding.hint(h),
            None => finding,
        });
    }
}

/// Owners that alerts sent to `owner` can't reach from what's in the project (users live in the
/// database, so a user's name may still reach someone).
fn unreachable_owners<'a>(
    alerts: &ductus_core::alerts::AlertSettings,
    directory: &BTreeMap<String, ductus_core::alerts::OwnerContact>,
    resources: &ductus_core::Resources,
    configs: impl Iterator<Item = &'a InterfaceConfig> + Clone,
) -> Vec<String> {
    use ductus_core::alerts::OWNER;
    let to_owner = alerts.rules.iter().any(|r| r.to.iter().any(|t| t == OWNER))
        || configs.clone().any(|c| c.alert.to.iter().any(|t| t == OWNER));
    if !to_owner {
        return Vec::new();
    }
    ductus_core::owners::by_owner(ductus_core::owners::owned(configs, resources))
        .into_iter()
        .map(|(owner, _)| owner)
        .filter(|o| !o.is_empty() && ductus_core::alerts::owner_addresses(o, &[], directory).is_none())
        .collect()
}

/// `ductus alerts test <channel>`: a test notification from ductus.yaml, without an engine.
pub async fn test_alert(root: &Path, env: &str, channel: &str) -> Result<()> {
    let environment = Environment::load(root, env)?;
    let settings = ductus_core::settings::Settings::load(root)?;
    let Some(alerts) = settings.alerts(&environment)? else {
        anyhow::bail!("ductus.yaml has no `alerts:`");
    };
    let resources = ductus_core::Resources::load(root, &environment)?;
    let certificates = std::sync::Arc::new(ductus_runtime::tls::Certificates::load(&resources.certificates)?);
    let alerts = ductus_runtime::alerts::Alerts::new(alerts, settings.owners, env, certificates)?;
    alerts.test(channel).await?;
    println!("sent a test notification to `{channel}`");
    Ok(())
}

/// Every .ts file of the project into `files`; returns the shared modules (see
/// `ductus_runtime::project::project_files`).
fn project_files(
    root: &Path,
    interfaces: &[InterfaceDir],
    files: &mut BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let dirs: Vec<&Path> = interfaces.iter().map(|i| i.dir.as_path()).collect();
    ductus_runtime::project::project_files(root, &dirs, files)
}

/// The databases and APIs each interface's code uses (`db("...")`, `api("...")`, through its
/// imports).
pub(crate) async fn code_uses(root: &Path, interfaces: &[InterfaceDir]) -> Result<ductus_core::impact::CodeUses> {
    let dirs: Vec<&Path> = interfaces.iter().map(|i| i.dir.as_path()).collect();
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    ductus_runtime::project::code_uses(root, &dirs, &configs).await
}

/// `ductus impact`: every interface a vendor, application or host touches, how, and where to
/// look. Returns false if some interfaces couldn't be checked (their config doesn't load).
pub async fn impact(root: &Path, env: &str, query: ductus_core::impact::Query, resolve: bool) -> Result<bool> {
    use ductus_core::impact::{self, Query};
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let resources = ductus_core::Resources::load(root, &environment)?;
    println!("environment: {env}");
    let mut complete = true;
    for i in &interfaces {
        if let Err(e) = &i.config {
            complete = false;
            println!("✗ {}: not checked, its config doesn't load: {}", i.label(), first_line(&format!("{e:#}")));
        }
    }
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    let code = code_uses(root, &interfaces).await?;

    let lookup = |name: &str| -> Vec<std::net::IpAddr> {
        use std::net::ToSocketAddrs;
        if !resolve {
            return Vec::new();
        }
        let mut ips: Vec<_> = (name, 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
        ips.sort();
        ips.dedup();
        ips
    };
    let hits = impact::impact(configs.iter().copied(), &resources, &code, &query, &lookup);

    let affected: std::collections::BTreeSet<&str> = hits.iter().map(|h| h.interface.as_str()).collect();
    println!("{query}: {} interface(s) affected, of {}\n", affected.len(), configs.len());
    let width = hits.iter().map(|h| h.role.chars().count()).max().unwrap_or(0);
    let owned = |owner: &str| if owner.is_empty() { String::new() } else { format!("  (owner: {owner})") };
    let (mut last, mut heading_owner) = ("", "");
    for h in &hits {
        if h.interface != last {
            if !last.is_empty() {
                println!();
            }
            println!("{}{}", h.interface, owned(&h.owner));
            (last, heading_owner) = (&h.interface, &h.owner);
        }
        // A destination with an owner of its own says so.
        let own = if h.owner != heading_owner { owned(&h.owner) } else { String::new() };
        println!("  {:<width$}  {}{own}", h.role, h.reason);
        for e in &h.endpoints {
            println!("  {:<width$}  → {e}", "");
        }
    }
    if hits.is_empty() {
        match &query {
            Query::Vendor(_) | Query::Application(_) => {
                let known = impact::known_values(configs.iter().copied(), &resources, &query);
                let what = if matches!(query, Query::Vendor(_)) { "vendors" } else { "applications" };
                if known.is_empty() {
                    println!(
                        "no {what} are named in this project yet (`about:` in interface.yaml, `vendor`/`application` in resources.yaml)"
                    );
                } else {
                    println!("known {what}: {}", known.join(", "));
                }
            }
            Query::Host(_) => {}
        }
    }
    if matches!(query, Query::Host(_)) && !resolve {
        println!(
            "\nnote: host names in the config were compared as written; add --resolve to look them up and match by address"
        );
    }
    let unnamed = configs.iter().filter(|c| !impact::about_gaps(c, &resources).is_empty()).count();
    if unnamed > 0 && !matches!(query, Query::Host(_)) {
        println!(
            "\nnote: {unnamed} interface(s) don't name every system they connect (see `ductus validate`); they may be missing here"
        );
    }
    Ok(complete)
}

/// `ductus docs`: the company-defined documentation fields (docs-schema.yaml) and their values
/// for every interface, destination and resources.yaml entry. Returns false when `missing`
/// finds required values that aren't there (or something doesn't load).
pub fn docs(root: &Path, env: &str, only_missing: bool, json: bool) -> Result<bool> {
    use ductus_core::docs;
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let resources = ductus_core::Resources::load(root, &environment)?;
    let schema = docs::Schema::load(root)?;
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    let mut complete = interfaces.iter().all(|i| i.config.is_ok());
    let rows: Vec<(docs::Entry, Vec<String>)> = docs::entries(configs.iter().copied(), &resources)
        .into_iter()
        .map(|e| {
            let missing = schema.missing(e.level, &e.custom);
            (e, missing)
        })
        .filter(|(_, missing)| !only_missing || !missing.is_empty())
        .collect();
    complete &= rows.iter().all(|(_, missing)| missing.is_empty());
    if json {
        let out: Vec<_> = rows
            .iter()
            .map(|(e, missing)| serde_json::json!({"level": e.level, "name": e.name, "custom": e.custom, "missing": missing}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(complete);
    }
    println!("environment: {env}");
    if schema.fields.is_empty() {
        println!("no fields are declared (docs-schema.yaml)");
        return Ok(complete);
    }
    for (entry, missing) in &rows {
        let fields: Vec<_> = schema.for_level(entry.level).collect();
        if fields.is_empty() {
            continue;
        }
        println!("\n{} ({})", entry.name, entry.level.label());
        let width = fields.iter().map(|d| d.title().chars().count()).max().unwrap_or(0);
        for d in fields {
            let shown = match entry.custom.get(&d.key) {
                Some(v) if !docs::is_blank(v) => match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(a) => {
                        a.iter().map(|i| i.as_str().unwrap_or("?")).collect::<Vec<_>>().join(", ")
                    }
                    other => other.to_string(),
                },
                _ if missing.contains(&d.key) => "⚠ missing".into(),
                _ => "–".into(),
            };
            if !only_missing || missing.contains(&d.key) {
                println!("  {:<width$}  {shown}", d.title());
            }
        }
    }
    if only_missing && rows.is_empty() {
        println!("every required field has a value");
    }
    Ok(complete)
}

/// `ductus ports`: listening ports and HTTP routes, outbound connections, and port conflicts
/// for one environment. Returns false on conflicts or interfaces that don't load.
pub fn ports(root: &Path, env: &str) -> Result<bool> {
    use ductus_core::ports;
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    println!("environment: {env}");
    let mut problems = 0;
    for i in &interfaces {
        if let Err(e) = &i.config {
            problems += 1;
            println!("✗ {}: not shown, its config doesn't load: {}", i.label(), first_line(&format!("{e:#}")));
        }
    }
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();

    let mut listening = ports::listeners(configs.iter().copied());
    // The engine's own REST API listens too (unless it's `off`).
    let settings = ductus_core::settings::Settings::load(root)?;
    let mut conflicts = Vec::new();
    if let Some((host, port)) = settings.api.address(None)? {
        if let Some(l) = listening.iter().find(|l| l.port == port) {
            conflicts.push(format!("port {port}: the REST API (`api.listen` in ductus.yaml) and `{}`", l.interface));
        }
        listening.push(ports::api_listener(&host, port, settings.api.tls.as_ref(), settings.api.allow.as_ref()));
        listening.sort();
    }
    println!("\nListening");
    if listening.is_empty() {
        println!("  (nothing: no MLLP or HTTP sources, and the REST API is off)");
    } else {
        table(
            &["PORT", "PROTOCOL", "HOST", "INTERFACE", "ROUTE", "TLS", "ALLOWED"],
            listening.into_iter().map(|l| {
                vec![
                    l.port.to_string(),
                    l.protocol.into(),
                    l.host,
                    l.interface,
                    l.route.unwrap_or_default(),
                    l.tls,
                    l.allow,
                ]
            }),
        );
    }

    let resources = match ductus_core::Resources::load(root, &environment) {
        Ok(r) => r,
        Err(e) => {
            problems += 1;
            println!("✗ resources: {e:#}");
            Default::default()
        }
    };
    let outbound = ports::outbound(configs.iter().copied(), &resources);
    println!("\nConnects to");
    if outbound.is_empty() {
        println!("  (nothing)");
    } else {
        table(
            &["HOST", "PORT", "PROTOCOL", "USED BY", "TLS"],
            outbound.into_iter().map(|o| vec![o.host, o.port.to_string(), o.protocol.into(), o.used_by, o.tls]),
        );
    }

    println!();
    conflicts.extend(ductus_core::config::check_endpoints(configs.iter().copied()).into_iter().map(|p| p.text));
    if conflicts.is_empty() {
        println!("✓ no port conflicts");
    }
    for c in &conflicts {
        problems += 1;
        println!("✗ {c}");
    }
    Ok(problems == 0)
}

/// Print rows as aligned columns, indented two spaces.
fn table(headers: &[&str], rows: impl IntoIterator<Item = Vec<String>>) {
    let rows: Vec<Vec<String>> = rows.into_iter().collect();
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).chain([headers[i].len()]).max().unwrap_or(0))
        .collect();
    let line = |cells: Vec<&str>| {
        let padded: Vec<String> = cells.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        println!("  {}", padded.join("  ").trim_end());
    };
    line(headers.to_vec());
    for r in &rows {
        line(r.iter().map(String::as_str).collect());
    }
}

pub enum SecretCommand {
    Init,
    Set { name: String, value: Option<String>, root: PathBuf, env: String },
    List { root: PathBuf, env: String },
}

pub fn secret(command: SecretCommand) -> Result<()> {
    use ductus_core::secrets::{KEY_VAR, SecretStore, generate_key};
    match command {
        SecretCommand::Init => {
            println!("{}", generate_key());
            eprintln!(
                "Set this as {KEY_VAR} wherever Ductus runs. Keep it out of Git; without it the secrets can't be read."
            );
        }
        SecretCommand::Set { name, value, root, env } => {
            let key = std::env::var(KEY_VAR)
                .with_context(|| format!("{KEY_VAR} is not set (create one with `ductus secret init`)"))?;
            let value = match value {
                Some(v) => v,
                None => {
                    use std::io::{IsTerminal, Read};
                    let mut v = String::new();
                    // Typed: one line. Piped: everything, so multi-line values such as SSH
                    // private keys work (`ductus secret set KEY < id_ed25519`).
                    if std::io::stdin().is_terminal() {
                        std::io::stdin().read_line(&mut v)?;
                    } else {
                        std::io::stdin().read_to_string(&mut v)?;
                    }
                    v.trim_end_matches(['\r', '\n']).to_string()
                }
            };
            SecretStore::set(&root, &env, &name, &value, &key)?;
            println!("saved {name} in secrets/{env}.json");
        }
        SecretCommand::List { root, env } => match SecretStore::open(&root, &env)? {
            Some(store) => store.names().for_each(|n| println!("{n}")),
            None => println!("no secrets for {env}"),
        },
    }
    Ok(())
}

fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n")
}

/// Load an interface's code the way the engine does: every function its destinations name
/// must be exported.
async fn load_functions(config: &ductus_core::InterfaceConfig, script: &Path) -> Result<()> {
    let functions = config.functions();
    let functions: Vec<(&str, &str)> = functions.iter().map(|(n, f)| (n.as_str(), f.as_str())).collect();
    let mut isolate = Isolate::host();
    isolate.capture_console(); // top-level `console` lines: never among validate's output
    isolate.add_functions(script, &functions, config.timeout).await.map(drop)
}

pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ductus_core::ldap::{LdapAttributes, LdapAuth};

    #[test]
    fn validate_warns_when_renamed_and_reused_names_cant_be_told_apart() {
        let auth = |filter: &str, attributes: LdapAttributes| LdapAuth {
            resource: "corp".into(),
            user_base: "dc=h".into(),
            user_filter: filter.into(),
            attributes,
            groups: Default::default(),
            nested: false,
            group_base: None,
            refresh: std::time::Duration::from_secs(900),
        };
        assert!(directory_warnings(&auth("(uid={username})", LdapAttributes::default())).is_empty());
        let none = LdapAttributes { id: "none".into(), ..Default::default() };
        let w = directory_warnings(&auth("(uid={username})", none));
        assert!(w.len() == 1 && w[0].contains("gets a second account"), "{w:?}");
        let w = directory_warnings(&auth("(|(cn=*{username}*))", LdapAttributes::default()));
        assert!(w.len() == 1 && w[0].contains("set attributes.username"), "{w:?}");
        let named = LdapAttributes { username: Some("uid".into()), ..Default::default() };
        assert!(directory_warnings(&auth("(|(cn=*{username}*))", named)).is_empty());
    }
}
