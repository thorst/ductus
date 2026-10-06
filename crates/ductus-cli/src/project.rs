//! Project commands: `types`, `test`, `validate`.

use anyhow::{Context, Result};
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::Isolate;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TYPES_FILE: &str = ".ductus/types/ductus-sdk.d.ts";

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
struct InterfaceDir {
    dir: PathBuf,
    config: Result<InterfaceConfig>,
}

impl InterfaceDir {
    fn label(&self) -> String {
        match &self.config {
            Ok(c) => c.name.clone(),
            Err(_) => self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

fn interface_dirs(root: &Path, env: &Environment) -> Result<Vec<InterfaceDir>> {
    let base = root.join("interfaces");
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .with_context(|| format!("reading {}", base.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("interface.yaml").is_file())
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().map(|dir| InterfaceDir { config: InterfaceConfig::load_in(&dir, env), dir }).collect())
}

/// Every file under `dir` (recursively) accepted by `keep`, skipping hidden folders and node_modules.
fn files_under(dir: &Path, keep: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" {
                out.extend(files_under(&path, keep));
            }
        } else if keep(&path) {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn is_ts(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "ts") && !p.to_string_lossy().ends_with(".d.ts")
}

fn is_test(p: &Path) -> bool {
    p.to_string_lossy().ends_with(".test.ts")
}

/// Forward-slash absolute path, the form the type checker uses.
fn key(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().replace('\\', "/")
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
        let report = match Isolate::tests(&paths, &serde_json::to_string(&fixtures)?).await {
            Ok(mut isolate) => isolate.run_tests(),
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

/// Check configs, module loading and types. Returns false if anything is wrong.
pub async fn validate(root: &Path, env: &str) -> Result<bool> {
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
    match ductus_core::Resources::load(root, &env) {
        Ok(r) if r.databases.is_empty() => {}
        Ok(r) => println!("✓ resources: {} database(s)", r.databases.len()),
        Err(e) => {
            problems += 1;
            println!("✗ resources: {e:#}");
        }
    }

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
                    && let Err(e) = Isolate::interface(&script).await
                {
                    problems += 1;
                    println!("✗ {label}: {}", first_line(&format!("{e:#}")));
                    continue;
                }
                println!("✓ {label}: config and code load");
            }
        }
    }

    // Every .ts file in the project, so imports of shared libraries resolve. Files outside any
    // interface folder are shared modules: one instance per isolate, imported by many interfaces.
    let interface_dirs: Vec<String> = interfaces.iter().map(|i| format!("{}/", key(&i.dir))).collect();
    let mut shared = Vec::new();
    for f in files_under(root, &is_ts) {
        let k = key(&f);
        if !interface_dirs.iter().any(|d| k.starts_with(d)) {
            shared.push(k.clone());
        }
        files.insert(k, std::fs::read_to_string(&f)?);
    }

    let diagnostics = ductus_script::type_check(&files, &shared)?;
    for d in &diagnostics {
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
                    let mut v = String::new();
                    std::io::stdin().read_line(&mut v)?;
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

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}
