mod admin;
mod doctor;
mod new;
mod owners;
mod project;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ductus", version, about = "Code-first healthcare integration engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run every interface in a project.
    Run {
        /// Project folder containing `interfaces/`.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_DATABASE_URL", default_value = tools::DEFAULT_DB)]
        database_url: String,
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
        #[arg(long, env = "DUCTUS_DATABASE_URL", default_value = tools::DEFAULT_DB)]
        database_url: String,
    },
    /// Who owns which interface and destination (`about.owner`, and `owner` on a destination
    /// someone else owns). `ductus owners reassign` moves ownership.
    #[command(args_conflicts_with_subcommands = true)]
    Owners {
        #[command(subcommand)]
        action: Option<OwnersAction>,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Manage REST API tokens (in the engine's database). Roles: viewer reads, operator also
    /// drains and runs connection checks, admin also manages tokens.
    Token {
        #[command(subcommand)]
        action: TokenAction,
        #[arg(long, env = "DUCTUS_DATABASE_URL", default_value = tools::DEFAULT_DB, global = true)]
        database_url: String,
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
    /// Take new messages again after `ductus drain`.
    Resume {
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
    /// Manage encrypted secrets (secrets/<env>.json; key in DUCTUS_SECRET_KEY).
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
    /// Write editor typings (.ductus/types) and tsconfig.json for VS Code.
    Types {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Show message counts by interface and status.
    Status {
        #[arg(long, env = "DUCTUS_DATABASE_URL", default_value = tools::DEFAULT_DB)]
        database_url: String,
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
    /// Receive messages over MLLP, ACK them, and verify per-patient ordering.
    Sink {
        #[arg(long, default_value_t = 6662)]
        port: u16,
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
    /// Change an owner in interface.yaml files (someone left, or work is split up): only the
    /// `owner:` values change, comments and layout stay. To split, run it per group of
    /// interfaces with --interface.
    Reassign {
        /// The current owner (ignores case).
        #[arg(long)]
        from: String,
        /// The new owner.
        #[arg(long)]
        to: String,
        /// Only these interfaces (repeat for several).
        #[arg(long)]
        interface: Vec<String>,
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
enum TokenAction {
    /// Create a token and print it (only this once: it's stored as a hash).
    Create {
        /// Who or what uses it, e.g. `ci` or `monitoring`.
        name: String,
        /// viewer, operator or admin.
        #[arg(long, default_value = "viewer")]
        role: String,
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
    let cli = Cli::parse();
    if !matches!(cli.command, Command::Run { .. } | Command::Gen { .. } | Command::Sink { .. }) {
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
                "info,sqlx=warn".into()
            }
        }))
        .init();

    match cli.command {
        Command::Run { root, database_url, threads, db_connections, env, api_listen } => {
            let defaults = ductus_runtime::EngineOptions::default();
            let options = ductus_runtime::EngineOptions {
                pool_threads: threads.unwrap_or(defaults.pool_threads),
                db_connections,
                env,
                api_listen,
            };
            ductus_runtime::run(&root, &database_url, options).await
        }
        Command::Test { root, interface, env } => {
            exit_on_failure(project::test(&root, &env, interface.as_deref()).await)
        }
        Command::Validate { root, env } => exit_on_failure(project::validate(&root, &env).await),
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
            let options = doctor::Options { root, env, database_url, interfaces: interface, timeout, report };
            exit_on_failure(doctor::run(options).await)
        }
        Command::Owners { action: None, root, env } => exit_on_failure(owners::report(&root, &env)),
        Command::Owners {
            action: Some(OwnersAction::Reassign { from, to, interface, dry_run, root, env }), ..
        } => exit_on_failure(owners::reassign(&root, &env, &from, &to, &interface, dry_run)),
        Command::Token { action, database_url } => {
            let command = match action {
                TokenAction::Create { name, role } => admin::TokenCommand::Create { name, role },
                TokenAction::List { revoked } => admin::TokenCommand::List { revoked },
                TokenAction::Revoke { name } => admin::TokenCommand::Revoke { name },
            };
            admin::token(&database_url, command).await
        }
        Command::Drain { stop, wait, api } => admin::drain(admin::Remote::new(&api.api, api.token)?, stop, wait).await,
        Command::Resume { api } => admin::resume(admin::Remote::new(&api.api, api.token)?).await,
        Command::Retention { action, api } => {
            let remote = admin::Remote::new(&api.api, api.token)?;
            match action {
                None => admin::retention(remote).await,
                Some(RetentionAction::Confirm { interface }) => admin::confirm_retention(remote, &interface).await,
            }
        }
        Command::Secret { action } => project::secret(action_args(action)),
        Command::Types { root } => project::types(&root).await,
        Command::Status { database_url } => tools::status(&database_url).await,
        Command::Gen { to, count, patients, rate, connections, hot, offset } => {
            tools::generate(tools::GenOptions { to, count, patients, rate, connections, hot, offset }).await
        }
        Command::Sink { port } => tools::sink(port).await,
    }
}
