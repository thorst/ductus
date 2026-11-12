//! `--env-file`: settings in a file of `NAME=value` lines, for places without a service
//! manager that reads one (the Windows service; also handy in a shell). Same names and meaning as
//! the environment variables; a variable that is already set wins over the file.
//!
//! An engine started again in this process (an in-process handover: a deploy or a reload) reads
//! the file again (`run_args_again`), so a reload picks up what changed in it.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The variables `load` set from the file at start: the file's to change at a reload.
static FROM_FILE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Where `--env-file` (or `DUCTUS_ENV_FILE`) points, found before the arguments are parsed so
/// that the file's settings are there when clap reads its `env = ...` options.
pub fn requested() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--env-file" {
            return args.next().map(PathBuf::from);
        }
        if let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--env-file=")) {
            return Some(PathBuf::from(path));
        }
    }
    std::env::var_os("DUCTUS_ENV_FILE").map(PathBuf::from)
}

/// Read the file and set what it names. Call before any other thread exists.
pub fn load(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading the env file {}", path.display()))?;
    for (name, value) in parse(&text).with_context(|| format!("in the env file {}", path.display()))? {
        if std::env::var_os(&name).is_none() {
            // SAFETY: called first thing in `main`, before the runtime starts any thread.
            unsafe { std::env::set_var(&name, value) };
            FROM_FILE.lock().unwrap().push(name);
        }
    }
    Ok(())
}

/// `ductus run`'s arguments, read again for an engine started again in this process: the command
/// line as given, each variable set outside the env file as it was at start (it still wins), and
/// the env file as it is now (a line added, changed or removed counts). The process's
/// environment isn't changed (other threads read it).
pub fn run_args_again() -> Result<crate::RunArgs> {
    let file = match requested() {
        Some(path) => {
            parse(&std::fs::read_to_string(&path).with_context(|| format!("reading the env file {}", path.display()))?)
                .with_context(|| format!("in the env file {}", path.display()))?
        }
        None => Vec::new(),
    };
    let from_file = FROM_FILE.lock().unwrap().clone();
    let variable = |name: &str| -> Option<OsString> {
        let outside = !from_file.iter().any(|n| n == name);
        match std::env::var_os(name) {
            Some(value) if outside => Some(value),
            _ => file.iter().find(|(n, _)| n == name).map(|(_, v)| v.into()),
        }
    };
    run_args_from(std::env::args_os().collect(), variable)
}

/// `run`'s arguments from `argv`, with `variable` in place of the process's environment for every
/// option that reads one (clap's own lookup is turned off). `DUCTUS_TAKEOVER` is never read: it
/// was for the process a handover started, not for an engine started in it.
pub fn run_args_from(argv: Vec<OsString>, variable: impl Fn(&str) -> Option<OsString>) -> Result<crate::RunArgs> {
    use clap::{CommandFactory, FromArgMatches, parser::ValueSource};
    let mut options = Vec::new();
    let command = crate::Cli::command().mut_subcommand("run", |run| {
        for arg in run.get_arguments() {
            if let (Some(long), Some(env)) = (arg.get_long(), arg.get_env()) {
                options.push((arg.get_id().to_string(), long.to_string(), env.to_string_lossy().into_owned()));
            }
        }
        run.mut_args(|arg| arg.env(None::<&'static str>))
    });
    let given = command.clone().try_get_matches_from(&argv).context("reading `ductus run`'s arguments again")?;
    let Some(("run", run)) = given.subcommand() else { bail!("not started as `ductus run`") };
    let mut argv = argv.clone();
    for (id, long, env) in options {
        if id == "takeover" || run.value_source(&id) == Some(ValueSource::CommandLine) {
            continue;
        }
        if let Some(value) = variable(&env) {
            let mut option = OsString::from(format!("--{long}="));
            option.push(value);
            argv.push(option);
        }
    }
    let matches = command.try_get_matches_from(argv).context("reading `ductus run`'s settings again")?;
    match crate::Cli::from_arg_matches(&matches)?.command {
        crate::Command::Run(args) => Ok(args),
        _ => bail!("not started as `ductus run`"),
    }
}

/// `NAME=value` lines; blank lines and `#` comments are skipped; a value may be wrapped in single
/// or double quotes (kept: spaces, `#`); nothing is expanded.
pub fn parse(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            bail!("line {}: expected NAME=value", n + 1);
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("line {}: {name:?} isn't a variable name", n + 1);
        }
        let value = value.trim();
        let value = match value.chars().next() {
            Some(q @ ('"' | '\'')) if value.len() >= 2 && value.ends_with(q) => &value[1..value.len() - 1],
            _ => value,
        };
        out.push((name.to_string(), value.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn lines_comments_quotes() {
        let got = parse(
            "\u{feff}# settings\n\nDUCTUS_ENV=prod\n  DUCTUS_PROJECT = \"C:\\Program Data\\x y\"\nexport A='b # c'\n",
        )
        .unwrap();
        assert_eq!(
            got,
            [
                ("DUCTUS_ENV".to_string(), "prod".to_string()),
                ("DUCTUS_PROJECT".to_string(), "C:\\Program Data\\x y".to_string()),
                ("A".to_string(), "b # c".to_string()),
            ]
        );
    }

    /// A reload's settings: what the command line says stays; the rest comes from the variables
    /// as given (here: the env file as it is now); a handover's descriptor never.
    #[test]
    fn run_s_settings_are_read_again_from_the_variables_given() {
        let argv = ["ductus", "run", "--service", "--env", "prod"].map(std::ffi::OsString::from).to_vec();
        let variables = |name: &str| match name {
            "DUCTUS_THREADS" => Some("3".into()),
            "DUCTUS_ENV" => Some("dev".into()),
            "DUCTUS_TAKEOVER" => Some("7".into()),
            "DUCTUS_RELEASES" => Some("C:\\ProgramData\\Ductus\\releases".into()),
            _ => None,
        };
        let args = super::run_args_from(argv, variables).unwrap();
        assert_eq!(args.threads, Some(3), "from the variables");
        assert_eq!(args.env, "prod", "the command line wins");
        assert_eq!(args.takeover, None, "never a handover's descriptor");
        assert_eq!(args.releases.as_deref(), Some(std::path::Path::new("C:\\ProgramData\\Ductus\\releases")));
        assert_eq!(args.db_connections, 32, "the default, with no variable");
        assert!(args.root.is_none());
    }

    #[test]
    fn mistakes_name_their_line() {
        assert!(parse("A=1\nnot a setting\n").unwrap_err().to_string().contains("line 2"));
        assert!(parse("1 2=3").unwrap_err().to_string().contains("isn't a variable name"));
    }
}
