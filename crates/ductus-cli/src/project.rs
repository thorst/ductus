//! Project commands: `types`, `test`, `validate`, `ports`, `secret`.

use anyhow::{Context, Result};
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::Isolate;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ductus_runtime::project::{TYPES_FILE, code_references, files_under, key, resource_kind};

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

/// Write the SDK typings and (if missing) a tsconfig.json, so VS Code gets IntelliSense and hover docs.
pub async fn write_types(root: &Path) -> Result<String> {
    let types = Isolate::generate_types().await?;
    let path = root.join(TYPES_FILE);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, &types).with_context(|| format!("writing {}", path.display()))?;
    let tsconfig = root.join("tsconfig.json");
    if !tsconfig.exists() {
        std::fs::write(&tsconfig, TSCONFIG)?;
    }
    Ok(types)
}

pub async fn types(root: &Path) -> Result<()> {
    write_types(root).await?;
    println!("wrote {} (and tsconfig.json if it was missing)", root.join(TYPES_FILE).display());
    Ok(())
}

/// Run each interface's `*.test.ts` files. Returns false if anything failed.
pub async fn test(root: &Path, env: &str, only: Option<&str>) -> Result<bool> {
    write_types(root).await?;
    let env = Environment::load(root, env)?;
    let (mut passed, mut failed) = (0, 0);

    for iface in interface_dirs(root, &env)? {
        let label = iface.label();
        if only.is_some_and(|o| o != label) {
            continue;
        }
        let tests = files_under(&iface.dir, &is_test);
        if tests.is_empty() {
            println!("{label}: no tests");
            continue;
        }
        let fixtures_dir = iface.dir.join("fixtures");
        let mut fixtures = BTreeMap::new();
        for f in files_under(&fixtures_dir, &|_| true) {
            let name = f.strip_prefix(&fixtures_dir).unwrap().to_string_lossy().replace('\\', "/");
            fixtures.insert(name, std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?);
        }

        println!("{label}");
        let paths: Vec<&Path> = tests.iter().map(PathBuf::as_path).collect();
        let formats = iface
            .config
            .as_ref()
            .map(|c| c.format_spec())
            .unwrap_or_else(|_| ductus_script::DEFAULT_FORMATS.to_string());
        let report = match Isolate::tests(&paths, &serde_json::to_string(&fixtures)?, &formats).await {
            Ok(mut isolate) => isolate.run_tests().await,
            Err(e) => Err(e),
        };
        let report = match report {
            Ok(r) => r,
            Err(e) => {
                println!("  ✗ failed to load tests\n{}", indent(&format!("{e:#}"), 6));
                failed += 1;
                continue;
            }
        };
        let results: Vec<serde_json::Value> = serde_json::from_str(&report)?;
        for r in results {
            let name = r["name"].as_str().unwrap_or("?");
            if r["ok"] == true {
                passed += 1;
                println!("  ✓ {name} ({}ms)", r["ms"]);
            } else {
                failed += 1;
                println!("  ✗ {name}\n{}", indent(r["error"].as_str().unwrap_or(""), 6));
            }
        }
    }

    println!("\n{passed} passed, {failed} failed");
    Ok(failed == 0)
}

/// `tls:` on the listeners, `ca` and `certificate` on what the engine connects to, and the
/// `certificates:` entries: names, kinds, files, keys, expiry, and the API on the network without
/// TLS. Returns the number of problems.
fn check_tls<'a>(
    root: &Path,
    env: &Environment,
    resources: &ductus_core::Resources,
    configs: impl IntoIterator<Item = &'a InterfaceConfig> + Clone,
) -> usize {
    let settings = ductus_core::settings::Settings::load(root).ok();
    let api = settings.as_ref().map(|s| &s.api);
    // Broken alert settings are reported below; their TLS names are checked once they load.
    let alerts = settings.as_ref().and_then(|s| s.alerts(env).ok().flatten());
    let mut problems = 0;
    let mut wrong = ductus_core::tls::check(
        &ductus_core::tls::listeners(configs.clone(), api.and_then(|a| a.tls.as_ref())),
        resources,
    );
    wrong.extend(ductus_core::tls::check_outbound(
        &ductus_core::tls::outbound(configs, resources, alerts.as_ref()),
        resources,
    ));
    for problem in wrong {
        problems += 1;
        println!("✗ {problem}");
    }
    for (name, entry) in &resources.certificates {
        // One at a time, so every broken one is reported.
        match ductus_runtime::tls::Certificates::load(&BTreeMap::from([(name.clone(), entry.clone())])) {
            Err(e) => {
                problems += 1;
                println!("✗ {e:#}");
            }
            Ok(loaded) => match loaded.pairs().next().map(|p| p.about()) {
                None => println!("✓ certificate `{name}`: a CA"),
                Some(about) => {
                    let days = (about.not_after - chrono::Utc::now()).num_days();
                    let line = format!(
                        "certificate `{name}`: {} ({}), expires {} ({days} days)",
                        about.subject,
                        about.names.join(", "),
                        about.not_after.format("%Y-%m-%d")
                    );
                    if days < 30 { println!("⚠ {line}: renew it soon") } else { println!("✓ {line}") }
                }
            },
        }
    }
    if let Some(api) = api
        && api.tls.is_none()
        && let Ok(Some((host, _))) = api.address(None)
        && !ductus_core::settings::loopback(&host)
    {
        println!(
            "⚠ api.listen `{}` is on the network without TLS: passwords and tokens cross it readable, and browsers won't keep the session cookie; add `tls: {{ certificate: <name> }}` to `api`",
            api.listen
        );
    }
    problems
}

/// Check configs, module loading and types. Returns false if anything is wrong.
pub async fn validate(root: &Path, env: &str, database_url: Option<String>) -> Result<bool> {
    let types = write_types(root).await?;
    let mut problems = 0;
    let mut files = BTreeMap::new();
    files.insert(key(&root.join(TYPES_FILE)), types);

    println!("environment: {env}");
    let env = match Environment::load(root, env) {
        Ok(e) => e,
        Err(e) => {
            println!("✗ {e:#}");
            return Ok(false);
        }
    };
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
                println!(
                    "✓ resources: {} database(s), {} SFTP server(s), {} API(s){directories}{certificates}",
                    r.databases.len(),
                    r.sftp.len(),
                    r.apis.len()
                );
            }
            for name in r.insecure_ldap() {
                println!(
                    "⚠ ldap `{name}`: plain ldap:// (`insecure: true`) sends passwords readable; use ldaps:// or `starttls: true` outside a test directory"
                );
            }
            Some(r)
        }
        Err(e) => {
            problems += 1;
            println!("✗ resources: {e:#}");
            None
        }
    };

    let interfaces = interface_dirs(root, &env)?;
    for iface in &interfaces {
        let label = iface.label();
        match &iface.config {
            Err(e) => {
                problems += 1;
                println!("✗ {label}: {e:#}");
                continue;
            }
            Ok(config) => {
                if let Some(script) = config.script_path()
                    && let Err(e) = load_functions(config, &script).await
                {
                    problems += 1;
                    println!("✗ {label}: {}", first_line(&format!("{e:#}")));
                    continue;
                }
                println!("✓ {label}: config and code load");
            }
        }
    }

    let configs = || interfaces.iter().filter_map(|i| i.config.as_ref().ok());
    let mut wiring = ductus_core::config::check_endpoints(configs());
    wiring.extend(ductus_core::config::check_routes(configs()));
    if let Some(r) = &resources {
        wiring.extend(r.missing_references(configs()));
    }
    for problem in wiring {
        problems += 1;
        println!("✗ {problem}");
    }
    for name in ductus_core::config::unrouted(configs()) {
        println!(
            "⚠ {name}: has `source: internal` but no interface routes to it (`destination: {{ interface: {name} }}`)"
        );
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
                    problems += 1;
                    let at = relative(root, &reference.file);
                    println!(
                        "✗ {at}:{}:{} {}(\"{}\"): {missing}",
                        reference.line, reference.column, reference.kind, reference.name
                    );
                }
            }
        }
    }

    // Which systems each interface connects, for `ductus impact`.
    let require_about = ductus_core::settings::Settings::load(root).map(|s| s.require_about).unwrap_or(false);
    let mut gaps = 0;
    for config in configs() {
        for gap in ductus_core::impact::about_gaps(config, resources.as_ref().unwrap_or(&Default::default())) {
            gaps += 1;
            println!("{} {}: {gap}", if require_about { "✗" } else { "⚠" }, config.name);
        }
    }
    if require_about {
        problems += gaps;
    } else if gaps > 0 {
        println!(
            "  (warnings: `ductus impact` can't find what isn't named; `require_about: true` in ductus.yaml makes them errors)"
        );
    }

    // The company's own documentation fields (docs-schema.yaml) against their values.
    match ductus_core::docs::Schema::load(root) {
        Err(e) => {
            problems += 1;
            println!("✗ {}: {e:#}", ductus_core::docs::FILE);
        }
        Ok(schema) => {
            let none = Default::default();
            for line in ductus_core::docs::check_project(&schema, configs(), resources.as_ref().unwrap_or(&none)) {
                problems += 1;
                println!("✗ {line}");
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
        match ductus_runtime::ldap::Directory::new(settings.auth.ldap.as_ref(), r, &certificates) {
            Ok(Some(d)) => println!("✓ sign-in with directory `{}` ({})", d.name, d.server.url),
            Ok(None) => {}
            Err(_) if ca_reported => {}
            Err(e) => {
                problems += 1;
                println!("✗ {e:#}");
            }
        }
    }

    // TLS: what each listener names, and each certificate loads as the engine would load it.
    if let Some(r) = &resources {
        problems += check_tls(root, &env, r, configs());
    }

    // Alerts (`alerts:` in ductus.yaml), with this environment's variables and secrets.
    match ductus_core::settings::Settings::load(root) {
        Err(e) => {
            problems += 1;
            println!("✗ {e:#}");
        }
        Ok(settings) => match settings.alerts(&env) {
            Err(e) => {
                problems += 1;
                println!("✗ {e:#}");
            }
            Ok(None) => {
                for config in configs().filter(|c| !c.alert.to.is_empty()) {
                    println!("⚠ {}: alert.to is set, but ductus.yaml has no `alerts:`", config.name);
                }
            }
            Ok(Some(alerts)) => {
                println!("✓ alerts: {} channel(s), {} rule(s)", alerts.channels.len(), alerts.rules.len());
                for warning in alerts.warnings() {
                    println!("⚠ {warning}");
                }
                for problem in configs().flat_map(|c| alerts.check_interface(&c.name, &c.alert)) {
                    problems += 1;
                    println!("✗ {problem}");
                }
                for owner in
                    unreachable_owners(&alerts, &settings.owners, &resources.clone().unwrap_or_default(), configs())
                {
                    println!(
                        "⚠ owner `{owner}`: not in `owners:` in ductus.yaml and not an email address: its alerts reach someone only if a user has that name and an email (the engine checks at start)"
                    );
                }
            }
        },
    }

    // Against the database the engines use, reading only: what the next start would refuse or
    // change. Without one, say what wasn't checked.
    let configs: Vec<&InterfaceConfig> = configs().collect();
    match database_url.filter(|u| !u.trim().is_empty()) {
        Some(url) => problems += check_database(root, &url, &configs).await,
        None => println!(
            "note: not checked against a database (set DUCTUS_DATABASE_URL): lanes that would strand waiting messages, retention changes"
        ),
    }

    let diagnostics = &analysis.diagnostics;
    for d in diagnostics {
        if d.is_error() {
            problems += 1;
        }
        let at = match &d.file {
            Some(f) => format!("{}:{}:{} ", relative(root, f), d.line, d.column),
            None => String::new(),
        };
        // Codes 90000+ are Ductus lints; below that, TypeScript's own.
        let code = if d.code >= 90_000 { format!("DX{}", d.code - 90_000) } else { format!("TS{}", d.code) };
        println!("{at}{} {code}: {}", d.category, d.message);
    }
    if diagnostics.is_empty() {
        println!("✓ types: no errors");
    }

    println!("\n{}", if problems == 0 { "valid".to_string() } else { format!("{problems} problem(s)") });
    Ok(problems == 0)
}

/// `validate` against a database, reading only (it may run with a newer binary than the engines
/// there, before deploying it): the lanes guard the engine runs at start, and retention changes
/// running, waiting for a confirmation, or that the next start would begin. Returns the problems
/// (what would refuse the start), after printing everything.
async fn check_database(root: &Path, url: &str, configs: &[&InterfaceConfig]) -> usize {
    use ductus_runtime::ownership::{Lanes, check_lanes, stranded_lines};
    use std::collections::HashMap;
    let store = match ductus_store::Store::connect_read_only(url).await {
        Ok(s) => s,
        Err(e) => {
            println!("✗ database: {e:#}");
            return 1;
        }
    };
    let lanes: Vec<Lanes> = configs.iter().filter_map(|c| Lanes::of(c)).collect();
    let names: Vec<String> = lanes.iter().map(|l| l.interface.clone()).collect();
    let recorded = store.has_table("interface_applied").await.unwrap_or(false);
    let applied: HashMap<String, i32> = if recorded {
        store.applied_lanes(&names).await.map(|v| v.into_iter().collect()).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let mut problems = 0;
    match check_lanes(&store, &lanes, &applied).await {
        Err(e) => {
            println!("✗ database: checking lanes: {e:#}");
            problems += 1;
        }
        Ok(check) => {
            for line in stranded_lines(&check.stranded) {
                problems += 1;
                println!("✗ {line}");
            }
            if !check.stranded.is_empty() {
                println!(
                    "  (the engine would refuse to start: no worker reads those lanes. Keep `ordering` as it is until they're sent; `ductus drain --wait` stops new messages until nothing waits)"
                );
            }
            for r in &check.raised {
                println!(
                    "⚠ {}: lanes go from {} to {} with {} message(s) waiting: a patient's (key's) new messages may be sent before its older ones until those are",
                    r.interface, r.before, r.now, r.waiting
                );
            }
            if check.stranded.is_empty() {
                println!("✓ database: every interface's lanes reach the messages waiting for it");
            }
            if !recorded {
                println!(
                    "note: no engine of this version has run on this database yet: the lanes interfaces last ran with aren't recorded, so raised lanes can't be told (lowered ones are checked)"
                );
            }
        }
    }
    // Retention: what the engine does with each interface's configured value at its next start.
    let settings = ductus_core::settings::Settings::load(root).ok();
    match store.retention().await {
        Err(e) => println!("⚠ database: reading retention: {e:#}"),
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
                    println!(
                        "⚠ {}: a retention change from {} to {} is still running{left}",
                        c.name,
                        window(from),
                        window(state.days)
                    );
                }
                if wanted < state.days && state.confirmed != Some(wanted) {
                    println!(
                        "⚠ {}: retention {} is shorter than the {} applied: it deletes messages, so it waits for `ductus retention confirm {}`",
                        c.name,
                        window(wanted),
                        window(state.days),
                        c.name
                    );
                } else if wanted != state.days && state.changing_from.is_none() {
                    println!(
                        "⚠ {}: retention changes from {} to {} at the next start (its messages move in the background)",
                        c.name,
                        window(state.days),
                        window(wanted)
                    );
                }
            }
        }
    }
    problems
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
        listening.push(ports::api_listener(&host, port, settings.api.tls.as_ref()));
        listening.sort();
    }
    println!("\nListening");
    if listening.is_empty() {
        println!("  (nothing: no MLLP or HTTP sources, and the REST API is off)");
    } else {
        table(
            &["PORT", "PROTOCOL", "HOST", "INTERFACE", "ROUTE", "TLS"],
            listening.into_iter().map(|l| {
                vec![l.port.to_string(), l.protocol.into(), l.host, l.interface, l.route.unwrap_or_default(), l.tls]
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
    conflicts.extend(ductus_core::config::check_endpoints(configs.iter().copied()));
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
    Isolate::host().add_functions(script, &functions, config.timeout).await.map(drop)
}

pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}
