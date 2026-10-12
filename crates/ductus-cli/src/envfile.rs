//! `--env-file`: settings in a file of `NAME=value` lines, for places without a service
//! manager that reads one (the Windows service; also handy in a shell). Same names and meaning as
//! the environment variables; a variable that is already set wins over the file.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

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
            unsafe { std::env::set_var(name, value) };
        }
    }
    Ok(())
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

    #[test]
    fn mistakes_name_their_line() {
        assert!(parse("A=1\nnot a setting\n").unwrap_err().to_string().contains("line 2"));
        assert!(parse("1 2=3").unwrap_err().to_string().contains("isn't a variable name"));
    }
}
