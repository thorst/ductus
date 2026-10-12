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
        ductus_runtime::run(&options.root, &url, options.engine).await
    };

    match postgres.stop().await {
        Ok(()) => println!("Postgres stopped"),
        Err(e) => eprintln!("Postgres didn't stop cleanly ({e}); the next `ductus dev` stops it"),
    }
    drop(lock);
    result
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
