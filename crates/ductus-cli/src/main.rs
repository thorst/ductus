mod admin;
mod cert;
mod deploy;
mod dev;
mod doctor;
mod envfile;
mod findings;
mod history;
mod init;
mod new;
mod owners;
mod project;
mod reference;
mod release;
mod replay;
mod service;
mod sinks;
mod spread;
mod tools;
mod trying;
mod upgrade;

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
    /// Project folder containing `interfaces/` (folder mode; default: the current folder).
    #[arg(long, env = "DUCTUS_PROJECT")]
    root: Option<PathBuf>,
    /// Release mode: run the release deployed to this environment (`ductus deploy`), unpacked
    /// from the database into this folder, instead of a project folder. Not with `--root`
    /// (DUCTUS_PROJECT). Example: `/var/lib/ductus/releases`.
    #[arg(long, env = "DUCTUS_RELEASES", value_name = "FOLDER")]
    releases: Option<PathBuf>,
    /// Release mode: the project this engine runs, when several are deployed to its environment on
    /// one database (default: the only one).
    #[arg(long, env = "DUCTUS_RELEASE_PROJECT", value_name = "NAME", requires = "releases")]
    release_project: Option<String>,
    /// Release mode: how long a deploy may reuse a passing check of the same release here instead
    /// of running the gate again (only when nothing was deployed since, by the same Ductus
    /// version); `0` runs the gate at every deploy.
    #[arg(long, env = "DUCTUS_DEPLOY_REUSE_CHECK", default_value = "1h", value_parser = parse_duration)]
    deploy_reuse_check: std::time::Duration,
    /// Release mode: environments a release must have been deployed to before this one takes it
    /// (`test`, or `test,uat`), read from the record the release carries (`ductus deploy --from`
    /// brings it) and this database's deployments. `--force` doesn't skip it; a rollback (a
    /// release that ran here before) does. The record is unsigned: see the releases guide.
    #[arg(long, env = "DUCTUS_DEPLOY_REQUIRES", value_name = "ENVIRONMENTS", value_delimiter = ',')]
    deploy_requires: Vec<String>,
    /// Release mode: the branches a release may be made on to be deployed here (`main`, or
    /// `main,release/*`: a trailing `*` matches the start). A release made on another branch, on
    /// none (a detached HEAD without `ductus release --branch`) or without git is refused.
    /// `--force` doesn't skip it; a rollback does. The branch is a label the release carries,
    /// unsigned: the releases guide says what to rely on.
    #[arg(long, env = "DUCTUS_DEPLOY_BRANCHES", value_name = "BRANCHES", value_delimiter = ',')]
    deploy_branches: Vec<String>,
    /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
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
    /// Take over from the running engine that started this one: its handover (`ductus
    /// handover`) passes the descriptor. Never by hand.
    #[arg(long, env = "DUCTUS_TAKEOVER", hide = true)]
    takeover: Option<i32>,
}

impl RunArgs {
    /// Where the project is, which database, and the engine's settings.
    fn parts(self) -> Result<(PathBuf, String, ductus_runtime::EngineOptions)> {
        let defaults = ductus_runtime::EngineOptions::default();
        if let (Some(project), Some(_)) = (&self.root, &self.releases) {
            anyhow::bail!(
                "both a project folder ({}, `--root` or DUCTUS_PROJECT) and release mode (`--releases` or DUCTUS_RELEASES) \
                 are set: remove DUCTUS_PROJECT to run releases, or DUCTUS_RELEASES to run the folder",
                project.display()
            );
        }
        let names = |list: Vec<String>| -> Vec<String> {
            list.into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect()
        };
        let releases = self.releases.map(|dir| ductus_runtime::deploy::Mode {
            dir,
            project: self.release_project,
            reuse_check: self.deploy_reuse_check,
            requires: names(self.deploy_requires),
            branches: names(self.deploy_branches),
        });
        let options = ductus_runtime::EngineOptions {
            pool_threads: self.threads.unwrap_or(defaults.pool_threads),
            db_connections: self.db_connections,
            env: self.env,
            api_listen: self.api_listen,
            stop_timeout: self.stop_timeout,
            control: true,
            rebuild: Some(Self::rebuild()),
            takeover: self.takeover.map(ductus_runtime::handover::Takeover::new).transpose()?,
            releases,
            ..defaults
        };
        Ok((self.root.unwrap_or_else(|| PathBuf::from(".")), tools::database_url(self.database_url)?, options))
    }

    /// How an engine started again in this process gets its settings (an in-process handover: a
    /// deploy or reload on Windows, in a container, under a plain `ductus run`): `run`'s arguments
    /// read again, with the env file as it is now (`envfile::run_args_again`).
    fn rebuild() -> ductus_runtime::handover::Rebuild {
        std::sync::Arc::new(|| {
            let mut args = envfile::run_args_again()?;
            args.takeover = None; // the handover gives the new engine its own
            let (root, database_url, options) = args.parts()?;
            Ok(ductus_runtime::handover::Restart { root, database_url, options })
        })
    }
}

#[derive(Subcommand)]
enum Command {
    /// Start a new project: settings, environments, resources and a first interface, ready for
    /// `ductus test`. The folder must be empty (a fresh Git clone is fine).
    Init {
        /// The folder to make the project in (made if missing).
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
        /// On Ctrl-C, how long to finish the messages in hand before exiting anyway.
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
        #[arg(long, conflicts_with = "inspect")]
        database_only: bool,
        /// Let a debugger attach to this interface's code (its name, or a file or folder inside
        /// its folder; again for another). It runs on a thread of its own for the run, so while
        /// it's paused at a breakpoint (or looping under the debugger) only its messages wait:
        /// receiving, ACKs and every other interface go on. No time limit while a debugger is
        /// attached; it applies again when it detaches. The debugging guide shows VS Code's and
        /// Chrome's setup.
        #[arg(long, value_name = "INTERFACE")]
        inspect: Vec<String>,
        /// Where the debugger listens with `--inspect`: a port, or this machine's address and a
        /// port (never another machine's).
        #[arg(long, value_name = "ADDRESS", default_value = ductus_runtime::inspect::DEFAULT, requires = "inspect")]
        inspect_address: String,
    },
    /// Run each interface's `*.test.ts` files.
    ///
    /// Each test runs inside the engine's own JavaScript runtime with the SDK it uses, one at a
    /// time, with no database, network or engine. A failed test prints where it is, where it failed
    /// (the `expect`'s line, or the stack of what was thrown) and why; exits 1 when one fails, or
    /// when `--filter` matches no test.
    Test {
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only this interface: its name, or a file or folder inside its folder
        /// (`interfaces/adt-in/route.test.ts`), as an editor passes the open file. Again for
        /// another (`--interface adt-in --interface lab-out`).
        #[arg(long)]
        interface: Vec<String>,
        /// Only the tests whose name contains this text (case matters): `--filter "A31"`.
        #[arg(long)]
        filter: Option<String>,
        /// List the tests, with the file and line of each, without running them.
        #[arg(long)]
        list: bool,
        /// Run the tests under a debugger (VS Code, Chrome DevTools, anything that speaks the
        /// Chrome DevTools Protocol): listen on 127.0.0.1:9229 (or the address given, on this
        /// machine only), wait for one to attach, then run one interface's tests (`--interface`,
        /// or the only one with tests). Breakpoints and `debugger;` statements pause there; the
        /// time limit is off while it's attached. The debugging guide shows VS Code's and
        /// Chrome's setup.
        #[arg(long, value_name = "ADDRESS", num_args = 0..=1, require_equals = true, default_missing_value = "127.0.0.1:9229")]
        inspect: Option<String>,
        /// JSON Lines for tools (an editor, CI): one object per test (its interface, name, file,
        /// line, column, and when run `ok`, `ms`, the error, where it failed and the expected and
        /// actual values), then a summary. The testing guide describes every field.
        #[arg(long)]
        json: bool,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// Write nothing into the project (no editor typings): for `ductus upgrade --check`.
        #[arg(long, hide = true)]
        read_only: bool,
    },
    /// Run one message through an interface's code, every destination, and show what each would
    /// do with it now: sent (and what), filtered and why, quarantined and why. Sends nothing and
    /// writes nothing: lookups (`db().query`, GET requests) are made, writes (`db().execute`, other
    /// requests) are refused and reported. The message comes from a file, a fixture, or the store
    /// (`--message`), then with what each destination did with it then, and, when other code decided
    /// that, which code version did (the first line names the version tried). With `--inspect`,
    /// under a debugger, as `ductus test --inspect`.
    Try {
        /// The interface: its name, or a file or folder inside its folder.
        interface: String,
        /// A file holding the message as the sender sends it.
        file: Option<PathBuf>,
        /// A file in the interface's `fixtures/` folder instead.
        #[arg(long, conflicts_with = "file")]
        fixture: Option<String>,
        /// A stored message, by id: read from the database (DUCTUS_DATABASE_URL), or from the
        /// running engine at `--from` through its API.
        #[arg(long, conflicts_with_all = ["file", "fixture"])]
        message: Option<i64>,
        /// With `--message`: the engine's REST API to read it from (`https://ductus-prod:7700`):
        /// a token with the `payloads` permission (DUCTUS_API_TOKEN), and the read is in that
        /// engine's audit log.
        #[arg(long, requires = "message")]
        from: Option<String>,
        /// Only this destination's code (by its name in interface.yaml), when the others don't matter
        /// or aren't ready: the rest aren't run.
        #[arg(long)]
        destination: Option<String>,
        /// The token for `--from`.
        #[arg(long, env = "DUCTUS_API_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// With `--message` and no `--from`: the database.
        #[arg(long, env = "DUCTUS_DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment whose values and resources the code uses (lookups go to its databases).
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// Under a debugger, as `ductus test --inspect`: listen on 127.0.0.1:9229 (or the address
        /// given, on this machine only) and wait for one before the code loads.
        #[arg(long, value_name = "ADDRESS", num_args = 0..=1, require_equals = true, default_missing_value = "127.0.0.1:9229")]
        inspect: Option<String>,
        /// One JSON document: each destination's decision, output, console lines and what it
        /// recorded then.
        #[arg(long)]
        json: bool,
    },
    /// One timeline of who changed an interface, what and why, newest first: the git commits that
    /// changed its files (its interface.yaml, its code and the shared files it imports, every
    /// version's), the deployments that changed its code (who, why, from which commit, and a ⚠
    /// when one may have undone someone else's change), the audit log's entries naming it (pauses,
    /// resends, retention) and its documentation's changes. Each line ends with the parts it touches
    /// (`[source, lab]`); `--source` or `--destination NAME` shows only one part's. Reads the
    /// project and, with DUCTUS_DATABASE_URL, the database (read-only); without it, git only.
    History {
        /// The interface: its name, or a file or folder inside its folder. One no longer in the
        /// project is found by name in what was recorded.
        interface: String,
        /// Only what touches its source: `source:`, `format`, `ordering`, `dedupe`, `durability`,
        /// `about.from`, `monitor`'s times (and an HTTP `respond` interface's code).
        #[arg(long, conflicts_with = "destination")]
        source: bool,
        /// Only what touches this destination: its block in interface.yaml, the interface's
        /// settings it has none of its own for, the files its code loads (a shared file touches
        /// each destination that loads it), its pauses, resends and alerts, its documentation.
        #[arg(long, value_name = "NAME")]
        destination: Option<String>,
        /// Everything: also the starts that ran it unchanged, payload reads and refused attempts.
        #[arg(long)]
        all: bool,
        /// How many entries, newest first.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Entries older than this one: the `--before` the previous page ends with.
        #[arg(long)]
        before: Option<String>,
        /// One JSON document: every entry with its time, kind, who, what, and its details (the
        /// commit and files, the deployment, the audit row, the documentation fields changed).
        #[arg(long)]
        json: bool,
        /// The database (read-only) for deployments, the audit log and documentation; git only
        /// without it.
        #[arg(long, env = "DUCTUS_DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment its interface.yaml is read in.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// Check configs, code loading and TypeScript types for an environment (exits 1 on problems).
    Validate {
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// Also check the project against this database, reading only: lane changes the next start
        /// would make, retention changes still running or waiting for a confirmation.
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
        /// One JSON document for tools (an editor, CI) instead of the text: every finding with its
        /// severity, code (reference/validate.md), file, line and column, whether TypeScript or
        /// Ductus found it, and a fix where there is one.
        #[arg(long)]
        json: bool,
        /// Write nothing into the project (no editor typings): for `ductus upgrade --check`.
        #[arg(long, hide = true)]
        read_only: bool,
    },
    /// Before an upgrade, run with the new version's binary (from the portable archive, on the
    /// server or beside it), reading only: the migrations its start would apply and how big the
    /// tables they work through are, the engines on the database and their versions, the project
    /// validated against the database and its tests run with the new SDK, deprecated settings in
    /// use. Exits 1 on anything that would refuse the start or fail the project. A read-only login
    /// is enough (`pg_read_all_data`).
    Upgrade {
        /// Check, change nothing (the only mode for now).
        #[arg(long, required = true)]
        check: bool,
        /// One JSON document instead of the report.
        #[arg(long)]
        json: bool,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, env = "DUCTUS_PROJECT", default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
        /// Also replay this much of the latest traffic (`1d`, `12h`) through the new version and
        /// compare it with the installed one (`ductus replay`).
        #[arg(long, value_name = "DURATION")]
        replay: Option<String>,
    },
    /// Make a release of the project: one file (a .tar.gz) holding exactly what HEAD's commit has
    /// (the project's folder of it), every interface's code version, the project version, who
    /// made it, why, from which commit (with its whole history) and the tests' result. Its id is
    /// a hash of the files it carries, so the same commit gives the same id anywhere. Refuses files
    /// that aren't committed (unless `--uncommitted`), a private key, and code imported from
    /// outside the project; runs the tests first (on what the release carries) and refuses when one
    /// fails. Writes `.ductus/releases/<project>-<id>.tar.gz`. `ductus release check` is the gate
    /// before a deploy.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Release {
        #[command(subcommand)]
        action: Option<ReleaseAction>,
        #[command(flatten)]
        make: ReleaseArgs,
    },
    /// Deploy a release to an engine in release mode (`ductus run --releases`): the engine holds
    /// it against what runs there (the "replaces" check: refused when it would undo someone
    /// else's work), runs the gate (`ductus release check`) against its environment unless a
    /// recent passing check can be reused, then hands over to an engine running it: receiving
    /// never stops. Exits 1 when it wasn't deployed. Needs a token with the `deploy` permission.
    /// A file deployed gets the record of where it went written into it (its id unchanged), which
    /// an environment requiring this one first reads (DUCTUS_DEPLOY_REQUIRES). Promote from one
    /// engine to the next with `--from`; go back with `--rollback`. Example: `ductus deploy
    /// .ductus/releases/integrations-0a1b2c3d4e5f.tar.gz --to https://ductus-test:7700 -m "lab
    /// wants PV1-3 mapped"`, then `ductus deploy 0a1b2c3d4e5f --from https://ductus-test:7700 --to
    /// https://ductus-prod:7700 -m "tested in test"`.
    Deploy {
        /// The release: a file `ductus release` wrote (stored on the engine first when it isn't),
        /// or the id of a release stored there (its first 12 characters are enough). With
        /// `--rollback`, optional: the last release run there before the current one.
        #[arg(required_unless_present = "rollback")]
        release: Option<String>,
        /// The engine's REST API.
        #[arg(long, env = "DUCTUS_API_URL", default_value = "http://127.0.0.1:7700")]
        to: String,
        /// A token with the `deploy` permission (`ductus token create ci --role operator
        /// --permission deploy`).
        #[arg(long, env = "DUCTUS_API_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Promote: fetch the release (by its id) from this engine's API with its record of where
        /// it was deployed (`GET /releases/{id}/file`), store it on `--to`'s engine, and deploy it
        /// there. Example: `--from https://ductus-test:7700`.
        #[arg(long, value_name = "URL", conflicts_with = "rollback")]
        from: Option<String>,
        /// The token for `--from` (its engine's: a token with the `deploy` permission there);
        /// default: `--token`'s.
        #[arg(long, env = "DUCTUS_FROM_TOKEN", hide_env_values = true, requires = "from")]
        from_token: Option<String>,
        /// Roll back to a release that ran on that engine's environment before: the one named, or
        /// the last one before the current run (skipping any a rollback left). Not held to the
        /// environment's DUCTUS_DEPLOY_REQUIRES or DUCTUS_DEPLOY_BRANCHES; names whose newer work it
        /// undoes without refusing; the gate runs. Needs `-m`, except to `dev`.
        #[arg(long)]
        rollback: bool,
        /// Why: required, except to an environment named `dev`; recorded with the deployment,
        /// in its history and in the audit log.
        #[arg(short = 'm', long = "message")]
        reason: Option<String>,
        /// Deploy over someone else's work the "replaces" check names: their change is undone
        /// (recorded in the audit log as `release.force`, and in the history). Needs `-m`. Doesn't
        /// skip the environment's DUCTUS_DEPLOY_REQUIRES or DUCTUS_DEPLOY_BRANCHES.
        #[arg(long, conflicts_with = "rollback")]
        force: bool,
        /// Run every interface's tests in the gate, not only those of what the release changes
        /// there (never reuses a check).
        #[arg(long)]
        all_tests: bool,
        /// The engine's report as JSON.
        #[arg(long)]
        json: bool,
        /// How long to wait for the engine's answer (the gate, up to 30 minutes, then the
        /// handover).
        #[arg(long, default_value = "40m", value_parser = parse_duration)]
        timeout: std::time::Duration,
    },
    /// Run a period's messages through this version's code again, sending nothing, and compare what
    /// each destination's code decides (sent and what, filtered, quarantined) with the installed
    /// version run beside it, with what was recorded, and with kept outputs (`save_output`): the
    /// evidence a change board asks for before an upgrade or a code change. Reads only (a read-only
    /// login or a replica is enough). Exits 1 when anything differs.
    Replay {
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, env = "DUCTUS_PROJECT", default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
        /// The period's start: a duration back from its end (`24h`, `7d`) or a time (RFC 3339).
        #[arg(long, default_value = "24h")]
        since: String,
        /// The period's end, a time (RFC 3339); default now.
        #[arg(long)]
        until: Option<String>,
        /// Only this interface (repeat it for more).
        #[arg(long)]
        interface: Vec<String>,
        /// At most this many messages per interface, the first in the period: a sample.
        #[arg(long)]
        limit: Option<u64>,
        /// What it's compared with besides the record: `installed` (the version installed on this
        /// server, run beside this one: the default), `stored` (kept outputs only), or the path of
        /// another ductus binary.
        #[arg(long)]
        against: Option<String>,
        /// Compare with another version of the project instead: a folder, or a Git revision of this
        /// one (`main`, `v12`, `HEAD~1`): the same report for a code change. It runs with this binary
        /// unless --against names one.
        #[arg(long)]
        baseline: Option<String>,
        /// Also write the report to this file: a page a change board can read and sign (.html), or
        /// JSON (.json).
        #[arg(long)]
        report: Option<PathBuf>,
        /// The report as JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
        /// Leave field values out of the examples: the report then holds no patient data.
        #[arg(long)]
        hide_values: bool,
        /// Write this version's decisions as JSON lines: what the comparing version reads.
        #[arg(long, hide = true)]
        emit: bool,
    },
    /// Hand the running engine over to a new one on the project (or release) as it is now,
    /// refusing no connection: a reload. The new engine starts beside the running one on the same
    /// listening sockets; once it's ready, the running one stops and lets go. Linux: the package's
    /// `systemctl reload ductus` runs it, and the new engine is the binary now on disk (an
    /// upgrade too); exit 0 once it sends, 1 when it couldn't take over (the running engine goes
    /// on: the reason is printed). Windows: asks the service (`sc.exe control Ductus paramchange`,
    /// from an elevated shell); the new engine runs in the service's process, with ductus.env read
    /// again, and the service's log and the Event Log say how it went.
    Handover {
        /// The running engine's control socket (default: `$RUNTIME_DIRECTORY/control`, which
        /// systemd sets for the service and its reload, else /run/ductus/control).
        #[arg(long, env = "DUCTUS_CONTROL")]
        control: Option<PathBuf>,
        /// How long the new engine may take to be ready: it migrates the database first.
        #[arg(long, default_value = "5m", value_parser = parse_duration)]
        timeout: std::time::Duration,
        /// Start the new engine with this command's environment rather than the running
        /// engine's: under systemd, ductus.env as it is now (the unit's reload passes this).
        #[arg(long)]
        with_environment: bool,
        /// Why: recorded with the new engine's deployment, beside who asked (the person behind
        /// `sudo`, else this account; `systemctl reload` is recorded as systemd's, with no reason).
        /// Example: `sudo ductus handover -m "lab wants PV1-3 mapped"`.
        #[arg(short = 'm', long = "message")]
        reason: Option<String>,
    },
    /// Create something new in a project.
    New {
        #[command(subcommand)]
        what: NewCommand,
    },
    /// List the ports and HTTP routes a project listens on, the hosts it connects to, and port
    /// conflicts, for an environment (exits 1 on conflicts).
    Ports {
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
    },
    /// The company-defined documentation fields (`docs-schema.yaml`) and their values (`custom:`)
    /// for every interface, destination and resource. Exits 1 if a required value is missing.
    Docs {
        /// Only what's missing.
        #[arg(long)]
        missing: bool,
        /// As JSON, for a report or another tool.
        #[arg(long)]
        json: bool,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
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
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL", global = true)]
        database_url: Option<String>,
    },
    /// The audit log (who did what), straight from the engine's database.
    Audit {
        #[command(subcommand)]
        action: AuditAction,
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL", global = true)]
        database_url: Option<String>,
    },
    /// Stop taking new messages and deliver what's stored (for upgrades without a handover and
    /// server moves): MLLP listeners close, queued HTTP answers 503, file pickup and schedules
    /// pause. Through the running engine's REST API.
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
        /// The interface whose destination to pause.
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
    /// Certificates for `tls:` on listeners (`certificates:` in resources.yaml).
    Cert {
        #[command(subcommand)]
        action: CertAction,
    },
    /// The Windows service (as an administrator).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Print the REST API's OpenAPI document (the same as `GET /api/v1/openapi.json`, without a
    /// running engine).
    ApiSpec,
    /// Write what an editor needs to help with the project, for the version installed: the SDK's
    /// typings (.ductus/types) and a tsconfig.json for the interfaces' code, and the configuration
    /// files' schemas (.ductus/schema) with VS Code's settings pointing at them, for completion,
    /// descriptions on hover and mistakes underlined in interface.yaml, resources.yaml and
    /// ductus.yaml. `ductus test` and `validate` do it too; run it after an upgrade.
    Types {
        /// Project folder.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Write the docs site's reference pages (`DOCS/reference`) from this binary: the command line,
    /// the REST API, every setting, Event Log IDs, metrics and audit actions. For the docs, not
    /// for users.
    #[command(hide = true)]
    Reference {
        /// The docs folder (with SUMMARY.md, whose reference part is rewritten).
        #[arg(default_value = "docs")]
        docs: PathBuf,
    },
    /// Show message counts by interface and status (from the database, also while the engine is
    /// down); with a token, also each interface's and destination's health from the running
    /// engine (silent, behind, stalled, paused, catching up...).
    Status {
        /// The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`.
        #[arg(long, env = "DUCTUS_DATABASE_URL")]
        database_url: Option<String>,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Send synthetic ADT messages over MLLP.
    Gen {
        /// The MLLP listener to send to, `host:port`.
        #[arg(long, default_value = "127.0.0.1:6661")]
        to: String,
        /// How many messages to send.
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
        /// Send over TLS (also on with --ca or --cert), checking the receiver's certificate
        /// against this system's trusted roots and --ca, as the engine's `tls:` does.
        #[arg(long)]
        tls: bool,
        /// A CA (PEM) the receiver's certificate may come from (a self-signed one: that file).
        #[arg(long)]
        ca: Option<PathBuf>,
        /// Present this client certificate (PEM, ours first), with --key.
        #[arg(long, requires = "key")]
        cert: Option<PathBuf>,
        /// The client certificate's private key (PEM).
        #[arg(long, requires = "cert")]
        key: Option<PathBuf>,
        /// Connect from this address of this machine (127.0.0.2 on Linux, say), to try a
        /// receiver's `allow:` list as a sender elsewhere would.
        #[arg(long)]
        from: Option<std::net::IpAddr>,
    },
    /// Receive messages over MLLP, ACK them, and verify per-patient ordering. With --smtp or
    /// --http, a mail server or web server that accepts everything and prints it instead (to see
    /// what alerts send without a real one).
    Sink {
        /// The MLLP port to listen on (127.0.0.1).
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
        /// Take MLLP over TLS only, presenting --cert and --key (as the engine's `tls:` on a
        /// listener).
        #[arg(long, requires_all = ["cert", "key"], conflicts_with_all = ["smtp", "http"])]
        tls: bool,
        /// The certificate to present (PEM, ours first, then intermediates).
        #[arg(long, requires_all = ["tls", "key"])]
        cert: Option<PathBuf>,
        /// The certificate's private key (PEM).
        #[arg(long, requires_all = ["tls", "cert"])]
        key: Option<PathBuf>,
        /// Ask senders for a client certificate this CA (PEM) issued; others are refused.
        #[arg(long, requires = "tls")]
        clients_ca: Option<PathBuf>,
        /// Only client certificates whose subject matches (`CN=bridges*.hospital.org`; repeat).
        #[arg(long = "subject", requires = "clients_ca")]
        subjects: Vec<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum KeyKind {
    Rsa,
    Ecdsa,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Show the account the service runs as; with NAME, run it as that account: grants it "Log on
    /// as a service", checks its password, and restarts the service if it was running. NAME is
    /// `DOMAIN\svc-ductus$` (a gMSA: no password), `DOMAIN\name` or `.\name` (asks for its
    /// password; give it again after every upgrade), or `NT SERVICE\Ductus` (the default).
    Account {
        /// The account: `DOMAIN\name`, a gMSA (`DOMAIN\name$`), or none to show the current one.
        name: Option<String>,
        /// Read the password from stdin (one line) instead of asking.
        #[arg(long, requires = "name")]
        password_stdin: bool,
        /// Start the service afterwards even if it was stopped.
        #[arg(long, requires = "name")]
        start: bool,
    },
    /// The installer's step after installing the service (grants the logon right, sets the
    /// service's SID type, says when the account needs its password).
    #[command(hide = true)]
    Installed,
}

#[derive(Subcommand)]
enum CertAction {
    /// A new key and a certificate signing request (CSR) for the hospital's CA: writes OUT.key
    /// (private, never overwritten) and OUT.csr, and says what to send, what comes back and
    /// where it goes.
    Request {
        /// Where to write, without the extension: `tls/mllp` writes tls/mllp.key and tls/mllp.csr.
        out: PathBuf,
        /// A DNS name senders connect to (repeat for several; the first is the CN).
        #[arg(long = "dns", required_unless_present = "ips")]
        dns: Vec<String>,
        /// An IP address senders connect to (repeat for several).
        #[arg(long = "ip")]
        ips: Vec<String>,
        /// The rest of the subject, as the CA wants it: "O=General Hospital, OU=Interfaces, C=US".
        #[arg(long, default_value = "")]
        subject: String,
        /// The key: rsa (2048, what every CA takes) or ecdsa (P-256).
        #[arg(long, value_enum, default_value_t = KeyKind::Rsa)]
        key: KeyKind,
    },
    /// What a certificate file or a `certificates:` entry holds: subject, names, issuer, the
    /// chain's order and completeness, days left, and whether the key is the certificate's.
    /// Exits 1 when the engine or senders would refuse it.
    Show {
        /// A PEM file, or the name of a `certificates:` entry in resources.yaml.
        target: String,
        /// The key to check (default: the .key file beside a .pem, or the entry's key).
        #[arg(long)]
        key: Option<PathBuf>,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// A self-signed certificate and its key, for a lab or a test: writes OUT.pem and OUT.key and
    /// prints the resources.yaml entry. Senders must be given OUT.pem to trust. Production
    /// certificates come from the hospital's CA.
    SelfSigned {
        /// Where to write, without the extension: `tls/engine` writes tls/engine.pem and tls/engine.key.
        out: PathBuf,
        /// A DNS name or IP address senders connect to (repeat for several).
        #[arg(long = "name", required = true)]
        names: Vec<String>,
        /// How long it is valid.
        #[arg(long, default_value_t = 365)]
        days: u32,
    },
}

#[derive(Subcommand)]
enum AlertsAction {
    /// Send a test notification to a channel now, from ductus.yaml in this environment.
    Test {
        /// A channel's name under `alerts.channels` in ductus.yaml.
        channel: String,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
}

/// What `ductus release` takes to make one.
#[derive(clap::Args)]
struct ReleaseArgs {
    /// Why: a sentence the release carries, shown wherever it goes (`-m "lab feed: new OBX mapping"`).
    #[arg(short = 'm', long = "message", value_name = "WHY")]
    reason: Option<String>,
    /// Carry files that aren't committed as they are in the folder (changed, added, new and not
    /// ignored; deleted ones left out), instead of refusing. The release lists them, and every
    /// check of it says so: for trying a release out, not for production.
    #[arg(long)]
    uncommitted: bool,
    /// The release this one replaces, by its whole id or its file: recorded, so a deploy can tell
    /// whose work it would undo when the project isn't in git (in git, the commit's history says).
    #[arg(long, value_name = "RELEASE")]
    over: Option<String>,
    /// The branch to record when HEAD is detached (CI checking out a tag or a pull request; on
    /// GitHub Actions `--branch "$GITHUB_REF_NAME"`). An engine with DUCTUS_DEPLOY_BRANCHES
    /// refuses releases made on other branches. When HEAD is on a branch, it's that one (another
    /// name here is refused).
    #[arg(long, value_name = "NAME")]
    branch: Option<String>,
    /// Make it without running the tests; the release says they weren't run, and `ductus release
    /// check` then runs every interface's.
    #[arg(long)]
    no_tests: bool,
    /// Where to write it: a folder (made when missing if it ends with `/`), or the file's path
    /// (default `.ductus/releases/` in the project).
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
    /// One JSON document: the manifest, with the file written, its size and how long it took.
    #[arg(long)]
    json: bool,
    /// The project folder: the one with `interfaces/` and ductus.yaml.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// The environment the tests run in and interface.yaml is read in (the release itself is the
    /// same for every environment).
    #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
    env: String,
}

#[derive(Subcommand)]
enum ReleaseAction {
    /// The gate: check a release against an environment before deploying it, changing nothing.
    /// Unpacks it into a temporary folder (removed after), checks its files against its id, then
    /// runs `ductus validate` for the environment against its database (reading only) and the
    /// tests of the interfaces whose code differs from what runs there, each as a child process.
    /// Exits 1 on any problem. A read-only login is enough (`pg_read_all_data`).
    Check {
        /// The release's file (`ductus release` printed it).
        bundle: PathBuf,
        /// The environment to check it for: its values, secrets (DUCTUS_SECRET_KEY) and database.
        #[arg(long, env = "DUCTUS_ENV")]
        env: String,
        /// Run every interface's tests, not only those of what the release changes.
        #[arg(long)]
        all_tests: bool,
        /// The environment's database (read-only): what runs there now, and validate's checks
        /// against it. Without it every test runs and validate checks without a database (said).
        #[arg(long, env = "DUCTUS_DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        /// One JSON document instead of the report.
        #[arg(long)]
        json: bool,
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
        /// Where its messages come from.
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
        /// The project folder: the one with `interfaces/` and ductus.yaml.
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
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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
    Confirm {
        /// The interface whose shorter retention to apply.
        interface: String,
    },
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
        /// Only what concerns this interface.
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
    Revoke {
        /// The token's name, as `ductus token list` shows it.
        name: String,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Make an account. The password is asked for twice (or read from stdin with --password-stdin).
    Create {
        /// The name they sign in with.
        username: String,
        /// viewer, operator or admin.
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Their name as others see it (in the web UI and the audit log).
        #[arg(long)]
        display_name: Option<String>,
        /// Their email address: how owner texts (`about.owner`) and alerts reach them.
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
        /// The account's sign-in name.
        username: String,
        /// The new role: viewer, operator or admin (none given: leave it).
        #[arg(long)]
        role: Option<String>,
        /// The permissions the user has afterwards (none given: leave them as they are).
        #[arg(long = "permission")]
        permissions: Vec<String>,
    },
    /// Give a user a new temporary password (they must change it at their next sign-in).
    Password {
        /// The account's sign-in name.
        username: String,
        /// Read the password from stdin (one line) instead of asking.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Stop a user signing in; their sessions and tokens stop at once.
    Disable {
        /// The account's sign-in name.
        username: String,
    },
    /// Let a disabled user sign in again.
    Enable {
        /// The account's sign-in name.
        username: String,
    },
    /// Let a user locked out by wrong passwords try again.
    Unlock {
        /// The account's sign-in name.
        username: String,
    },
}

#[derive(Subcommand)]
enum SecretAction {
    /// Print a new random key to put in DUCTUS_SECRET_KEY (keep it out of Git).
    Init,
    /// Create or update a secret. The value is read from stdin if not given.
    Set {
        /// The secret's name, as config files use it: `${secret:NAME}`.
        name: String,
        /// The value (read from stdin when not given, which keeps it out of the shell's history).
        value: Option<String>,
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
        #[arg(long, env = "DUCTUS_ENV", default_value = "dev")]
        env: String,
    },
    /// List secret names (never values).
    List {
        /// The project folder: the one with `interfaces/` and ductus.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json.
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

/// A start the engine refused (another engine runs its interfaces):
/// the reason, then exit 3, which the systemd unit doesn't restart (a person has to act).
fn exit_if_refused(result: Result<()>) -> Result<()> {
    if let Err(e) = &result
        && let Some(refused) = e.downcast_ref::<ductus_runtime::ownership::Refused>()
    {
        eprintln!("error: {refused}");
        std::process::exit(ductus_runtime::ownership::REFUSED_EXIT);
    }
    result
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

#[cfg(windows)]
fn service_command(action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Account { name: None, .. } => service::account::show(),
        ServiceAction::Account { name: Some(name), password_stdin, start } => {
            service::account::change(&name, password_stdin, start)
        }
        ServiceAction::Installed => service::account::installed(),
    }
}

#[cfg(not(windows))]
fn service_command(_: ServiceAction) -> Result<()> {
    anyhow::bail!("`ductus service` is for the Windows service; on Linux systemd runs the engine (`systemctl`)")
}

async fn run(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        // Plain text when logging to a file or pipe (no color codes in log files).
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Doctor's report says what failed; the TLS verifier would log each failure again.
            if matches!(cli.command, Command::Doctor { .. }) {
                "warn,rustls_platform_verifier=off,tiberius=error".into()
            } else if matches!(cli.command, Command::Upgrade { .. } | Command::Replay { .. } | Command::Release { .. })
            {
                // Their reports say what went wrong, and `--json` (or `--emit`'s lines) must be the
                // only thing on stdout.
                "off".into()
            } else {
                // A certificate the platform verifier refuses is logged by it at ERROR every time,
                // and said again where it matters (a delivery's error, a check's result): the
                // engine checks certificates every few hours, private CAs included. tiberius warns
                // at every new connection with trust_server_certificate (the start says it once).
                // Interface code's `console` lines: the environment's `console:` decides.
                "info,sqlx=warn,rustls_platform_verifier=off,tiberius=error,script=debug".into()
            }
        }))
        .init();

    match cli.command {
        Command::Run(args) => {
            let (root, database_url, options) = args.parts()?;
            exit_if_refused(ductus_runtime::run(&root, &database_url, options).await)
        }
        Command::Dev {
            root,
            threads,
            env,
            api_listen,
            stop_timeout,
            port,
            reset,
            yes,
            database_only,
            inspect,
            inspect_address,
        } => {
            // Before Postgres starts: a wrong name or a port in use is said at once.
            let inspect = dev::inspect(&root, &env, &inspect, &inspect_address)?;
            let defaults = ductus_runtime::EngineOptions::default();
            let engine = ductus_runtime::EngineOptions {
                pool_threads: threads.unwrap_or(defaults.pool_threads),
                env,
                api_listen,
                stop_timeout,
                inspect,
                ..defaults
            };
            dev::run(dev::Options { root, engine, port, reset, yes, database_only }).await
        }
        Command::Test { root, interface, filter, list, inspect, json, env, read_only } => {
            let inspect = inspect.as_deref().map(ductus_runtime::inspect::address).transpose()?;
            if inspect.is_some() && list {
                anyhow::bail!("`--list` runs nothing to debug: leave out `--inspect`, or `--list`");
            }
            if inspect.is_some() && interface.len() > 1 {
                anyhow::bail!("a debugger attaches to one interface's tests at a time: give one `--interface`");
            }
            let options = project::TestOptions {
                interface: &interface,
                filter: filter.as_deref(),
                list,
                json,
                read_only,
                inspect,
            };
            exit_on_failure(project::test(&root, &env, options).await)
        }
        Command::Try {
            interface,
            file,
            fixture,
            message,
            from,
            destination,
            token,
            database_url,
            root,
            env,
            inspect,
            json,
        } => {
            let inspect = inspect.as_deref().map(ductus_runtime::inspect::address).transpose()?;
            let source = match (file, fixture, message) {
                (Some(file), _, _) => trying::Source::File(file),
                (_, Some(fixture), _) => trying::Source::Fixture(fixture),
                (_, _, Some(id)) => trying::Source::Stored { id, api: from.map(|url| (url, token)), database_url },
                _ => anyhow::bail!("give the message: a file, `--fixture NAME`, or `--message ID`"),
            };
            trying::run(trying::Args { root, env, interface, source, destination, json, inspect }).await
        }
        Command::History { interface, source, destination, all, limit, before, json, database_url, root, env } => {
            let part = match (source, destination) {
                (true, _) => Some(ductus_runtime::history::Part::Source),
                (false, Some(name)) => Some(ductus_runtime::history::Part::Destination(name)),
                (false, None) => None,
            };
            history::run(history::Args { root, env, interface, database_url, all, limit, before, json, part }).await
        }
        Command::Validate { root, env, database_url, json, read_only } => {
            exit_on_failure(project::validate(&root, &env, database_url, read_only, json).await)
        }
        Command::Release {
            action: Some(ReleaseAction::Check { bundle, env, all_tests, database_url, json }), ..
        } => exit_on_failure(release::check(release::Check { bundle, env, all_tests, database_url, json }).await),
        Command::Deploy { release, to, token, from, from_token, rollback, reason, force, all_tests, json, timeout } => {
            let d = deploy::Deploy {
                release,
                to,
                token,
                from,
                from_token,
                rollback,
                reason,
                force,
                all_tests,
                json,
                timeout,
            };
            exit_on_failure(deploy::run(d).await)
        }
        Command::Release { action: None, make } => {
            let ReleaseArgs { reason, uncommitted, over, branch, no_tests, out, json, root, env } = make;
            release::make(release::Make { root, env, reason, uncommitted, over, branch, no_tests, out, json }).await
        }
        Command::Upgrade { check: _, json, root, env, database_url, replay } => {
            let options =
                upgrade::Options { root, env, database_url: tools::database_url(database_url)?, json, replay };
            exit_on_failure(upgrade::check(options).await)
        }
        Command::Replay {
            root,
            env,
            database_url,
            since,
            until,
            interface,
            limit,
            against,
            baseline,
            report,
            json,
            hide_values,
            emit,
        } => {
            let database_url = tools::database_url(database_url)?;
            if emit {
                let (from, to) = replay::period(&since, until.as_deref())?;
                let options =
                    ductus_runtime::replay::Options { root, env, database_url, from, to, interfaces: interface, limit };
                return replay::emit(options).await;
            }
            let options = replay::Options {
                root,
                env,
                database_url,
                since,
                until,
                interfaces: interface,
                limit,
                against,
                baseline,
                report,
                json,
                hide_values,
            };
            exit_on_failure(replay::run(options).await)
        }
        #[cfg(windows)]
        Command::Handover { reason, .. } => service::reload(reason.as_deref()),
        #[cfg(not(windows))]
        Command::Handover { control, timeout, with_environment, reason } => {
            use ductus_runtime::handover;
            let path = control.or_else(handover::control_path).unwrap_or_else(|| "/run/ductus/control".into());
            let environment = with_environment.then(|| {
                std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
            });
            let asked = handover::Asked::here(reason);
            let request = handover::Request { timeout_secs: timeout.as_secs(), environment, asked };
            let (ok, text) = handover::ask(&path, &request, |line| println!("{line}")).await?;
            if !ok {
                eprintln!("error: {text}");
                std::process::exit(1);
            }
            println!("{text}");
            Ok(())
        }
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
        Command::Cert { action: CertAction::SelfSigned { out, names, days } } => cert::self_signed(&out, &names, days),
        Command::Cert { action: CertAction::Request { out, dns, ips, subject, key } } => {
            let key = match key {
                KeyKind::Rsa => ductus_runtime::tls::KeyType::Rsa,
                KeyKind::Ecdsa => ductus_runtime::tls::KeyType::Ecdsa,
            };
            cert::request(&out, &dns, &ips, &subject, key)
        }
        Command::Cert { action: CertAction::Show { target, key, root, env } } => {
            exit_on_failure(cert::show(&target, key.as_deref(), &root, &env))
        }
        Command::Service { action } => service_command(action),
        Command::Types { root } => project::types(&root).await,
        Command::Reference { docs } => reference::write(&docs),
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
        Command::Gen { to, count, patients, rate, connections, hot, offset, tls, ca, cert, key, from } => {
            let tls = (tls || ca.is_some() || cert.is_some()).then_some(tools::TlsFiles { ca, cert, key });
            tools::generate(tools::GenOptions { to, count, patients, rate, connections, hot, offset, tls, from }).await
        }
        Command::Sink { smtp: Some(port), dir, .. } => sinks::smtp(port, dir).await,
        Command::Sink { http: Some(port), dir, .. } => sinks::http(port, dir).await,
        Command::Sink { port, reject, tls, cert, key, clients_ca, subjects, .. } => {
            let files = tools::TlsFiles { ca: clients_ca, cert, key };
            tools::sink(port, reject, tls.then_some(tools::SinkTls { files, subjects })).await
        }
    }
}
