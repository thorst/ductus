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
    },
    /// Run each interface's `*.test.ts` files.
    Test {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only this interface.
        #[arg(long)]
        interface: Option<String>,
    },
    /// Check configs, code loading and TypeScript types.
    Validate {
        #[arg(long, default_value = ".")]
        root: PathBuf,
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
        /// Messages per second; 0 = as fast as ACKs come back.
        #[arg(long, default_value_t = 0)]
        rate: u64,
    },
    /// Receive messages over MLLP, ACK them, and verify per-patient ordering.
    Sink {
        #[arg(long, default_value_t = 6662)]
        port: u16,
    },
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Run { root, database_url } => ductus_runtime::run(&root, &database_url).await,
        Command::Test { root, interface } => exit_on_failure(project::test(&root, interface.as_deref()).await),
        Command::Validate { root } => exit_on_failure(project::validate(&root).await),
        Command::Types { root } => project::types(&root).await,
        Command::Status { database_url } => tools::status(&database_url).await,
        Command::Gen { to, count, patients, rate } => tools::generate(&to, count, patients, rate).await,
        Command::Sink { port } => tools::sink(port).await,
    }
}
