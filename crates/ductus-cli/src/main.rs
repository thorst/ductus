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
    /// List the ports and HTTP routes a project listens on, the hosts it connects to, and port
    /// conflicts, for an environment (exits 1 on conflicts).
    Ports {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
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

/// For commands that report problems themselves: exit 1 when they found any.
fn exit_on_failure(result: Result<bool>) -> Result<()> {
    if !result? {
        std::process::exit(1);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        // Plain text when logging to a file or pipe (no color codes in log files).
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Run { root, database_url, threads, db_connections, env } => {
            let defaults = ductus_runtime::EngineOptions::default();
            let options = ductus_runtime::EngineOptions {
                pool_threads: threads.unwrap_or(defaults.pool_threads),
                db_connections,
                env,
            };
            ductus_runtime::run(&root, &database_url, options).await
        }
        Command::Test { root, interface, env } => {
            exit_on_failure(project::test(&root, &env, interface.as_deref()).await)
        }
        Command::Validate { root, env } => exit_on_failure(project::validate(&root, &env).await),
        Command::Ports { root, env } => exit_on_failure(project::ports(&root, &env)),
        Command::Secret { action } => project::secret(action_args(action)),
        Command::Types { root } => project::types(&root).await,
        Command::Status { database_url } => tools::status(&database_url).await,
        Command::Gen { to, count, patients, rate, connections, hot, offset } => {
            tools::generate(tools::GenOptions { to, count, patients, rate, connections, hot, offset }).await
        }
        Command::Sink { port } => tools::sink(port).await,
    }
}
