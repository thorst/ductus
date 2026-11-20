//! `ductus dev`: the engine with a Postgres of its own, for local development. Nothing to install
//! or configure: the first run downloads Postgres once per user, later runs reuse it.
//!
//! Only this command ever starts a Postgres; `ductus run` never does.

use anyhow::{Context, Result, bail};
use postgresql_embedded::{PostgreSQL, Settings, VersionReq};
use serde_json::json;
use std::fs::{File, TryLockError};
use std::io::{IsTerminal, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Postgres version `dev` runs, so every developer works against the same one.
const VERSION: &str = "=18.6.0";
/// Where the binaries come from: zonky's, on Maven Central. They carry their own libraries (the
/// theseus-rs ones need the host's libxml2.so.2, which Ubuntu 26.04 no longer has).
const BINARIES: &str = "https://github.com/zonkyio/embedded-postgres-binaries";
/// The database `dev` creates for the engine.
const DATABASE: &str = "ductus";

pub struct Options {
    pub root: PathBuf,
    pub engine: ductus_runtime::EngineOptions,
    /// Listen on this port instead of the one kept from the last run.
    pub port: Option<u16>,
    /// Delete the project's dev database first.
    pub reset: bool,
    /// Don't ask before `reset` deletes it.
    pub yes: bool,
    /// Start only the Postgres, not the engine (for tools, or a service under test).
    pub database_only: bool,
}

/// What is kept between runs in `<project>/.ductus/postgres/dev.json`.
struct Kept {
    port: u16,
    password: String,
}

pub async fn run(options: Options) -> Result<()> {
    let state = options.root.join(".ductus");
    let dir = state.join("postgres");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // Held for as long as this process lives; the system lets go of it however the process ends.
    let lock = File::create(state.join("dev.lock")).context("creating .ductus/dev.lock")?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            bail!("`ductus dev` is already running for this project (stop it first: Ctrl-C in its terminal)")
        }
        Err(TryLockError::Error(e)) => return Err(e).context("locking .ductus/dev.lock"),
    }

    if std::env::var_os("DUCTUS_DATABASE_URL").is_some() {
        println!("note: DUCTUS_DATABASE_URL is ignored by `ductus dev`; it uses its own Postgres");
    }
    if options.reset {
        reset(&dir, options.yes)?;
    }

    let data_dir = dir.join("data");
    let mut settings = Settings::new();
    let fresh = Kept::read(&dir);
    settings.releases_url = BINARIES.into();
    settings.version = VersionReq::parse(VERSION)?;
    settings.host = "127.0.0.1".into();
    settings.password = fresh.as_ref().map_or(settings.password.clone(), |k| k.password.clone());
    settings.data_dir = data_dir.clone();
    settings.password_file = dir.join("password");
    settings.temporary = false;
    // Starting a cluster the first time takes longer than the crate's default 5 s.
    settings.timeout = Some(Duration::from_secs(120));
    settings.configuration.insert("listen_addresses".into(), "127.0.0.1".into());
    match std::env::var_os("DUCTUS_POSTGRES_DIR") {
        // An unpacked copy of the binaries (a folder holding `bin/`), for machines that can't download.
        Some(copy) => {
            settings.installation_dir = PathBuf::from(copy);
            settings.trust_installation_dir = true;
        }
        None => settings.installation_dir = cache_dir()?,
    }

    // A Postgres left behind by an engine that was killed still holds the data and the port.
    // (Fails harmlessly when nothing runs or nothing is installed yet.)
    if data_dir.join("postmaster.pid").exists() {
        let _ = PostgreSQL::new(settings.clone()).stop().await;
    }

    let port = match options.port.or(fresh.as_ref().map(|k| k.port)) {
        Some(port) if port_is_free(port) => port,
        Some(port) if options.port.is_some() => bail!("port {port} is in use; pick another with --port"),
        Some(port) => {
            let new = free_port()?;
            println!("port {port} is taken now; using {new} instead");
            new
        }
        None => free_port()?,
    };
    settings.port = port;
    Kept { port, password: settings.password.clone() }.write(&dir)?;
    let mut postgres = PostgreSQL::new(settings);

    if postgres.status() == postgresql_embedded::Status::NotInstalled {
        println!("downloading Postgres {} (once per user, about 30 MB)...", VERSION.trim_start_matches('='));
    }
    postgres.setup().await.context(
        "setting up Postgres (the first run downloads it from Maven Central (zonky's embedded-postgres-binaries); \
         without internet access, unpack a copy and point DUCTUS_POSTGRES_DIR at its folder)",
    )?;
    postgres.start().await.with_context(|| {
        format!("starting Postgres on port {port} (its log: {})", data_dir.join("start.log").display())
    })?;
    if !postgres.database_exists(DATABASE).await? {
        postgres.create_database(DATABASE).await?;
    }
    let url = postgres.settings().url(DATABASE);
    println!("Postgres {} is running for this project:\n  {url}", VERSION.trim_start_matches('='));
    println!("  (data in {}; stops with the engine)", data_dir.display());

    let result = if options.database_only {
        println!("(only the database: Ctrl-C stops it)");
        ductus_runtime::stop_signal().await;
        Ok(())
    } else {
        if let Err(e) = first_user(&options.root, &url).await {
            eprintln!("couldn't make a dev login ({e:#}): create one with `ductus user create` against the URL above");
        }
        let mut engine = options.engine;
        // Reloads in place from the API (an editor's "Deploy to dev", `ductus reload`); no handover.
        engine.reloads_in_place = true;
        let found = Found::path(&options.root);
        match api_url(&options.root, engine.api_listen.as_deref()) {
            Ok(Some(api)) => {
                let (path, api) = (found.clone(), api.clone());
                engine.on_started = Some(Box::new(move || {
                    if let Err(e) = (Found { api, pid: std::process::id() }).write(&path) {
                        eprintln!("couldn't write {} ({e:#}): editors won't find this engine", path.display());
                    }
                }));
            }
            Ok(None) => {} // no API: nothing to find
            Err(e) => eprintln!("note: editors won't find this engine ({e:#})"),
        }
        let ran = ductus_runtime::run(&options.root, &url, engine).await;
        let _ = std::fs::remove_file(&found);
        ran
    };

    match postgres.stop().await {
        Ok(()) => println!("Postgres stopped"),
        Err(e) => eprintln!("Postgres didn't stop cleanly ({e}); the next `ductus dev` stops it"),
    }
    drop(lock);
    result
}

/// `--inspect`: the interfaces named (by name, or a file or folder inside one, as an editor passes
/// the open file), and the debugger's server listening already, so a debugger's first tries find
/// it while Postgres starts (VS Code's waits up to its configuration's `timeout`).
pub fn inspect(root: &Path, env: &str, chosen: &[String], address: &str) -> Result<Option<ductus_runtime::Inspect>> {
    if chosen.is_empty() {
        return Ok(None);
    }
    let addr = ductus_runtime::inspect::address(address)?;
    let env = ductus_core::Environment::load(root, env)?;
    let interfaces = crate::project::interface_dirs(root, &env)?;
    let mut names = Vec::new();
    for c in chosen {
        let found = interfaces.iter().find(|i| crate::project::names_interface(root, c, i)).with_context(|| {
            let all: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
            format!("`--inspect {c}` isn't an interface (there are: {})", all.join(", "))
        })?;
        if !names.contains(&found.label()) {
            names.push(found.label());
        }
    }
    let inspector = ductus_runtime::inspect::Inspector::listen(addr)?;
    println!(
        "debugger listening on {addr} for {} (VS Code: \"Ductus: debug this interface\"; Chrome: chrome://inspect)",
        names.join(", ")
    );
    Ok(Some(ductus_runtime::Inspect { inspector: std::sync::Arc::new(inspector), interfaces: names }))
}

/// A dev database starts with no users, and nobody wants to run a command before seeing the UI:
/// make a `dev` admin with a random password, kept in `.ductus/dev-login` (the database is local
/// and holds only test data) so every start can say how to sign in.
async fn first_user(root: &Path, url: &str) -> Result<()> {
    let store = ductus_store::Store::connect(url, 2).await?;
    let file = root.join(".ductus").join("dev-login");
    if store.user_count().await? == 0 {
        let password = ductus_core::password::generate();
        let hash = ductus_core::password::hash(&password)?;
        store
            .create_user(
                &ductus_store::NewUser {
                    username: "dev",
                    display_name: "Dev",
                    email: "",
                    role: ductus_store::Role::Admin,
                    permissions: &[],
                    password_hash: Some(&hash),
                    must_change_password: false,
                    source: "local",
                },
                &ductus_store::Audit::new(ductus_store::By::cli("ductus dev"), "user.create"),
            )
            .await?;
        std::fs::write(&file, &password).with_context(|| format!("writing {}", file.display()))?;
    }
    if let Ok(password) = std::fs::read_to_string(&file) {
        println!("Web UI login: user `dev`, password `{}` (kept in {})", password.trim(), file.display());
    }
    if let Err(e) = editor_token(&store, root).await {
        eprintln!("couldn't make the editor's token ({e:#}): Deploy to dev in an editor will ask for one");
    }
    Ok(())
}

/// The name of the `dev` user's token kept in `.ductus/dev-token`.
const EDITOR_TOKEN: &str = "editor";

/// A token of the `dev` user's for editors and `ductus reload` here (`deploy`, and `payloads` to
/// read a message to debug), kept in `.ductus/dev-token` beside `dev-login`, so an editor's "Deploy
/// to dev" signs in without asking. Kept while it works; made again when it doesn't (a `--reset`,
/// revoked). None when there's no `dev` user (someone removed it): said, nothing made.
async fn editor_token(store: &ductus_store::Store, root: &Path) -> Result<()> {
    let file = root.join(".ductus").join("dev-token");
    if let Ok(kept) = std::fs::read_to_string(&file)
        && store.authenticate(kept.trim()).await?.is_some()
    {
        return Ok(());
    }
    let user = store.user_by_name("dev").await?.context("there's no `dev` user")?;
    let audit = ductus_store::Audit::new(ductus_store::By::cli("ductus dev"), "token.create");
    // One left by an earlier start whose file was deleted: replaced.
    let revoked = ductus_store::Audit::new(ductus_store::By::cli("ductus dev"), "token.revoke");
    store.revoke_token(EDITOR_TOKEN, Some(user.id), &revoked).await?;
    let permissions = ["deploy".to_string(), "payloads".to_string()];
    let (_, secret) =
        store.create_token(EDITOR_TOKEN, ductus_store::Role::Operator, &permissions, Some(user.id), &audit).await?;
    write_private(&file, &secret)
}

/// Write a file only this account may read (Unix; on Windows the user's profile already is).
fn write_private(file: &Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(file).with_context(|| format!("writing {}", file.display()))?;
    out.write_all(text.as_bytes()).with_context(|| format!("writing {}", file.display()))
}

/// How editors find the running `ductus dev` of a project: `.ductus/dev-engine.json`, written once
/// its interfaces and API are up, deleted when it stops (a killed one leaves it: its `api` then
/// doesn't answer). With `.ductus/dev-token`, all an editor needs to reload it.
#[derive(Clone)]
pub struct Found {
    /// The API's URL, as this machine reaches it: `http://127.0.0.1:7700`.
    pub api: String,
    pub pid: u32,
}

impl Found {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".ductus").join("dev-engine.json")
    }

    fn write(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(&json!({ "api": self.api, "pid": self.pid }))?;
        std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
    }

    /// The running dev engine of the project at `root`, as it said when it started.
    pub fn read(root: &Path) -> Option<Found> {
        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Self::path(root)).ok()?).ok()?;
        Some(Found { api: value["api"].as_str()?.to_string(), pid: u32::try_from(value["pid"].as_u64()?).ok()? })
    }
}

/// The URL this machine reaches the engine's API at (None: `off`): its address from ductus.yaml or
/// `--api-listen`, any address (`0.0.0.0`, `::`) as loopback, https with `api.tls`.
fn api_url(root: &Path, listen: Option<&str>) -> Result<Option<String>> {
    let settings = ductus_core::settings::Settings::load(root)?;
    let Some((host, port)) = settings.api.address(listen)? else { return Ok(None) };
    let host = match host.as_str() {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "::" => "[::1]".to_string(),
        h if h.contains(':') => format!("[{h}]"),
        h => h.to_string(),
    };
    let scheme = if settings.api.tls.is_some() { "https" } else { "http" };
    Ok(Some(format!("{scheme}://{host}:{port}")))
}

impl Kept {
    fn read(dir: &Path) -> Option<Kept> {
        let text = std::fs::read_to_string(dir.join("dev.json")).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(Kept {
            port: u16::try_from(value["port"].as_u64()?).ok()?,
            password: value["password"].as_str()?.to_string(),
        })
    }

    fn write(&self, dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(&json!({ "port": self.port, "password": self.password }))?;
        std::fs::write(dir.join("dev.json"), text).context("saving .ductus/postgres/dev.json")
    }
}

/// Delete the cluster, asking first unless told not to.
fn reset(dir: &Path, yes: bool) -> Result<()> {
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("--reset deletes this project's dev database: add --yes to do it without asking");
        }
        print!("Delete this project's dev database ({})? Type yes: ", dir.display());
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            bail!("not deleted");
        }
    }
    for name in ["data", "dev.json", "password"] {
        let path = dir.join(name);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    println!("deleted the dev database");
    Ok(())
}

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Where downloaded Postgres binaries are kept, once per user and version.
fn cache_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    };
    Ok(base.context("can't find a per-user cache folder: set DUCTUS_POSTGRES_DIR")?.join("ductus").join("postgres"))
}
