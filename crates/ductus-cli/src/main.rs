mod admin;
mod dev;
mod doctor;
mod envfile;
mod init;
mod new;
mod owners;
mod project;
#[cfg(windows)]
mod service;
mod sinks;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ductus", version, about = "Code-first healthcare integration engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Read settings from this file (`NAME=value` lines, the names of the DUCTUS_* variables);
    /// variables already set win. Read before anything else, in `main`.
    #[arg(long, global = true, env = "DUCTUS_ENV_FILE", value_name = "FILE")]
    #[allow(dead_code)]
    env_file: Option<PathBuf>,
}

/// What `ductus run` takes (and the Windows service, which is `ductus run --service`).
#[derive(clap::Args, Clone)]
struct RunArgs {
    /// Project folder containing `interfaces/`.
    #[arg(long, env = "DUCTUS_PROJECT", default_value = ".")]
    root: PathBuf,
    #[arg(long, env = "DUCTUS_DATABASE_URL")]
    database_url: Option<String>,
    /// Threads in the shared worker pool (default: one per CPU core).
    #[arg(long, env = "DUCTUS_THREADS")]
    threads: Option<usize>,
    /// Maximum PostgreSQL connections.
    #[arg(long, env = "DUCTUS_DB_CONNECTIONS", default_value_t = 32)]
    db_connections: u32,
    /// Environment: variables from environments/<env>.yaml, secrets from secrets/<env>.json.
    #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
    env: String,
    /// Where the REST API listens (`host:port`, or `off`), instead of `api.listen` in
    /// ductus.yaml (default 127.0.0.1:7700).
    #[arg(long, env = "DUCTUS_API_LISTEN")]
    api_listen: Option<String>,
    /// On Ctrl-C or SIGTERM, how long to finish the messages in hand before exiting anyway
    /// (they stay pending for the next start). Service managers should wait longer.
    #[arg(long, env = "DUCTUS_STOP_TIMEOUT", default_value = "30s", value_parser = parse_duration)]
    stop_timeout: std::time::Duration,
    /// Run as a Windows service, under the service manager (the installer sets this up).
    #[arg(long)]
    service: bool,
}

impl RunArgs {
    /// Where the project is, which database, and the engine's settings.
    fn parts(self) -> Result<(PathBuf, String, ductus_runtime::EngineOptions)> {
        let defaults = ductus_runtime::EngineOptions::default();
        let options = ductus_runtime::EngineOptions {
            pool_threads: self.threads.unwrap_or(defaults.pool_threads),
            db_connections: self.db_connections,
            env: self.env,
            api_listen: self.api_listen,
            stop_timeout: self.stop_timeout,
            ..defaults
        };
        Ok((self.root, tools::database_url(self.database_url)?, options))
    }
}

#[derive(Subcommand)]
enum Command {
    /// Start a new project: settings, environments, resources and a first interface, ready for
    /// `ductus test`. The folder must be empty (a fresh Git clone is fine).
    Init {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Run every interface in a project.
    Run(RunArgs),
    /// Run the project with a Postgres of its own, for local development: nothing to install.
    /// The first run downloads Postgres (once per user); its data lives in `.ductus/postgres`
    /// and it stops with the engine. Prints the connection URL.
    Dev {
        /// Project folder containing `interfaces/`.
        #[arg(long, env = "DUCTUS_PROJECT", default_value = ".")]
        root: PathBuf,
        /// Threads in the shared worker pool (default: one per CPU core).
        #[arg(long, env = "DUCTUS_THREADS")]
        threads: Option<usize>,
        /// Environment: variables from environments/<env>.yaml, secrets from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// Where the REST API listens (`host:port`, or `off`).
        #[arg(long, env = "DUCTUS_API_LISTEN")]
        api_listen: Option<String>,
        #[arg(long, env = "DUCTUS_STOP_TIMEOUT", default_value = "30s", value_parser = parse_duration)]
        stop_timeout: std::time::Duration,
        /// Postgres's port (default: the one kept from the last run, else a free one).
        #[arg(long)]
        port: Option<u16>,
        /// Delete this project's dev database and start again.
        #[arg(long)]
        reset: bool,
        /// Don't ask before --reset deletes it.
        #[arg(long, requires = "reset")]
        yes: bool,
        /// Start only the Postgres, without the engine, and print its URL.
        #[arg(long)]
        database_only: bool,
    },
    /// Run each interface's `*.test.ts` files.
    Test {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only this interface.
        #[arg(long)]
        interface: Option<String>,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Check configs, code loading and TypeScript types for an environment (exits 1 on problems).
    Validate {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Create something new in a project.
    New {
        #[command(subcommand)]
        what: NewCommand,
    },
    /// List the ports and HTTP routes a project listens on, the hosts it connects to, and port
    /// conflicts, for an environment (exits 1 on conflicts).
    Ports {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Which interfaces a vendor, application or host touches, how, and who owns them: for a
    /// vendor breach or a VPN/IP change. Reads `about:`, destinations, resources.yaml, and the
    /// databases and APIs code uses.
    #[command(group(clap::ArgGroup::new("what").required(true).args(["vendor", "application", "host"])))]
    Impact {
        /// e.g. "Epic Systems" (ignores case).
        #[arg(long)]
        vendor: Option<String>,
        /// e.g. "Sunquest Lab" (ignores case).
        #[arg(long)]
        application: Option<String>,
        /// An address, subnet (10.20.30.0/24) or host name.
        #[arg(long)]
        host: Option<String>,
        /// Look host names up in DNS, so names in the config match addresses (and the other way round).
        #[arg(long)]
        resolve: bool,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Check every connection the project needs, from this server: DNS, TCP (with the address
    /// a firewall request needs), TLS (certificate, expiry), then each system's own login.
    /// Explains failures in plain words. Never sends anything to a destination. Exits 1 if
    /// something failed.
    Doctor {
        /// Only what these interfaces use (repeat for several); the engine's database always.
        #[arg(long)]
        interface: Vec<String>,
        /// Longest wait per step, e.g. 5s or 500ms.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        timeout: std::time::Duration,
        /// Also save the report (for a network team or vendor): JSON if the name ends in .json.
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
    },
    /// The company-defined documentation fields (`docs-schema.yaml`) and their values (`custom:`)
    /// for every interface, destination and resource. Exits 1 if a required value is missing.
    Docs {
        /// Only what's missing.
        #[arg(long)]
        missing: bool,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Who owns which interface, destination and resources.yaml entry (`about.owner`, `owner` on
    /// a destination someone else owns, `owner` in resources.yaml), and, with a database, whether
    /// each owner is an account (username, email or display name), a team in `owners:` or nobody.
    /// `ductus owners reassign` moves ownership.
    #[command(args_conflicts_with_subcommands = true)]
    Owners {
        #[command(subcommand)]
        action: Option<OwnersAction>,
        /// Fail (exit 1) if an owner is no account, team or address, a disabled account, or names
        /// several accounts. Needs the database.
        #[arg(long)]
        check: bool,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// Where the accounts are (without it, owners aren't matched to them).
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
    },
    /// Manage the people who sign in (in the engine's database; that is how the first admin is
    /// made). Roles: viewer reads, operator also drains, resends and runs connection checks, admin
    /// also manages users and tokens. Permissions beyond a role: `payloads` (read message
    /// payloads), `audit`.
    User {
        #[command(subcommand)]
        action: UserAction,
        #[arg(long, env = "DUCTUS_DATABASE_URL", global = true)]
        database_url: Option<String>,
        /// The project folder (its ductus.yaml sets the password rules).
        #[arg(long, default_value = ".", global = true)]
        root: PathBuf,
    },
    /// Manage REST API tokens (in the engine's database). Roles: viewer reads, operator also
    /// drains and runs connection checks, admin also manages users and tokens. A token can't read
    /// message payloads unless it is given `--permission payloads`.
    Token {
        #[command(subcommand)]
        action: TokenAction,
        #[arg(long, env = "DUCTUS_DATABASE_URL", global = true)]
        database_url: Option<String>,
    },
    /// The audit log (who did what), straight from the engine's database.
    Audit {
        #[command(subcommand)]
        action: AuditAction,
        #[arg(long, env = "DUCTUS_DATABASE_URL", global = true)]
        database_url: Option<String>,
    },
    /// Stop taking new messages and deliver what's stored (for upgrades, server moves, config
    /// changes that need an empty queue): MLLP listeners close, queued HTTP answers 503, file
    /// pickup and schedules pause. Through the running engine's REST API.
    Drain {
        /// Exit the engine once nothing is pending.
        #[arg(long)]
        stop: bool,
        /// Wait until drained, showing what's left.
        #[arg(long)]
        wait: bool,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Take new messages again after `ductus drain`; or, with an interface, resume its paused
    /// destination (it sends its backlog in order).
    Resume {
        /// Resume this interface's paused destination (instead of ending a drain).
        interface: Option<String>,
        /// Which destination (needed when the interface has several).
        destination: Option<String>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Pause a destination at once: nothing is sent or retried until `ductus resume <interface>
    /// [destination]`; messages keep queueing. Act first, explain after: the reason is optional,
    /// and running it again with --reason adds one. Through the running engine's REST API.
    Pause {
        interface: String,
        /// Which destination (needed when the interface has several).
        destination: Option<String>,
        /// Why (can be added later).
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// How long each interface keeps its messages (`retention:` in interface.yaml), changes in
    /// progress, shorter values waiting for a confirmation, and expired partitions that had to
    /// stay. Through the running engine's REST API.
    Retention {
        #[command(subcommand)]
        action: Option<RetentionAction>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Where each destination's backlog sits: the lanes messages wait in (the ordering key's
    /// hash), the first message of each (key, age, attempts, last error), and which worker a
    /// failing message holds up. Through the running engine's REST API (viewer token).
    Hotspots {
        /// Only this interface.
        interface: Option<String>,
        /// Lanes to list per destination, most waiting first (default 10).
        #[arg(long)]
        lanes: Option<usize>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Alerts (`alerts:` in ductus.yaml): what is open, from the running engine's REST API (viewer
    /// token); or `ductus alerts test <channel>`, which sends a test notification straight from
    /// ductus.yaml (no engine needed: check the mail server or webhook before going live).
    Alerts {
        #[command(subcommand)]
        action: Option<AlertsAction>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Manage encrypted secrets (secrets/<env>.json; key in DUCTUS_SECRET_KEY).
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
    /// Print the REST API's OpenAPI document (the same as `GET /api/v1/openapi.json`, without a
    /// running engine).
    ApiSpec,
    /// Write editor typings (.ductus/types) and tsconfig.json for VS Code.
    Types {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Show message counts by interface and status (from the database, also while the engine is
    /// down); with a token, also each interface's and destination's health from the running
    /// engine (silent, behind, stalled, paused, catching up...).
    Status {
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Send synthetic ADT messages over MLLP.
    Gen {
        #[arg(long, default_value = "127.0.0.1:6661")]
        to: String,
        #[arg(long, default_value_t = 1000)]
        count: u64,
        /// Distinct patients (MRNs) to spread messages across.
        #[arg(long, default_value_t = 200)]
        patients: u64,
        /// Total messages per second; 0 = as fast as ACKs come back.
        #[arg(long, default_value_t = 0)]
        rate: u64,
        /// Concurrent MLLP connections. Each patient always uses the same connection.
        #[arg(long, default_value_t = 1)]
        connections: u64,
        /// Fraction of messages (0..1) for one hot patient, e.g. 0.3.
        #[arg(long, default_value_t = 0.0)]
        hot: f64,
        /// Added to every sequence number and patient number, so several generators can feed one
        /// sink without their messages looking like duplicates.
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Receive messages over MLLP, ACK them, and verify per-patient ordering. With --smtp or
    /// --http, a mail server or web server that accepts everything and prints it instead (to see
    /// what alerts send without a real one).
    Sink {
        #[arg(long, default_value_t = 6662)]
        port: u16,
        /// Be an SMTP server on this port: print each mail (and keep it in --dir).
        #[arg(long, conflicts_with = "http")]
        smtp: Option<u16>,
        /// Be an HTTP server on this port: print each request's body (and keep it in --dir).
        #[arg(long)]
        http: Option<u16>,
        /// Also write each mail or request to a file of its own here.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Answer AR (application reject) to messages that contain this text, as a receiver that
        /// refuses them would (repeat for several). The engine quarantines them.
        #[arg(long, conflicts_with_all = ["smtp", "http"])]
        reject: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AlertsAction {
    /// Send a test notification to a channel now, from ductus.yaml in this environment.
    Test {
        channel: String,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
}

#[derive(Subcommand)]
enum NewCommand {
    /// A working interface (config, code, a test and a sample message) that passes
    /// `ductus test` and `ductus validate` as generated. MLLP and HTTP get the next free port
    /// from the ranges in ductus.yaml (default 6661-6699 and 8080-8099).
    Interface {
        /// Folder name and interface name, e.g. adt-to-lab.
        name: String,
        #[arg(long, value_enum, default_value = "mllp")]
        source: new::SourceKind,
        /// Default: hl7 (json for http and schedule).
        #[arg(long, value_enum)]
        format: Option<new::FormatKind>,
        /// Listen on this port instead of the next free one.
        #[arg(long)]
        port: Option<u16>,
        /// SFTP server from resources.yaml (needed when there are several).
        #[arg(long)]
        server: Option<String>,
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Subcommand)]
enum OwnersAction {
    /// Change an owner in interface.yaml files and resources.yaml (someone left, or work is split
    /// up): only the `owner:` values change, comments and layout stay. To split, run it per group
    /// with --interface and --resource.
    Reassign {
        /// The current owner (ignores case).
        #[arg(long)]
        from: String,
        /// The new owner.
        #[arg(long)]
        to: String,
        /// Only these interfaces (repeat for several). With neither this nor --resource,
        /// everything of `--from` moves.
        #[arg(long)]
        interface: Vec<String>,
        /// Only these resources.yaml entries (repeat for several).
        #[arg(long)]
        resource: Vec<String>,
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
}

#[derive(clap::Args)]
struct ApiArgs {
    /// The engine's REST API.
    #[arg(long, env = "DUCTUS_API_URL", default_value = "http://127.0.0.1:7700")]
    api: String,
    /// An operator token (`ductus token create <name> --role operator`).
    #[arg(long, env = "DUCTUS_API_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

#[derive(Subcommand)]
enum RetentionAction {
    /// Apply an interface's shorter retention: it deletes the messages past it (operator token).
    Confirm { interface: String },
}

#[derive(Subcommand)]
enum AuditAction {
    /// Print the log as one JSON object per line, oldest first (for a SIEM or an auditor). The
    /// export is itself logged.
    Export {
        /// From this day (`2026-01-31`, UTC) or time (`2026-01-31T08:00:00Z`).
        #[arg(long)]
        since: Option<String>,
        /// Until this day or time (exclusive).
        #[arg(long)]
        until: Option<String>,
        /// A user's or token's name.
        #[arg(long)]
        actor: Option<String>,
        /// An action (`message.dismiss`) or a group (`message`, `user`, `session`).
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        interface: Option<String>,
        /// A message id.
        #[arg(long)]
        message: Option<i64>,
    },
}

#[derive(Subcommand)]
enum TokenAction {
    /// Create a token and print it (only this once: it's stored as a hash).
    Create {
        /// Who or what uses it, e.g. `ci` or `monitoring`.
        name: String,
        /// viewer, operator or admin.
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Beyond the role: `payloads` (read message payloads), `audit`. Repeat for both.
        #[arg(long = "permission")]
        permissions: Vec<String>,
    },
    /// List tokens (never their secrets).
    List {
        /// Include revoked tokens.
        #[arg(long)]
        revoked: bool,
    },
    /// Revoke a token: it stops working at once.
    Revoke { name: String },
}

#[derive(Subcommand)]
enum UserAction {
    /// Make an account. The password is asked for twice (or read from stdin with --password-stdin).
    Create {
        username: String,
        /// viewer, operator or admin.
        #[arg(long, default_value = "viewer")]
        role: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        email: Option<String>,
        /// Beyond the role: `payloads`, `audit`. Repeat for both.
        #[arg(long = "permission")]
        permissions: Vec<String>,
        /// Read the password from stdin (one line) instead of asking.
        #[arg(long)]
        password_stdin: bool,
        /// The person must choose their own password at their first sign-in.
        #[arg(long)]
        must_change: bool,
    },
    /// List users.
    List,
    /// Change a user's role and permissions (all of them are set to what is given).
    Set {
        username: String,
        #[arg(long)]
        role: Option<String>,
        /// The permissions the user has afterwards (none given: leave them as they are).
        #[arg(long = "permission")]
        permissions: Vec<String>,
    },
    /// Give a user a new temporary password (they must change it at their next sign-in).
    Password {
        username: String,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Stop a user signing in; their sessions and tokens stop at once.
    Disable { username: String },
    /// Let a disabled user sign in again.
    Enable { username: String },
    /// Let a user locked out by wrong passwords try again.
    Unlock { username: String },
}

#[derive(Subcommand)]
enum SecretAction {
    /// Print a new random key to put in DUCTUS_SECRET_KEY (keep it out of Git).
    Init,
    /// Create or update a secret. The value is read from stdin if not given.
    Set {
        name: String,
        value: Option<String>,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// List secret names (never values).
    List {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
}

fn action_args(action: SecretAction) -> project::SecretCommand {
    match action {
        SecretAction::Init => project::SecretCommand::Init,
        SecretAction::Set { name, value, root, env } => project::SecretCommand::Set { name, value, root, env },
        SecretAction::List { root, env } => project::SecretCommand::List { root, env },
    }
}

/// Let a report piped into `head` or `grep -q` end quietly when the reader stops, like other
/// Unix tools, instead of panicking on the next print. Rust ignores SIGPIPE so that a closed
/// socket is an error rather than death; that's what the engine needs, so this is only for
/// commands that print a report, never `run`, `gen` or `sink`.
fn quiet_on_closed_pipe() {
    #[cfg(unix)]
    // SAFETY: called at startup, before any other thread exists; SIG_DFL is always valid.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn parse_duration(text: &str) -> Result<std::time::Duration> {
    ductus_core::config::parse_duration(text)
}

/// For commands that report problems themselves: exit 1 when they found any.
fn exit_on_failure(result: Result<bool>) -> Result<()> {
    if !result? {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    if let Some(file) = envfile::requested() {
        envfile::load(&file)?;
    }
    let cli = Cli::parse();
    if let Command::Run(args) = &cli.command
        && args.service
    {
        #[cfg(windows)]
        return service::run(args.clone());
        #[cfg(not(windows))]
        anyhow::bail!("--service is for the Windows service; systemd runs `ductus run` itself");
    }
    if !matches!(cli.command, Command::Run(_) | Command::Dev { .. } | Command::Gen { .. } | Command::Sink { .. }) {
        quiet_on_closed_pipe();
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        // Plain text when logging to a file or pipe (no color codes in log files).
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Doctor's report says what failed; the TLS verifier would log each failure again.
            if matches!(cli.command, Command::Doctor { .. }) {
                "warn,rustls_platform_verifier=off".into()
            } else {
                // A certificate the platform verifier refuses is logged by it at ERROR every time,
                // and said again where it matters (a delivery's error, a check's result): the
                // engine checks certificates every few hours, private CAs included.
                "info,sqlx=warn,rustls_platform_verifier=off".into()
            }
        }))
        .init();

    match cli.command {
        Command::Run(args) => {
            let (root, database_url, options) = args.parts()?;
            ductus_runtime::run(&root, &database_url, options).await
        }
        Command::Dev { root, threads, env, api_listen, stop_timeout, port, reset, yes, database_only } => {
            let defaults = ductus_runtime::EngineOptions::default();
            let engine = ductus_runtime::EngineOptions {
                pool_threads: threads.unwrap_or(defaults.pool_threads),
                env,
                api_listen,
                stop_timeout,
                ..defaults
            };
            dev::run(dev::Options { root, engine, port, reset, yes, database_only }).await
        }
        Command::Test { root, interface, env } => {
            exit_on_failure(project::test(&root, &env, interface.as_deref()).await)
        }
        Command::Validate { root, env } => exit_on_failure(project::validate(&root, &env).await),
        Command::Init { dir } => init::project(&dir).await,
        Command::New { what: NewCommand::Interface { name, source, format, port, server, root } } => {
            new::interface(new::NewInterface { root, name, source, format, port, server })
        }
        Command::Ports { root, env } => exit_on_failure(project::ports(&root, &env)),
        Command::Impact { vendor, application, host, resolve, root, env } => {
            use ductus_core::impact::Query;
            let query = match (vendor, application, host) {
                (Some(v), _, _) => Query::Vendor(v),
                (_, Some(a), _) => Query::Application(a),
                (_, _, Some(h)) => Query::Host(ductus_core::about::HostQuery::parse(&h)?),
                _ => unreachable!("clap requires one"),
            };
            exit_on_failure(project::impact(&root, &env, query, resolve).await)
        }
        Command::Doctor { interface, timeout, report, root, env, database_url } => {
            let options = doctor::Options {
                root,
                env,
                database_url: tools::database_url(database_url)?,
                interfaces: interface,
                timeout,
                report,
            };
            exit_on_failure(doctor::run(options).await)
        }
        Command::Docs { missing, json, root, env } => exit_on_failure(project::docs(&root, &env, missing, json)),
        Command::Owners { action: None, check, root, env, database_url } => {
            exit_on_failure(owners::report(&root, &env, database_url, check).await)
        }
        Command::Owners {
            action: Some(OwnersAction::Reassign { from, to, interface, resource, dry_run, root, env }),
            ..
        } => exit_on_failure(owners::reassign(&root, &env, &from, &to, &interface, &resource, dry_run)),
        Command::User { action, database_url, root } => {
            let command = match action {
                UserAction::Create {
                    username,
                    role,
                    display_name,
                    email,
                    permissions,
                    password_stdin,
                    must_change,
                } => admin::UserCommand::Create {
                    username,
                    role,
                    display_name,
                    email,
                    permissions,
                    password_stdin,
                    must_change,
                },
                UserAction::List => admin::UserCommand::List,
                UserAction::Set { username, role, permissions } => {
                    admin::UserCommand::Set { username, role, permissions }
                }
                UserAction::Password { username, password_stdin } => {
                    admin::UserCommand::Password { username, password_stdin }
                }
                UserAction::Disable { username } => admin::UserCommand::Disable { username, disabled: true },
                UserAction::Enable { username } => admin::UserCommand::Disable { username, disabled: false },
                UserAction::Unlock { username } => admin::UserCommand::Unlock { username },
            };
            admin::user(&tools::database_url(database_url)?, &root, command).await
        }
        Command::Token { action, database_url } => {
            let command = match action {
                TokenAction::Create { name, role, permissions } => {
                    admin::TokenCommand::Create { name, role, permissions }
                }
                TokenAction::List { revoked } => admin::TokenCommand::List { revoked },
                TokenAction::Revoke { name } => admin::TokenCommand::Revoke { name },
            };
            admin::token(&tools::database_url(database_url)?, command).await
        }
        Command::Audit {
            action: AuditAction::Export { since, until, actor, action, interface, message },
            database_url,
        } => {
            let export = admin::AuditExport { since, until, actor, action, interface, message };
            admin::audit_export(&tools::database_url(database_url)?, export).await
        }
        Command::Drain { stop, wait, api } => admin::drain(admin::Remote::new(&api.api, api.token)?, stop, wait).await,
        Command::Resume { interface: None, api, .. } => admin::resume(admin::Remote::new(&api.api, api.token)?).await,
        Command::Resume { interface: Some(interface), destination, api } => {
            admin::unpause(admin::Remote::new(&api.api, api.token)?, &interface, destination.as_deref()).await
        }
        Command::Pause { interface, destination, reason, api } => {
            let remote = admin::Remote::new(&api.api, api.token)?;
            admin::pause(remote, &interface, destination.as_deref(), reason.as_deref()).await
        }
        Command::Retention { action, api } => {
            let remote = admin::Remote::new(&api.api, api.token)?;
            match action {
                None => admin::retention(remote).await,
                Some(RetentionAction::Confirm { interface }) => admin::confirm_retention(remote, &interface).await,
            }
        }
        Command::Hotspots { interface, lanes, api } => {
            admin::hotspots(admin::Remote::new(&api.api, api.token)?, interface.as_deref(), lanes).await
        }
        Command::Alerts { action: None, api } => admin::alerts(admin::Remote::new(&api.api, api.token)?).await,
        Command::Alerts { action: Some(AlertsAction::Test { channel, root, env }), .. } => {
            project::test_alert(&root, &env, &channel).await
        }
        Command::Secret { action } => project::secret(action_args(action)),
        Command::Types { root } => project::types(&root).await,
        Command::ApiSpec => {
            println!("{}", serde_json::to_string_pretty(&ductus_runtime::api::openapi())?);
            Ok(())
        }
        Command::Status { database_url, api } => {
            tools::status(&tools::database_url(database_url)?).await?;
            if api.token.as_deref().is_some_and(|t| !t.trim().is_empty()) {
                println!("\nhealth:");
                match admin::health(&admin::Remote::new(&api.api, api.token)?).await {
                    Ok(text) => print!("{text}"),
                    Err(e) => println!("(not from the engine: {e:#})"),
                }
            }
            Ok(())
        }
        Command::Gen { to, count, patients, rate, connections, hot, offset } => {
            tools::generate(tools::GenOptions { to, count, patients, rate, connections, hot, offset }).await
        }
        Command::Sink { smtp: Some(port), dir, .. } => sinks::smtp(port, dir).await,
        Command::Sink { http: Some(port), dir, .. } => sinks::http(port, dir).await,
        Command::Sink { port, reject, .. } => tools::sink(port, reject).await,
    }
}
