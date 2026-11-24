//! Type errors from TypeScript 7's own `tsc`, a native program shipped beside `ductus` (its
//! `typescript/` folder), run as a process. The same compiler on a laptop, in CI and at the deploy
//! gate: what's checked where code is written is what's checked where it's deployed.
//!
//! TS 7 is a Go program that can't run in V8, and 7.0 has no API (7.1's needs Node and is marked
//! unstable), so its command line is the interface: `tsc -p <config> --pretty false` prints one
//! `file(line,col): error TS2322: message` per error, an elaboration on the indented lines after.
//! What it names, imports and exports is read without it (`analysis.rs`).

use crate::analysis::Diagnostic;
use anyhow::{Context, Result, bail};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// The TypeScript version Ductus ships and its tests expect (`scripts/dev/tsc.sh` fetches it;
/// `the_tsc_script_fetches_the_version_ductus_expects` keeps them the same).
pub const TSC_VERSION: &str = "7.0.2";

/// Names a `tsc` to use instead of the one shipped beside `ductus`.
pub const TSC_VARIABLE: &str = "DUCTUS_TSC";

const EXE: &str = if cfg!(windows) { "tsc.exe" } else { "tsc" };

/// The options `validate` checks with, whatever the project's tsconfig.json says (the one
/// `ductus types` writes has the same): code is checked alike on every machine.
const OPTIONS: &str = r#"{
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2023"],
    "strict": true,
    "noEmit": true,
    "allowImportingTsExtensions": true,
    "skipLibCheck": true,
    "types": []
  }"#;

/// A TypeScript compiler to check with.
#[derive(Debug, Clone)]
pub struct Tsc {
    pub path: PathBuf,
}

/// No `tsc` where one was looked for: each place, in order.
#[derive(Debug)]
pub struct NotFound {
    pub looked: Vec<PathBuf>,
    /// `DUCTUS_TSC` was set (to the first place looked): only it was tried.
    pub variable: bool,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.variable {
            let named = self.looked.first().map(|p| p.display().to_string()).unwrap_or_default();
            return write!(f, "{TSC_VARIABLE} names {named}, which isn't a file: set it to a TypeScript 7 `tsc`");
        }
        let looked: Vec<String> = self.looked.iter().map(|p| p.display().to_string()).collect();
        write!(
            f,
            "TypeScript's `tsc` isn't where ductus looks for it ({}): install the whole package (its `typescript` folder goes beside the ductus program), or set {TSC_VARIABLE} to a TypeScript 7 `tsc`; a build from source gets it with `bash scripts/dev/tsc.sh`",
            looked.join(", ")
        )
    }
}

impl std::error::Error for NotFound {}

impl Tsc {
    /// Where `tsc` is looked for, in order: `DUCTUS_TSC` alone when it's set; else the
    /// `typescript` folder beside the program (the archives, the MSI), `../lib/ductus/typescript`
    /// from it (`/usr/bin/ductus` → `/usr/lib/ductus/typescript`: the .deb and .rpm, and the
    /// image's `/usr/local`), and the per-user cache `scripts/dev/tsc.sh` fills (builds from
    /// source, tests, CI).
    pub fn places() -> (Vec<PathBuf>, bool) {
        if let Some(named) = std::env::var_os(TSC_VARIABLE).filter(|v| !v.is_empty()) {
            return (vec![PathBuf::from(named)], true);
        }
        let mut places = Vec::new();
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            places.push(dir.join("typescript").join(EXE));
            if let Some(prefix) = dir.parent() {
                places.push(prefix.join("lib").join("ductus").join("typescript").join(EXE));
            }
        }
        if let Some(cache) = cache_dir() {
            places.push(cache.join("ductus").join(format!("typescript-{TSC_VERSION}")).join(EXE));
        }
        (places, false)
    }

    /// The first `tsc` of `places()` that exists.
    pub fn find() -> std::result::Result<Tsc, NotFound> {
        let (looked, variable) = Self::places();
        match looked.iter().find(|p| p.is_file()) {
            Some(path) => Ok(Tsc { path: path.clone() }),
            None => Err(NotFound { looked, variable }),
        }
    }

    /// What it says its version is (`7.0.2`).
    pub fn version(&self) -> Result<String> {
        let out = Command::new(&self.path)
            .arg("--version")
            .output()
            .with_context(|| format!("running {}", self.path.display()))?;
        let text = String::from_utf8_lossy(&out.stdout);
        match text.trim().strip_prefix("Version ") {
            Some(version) if out.status.success() => Ok(version.to_string()),
            _ => bail!("{} --version answered: {}", self.path.display(), said(&out)),
        }
    }

    /// The type errors in `files` (absolute paths with `/`, on disk) with the SDK's `typings`
    /// (which declare `@ductus/sdk`), checked with `OPTIONS`. `typings_key` is the name an error in
    /// the typings is reported under. Runs in a temporary folder of its own (the config and the
    /// typings), so the project is only read: the deploy gate checks a release where it can't write.
    pub fn check(&self, files: &[String], typings: &str, typings_key: &str) -> Result<Vec<Diagnostic>> {
        let folder = tempfile::Builder::new().prefix("ductus-tsc-").tempdir().context("making a folder for tsc")?;
        let dir = folder.path();
        const TYPINGS: &str = "ductus-sdk.d.ts";
        std::fs::write(dir.join(TYPINGS), typings).context("writing the SDK's typings for tsc")?;
        let mut listed: Vec<&str> = files.iter().map(String::as_str).collect();
        listed.push(TYPINGS);
        let config = format!(
            "{{\n  \"compilerOptions\": {OPTIONS},\n  \"files\": {}\n}}\n",
            serde_json::to_string_pretty(&listed)?
        );
        std::fs::write(dir.join("tsconfig.json"), config).context("writing the config for tsc")?;

        let out = Command::new(&self.path)
            .args(["-p", "tsconfig.json", "--pretty", "false"])
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.path.display()))?;
        let typings_at = key(&dir.join(TYPINGS));
        let mut found = parse(&String::from_utf8_lossy(&out.stdout), dir, out.status.success());
        for d in &mut found {
            if d.file.as_deref() == Some(typings_at.as_str()) {
                d.file = Some(typings_key.to_string());
            }
        }
        // Exit 0: no errors; 1 or 2: errors (in the code or the config), printed. Anything else, or
        // a failure with nothing printed (a crash), is the compiler's own failure.
        let printed = matches!(out.status.code(), Some(1 | 2)) && !found.is_empty();
        if !out.status.success() && !printed {
            bail!("{} failed: {}", self.path.display(), said(&out));
        }
        Ok(found)
    }
}

/// What a process printed, for a message: its output's first lines, or its exit status.
fn said(out: &std::process::Output) -> String {
    let text = [&out.stderr, &out.stdout].map(|b| String::from_utf8_lossy(b).trim().to_string()).join("\n");
    let lines: Vec<&str> = text.trim().lines().take(10).collect();
    match lines.is_empty() {
        true => format!("nothing printed ({})", out.status),
        false => format!("{} ({})", lines.join("\n"), out.status),
    }
}

/// The per-user cache folder: `%LOCALAPPDATA%` on Windows, else `$XDG_CACHE_HOME` or `~/.cache`
/// (where `ductus dev` keeps its Postgres too).
fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}

/// An absolute path with `/`, `..` and `.` taken out (as the project's files are keyed).
fn key(path: &Path) -> String {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

/// `tsc --pretty false`'s output as diagnostics. Paths are relative to `dir` (tsc writes them
/// relative to its working folder) and come back absolute. An elaboration (indented lines) joins
/// the message before it with a line break, as the type checker's API flattened it. A line that's
/// neither is kept as it is, never dropped (code 0: an error when tsc failed, else a message).
pub fn parse(output: &str, dir: &Path, succeeded: bool) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ')
            && let Some(last) = found.last_mut()
        {
            last.message.push('\n');
            last.message.push_str(line);
            continue;
        }
        found.push(diagnostic(line, dir).unwrap_or_else(|| Diagnostic {
            file: None,
            line: 0,
            column: 0,
            code: 0,
            category: if succeeded { "message" } else { "error" }.into(),
            message: line.to_string(),
        }));
    }
    found
}

/// One line: `file(line,col): category TSn: message`, or without the place for a global one.
fn diagnostic(line: &str, dir: &Path) -> Option<Diagnostic> {
    let (place, rest) = match line.find("): ") {
        // The place ends at the first `): ` followed by a category: a path may hold `(` or `): `.
        Some(_) if !starts_with_category(line) => {
            let end = line.match_indices("): ").map(|(i, _)| i).find(|i| starts_with_category(&line[i + 3..]))?;
            (Some(&line[..end + 1]), &line[end + 3..])
        }
        _ => (None, line),
    };
    let (category, rest) = rest.split_once(' ')?;
    if !CATEGORIES.contains(&category) {
        return None;
    }
    let (code, message) = rest.strip_prefix("TS")?.split_once(": ")?;
    let code: u32 = code.parse().ok()?;
    let (file, line, column) = match place {
        None => (None, 0, 0),
        Some(place) => {
            let open = place.rfind('(')?;
            let (l, c) = place[open + 1..place.len() - 1].split_once(',')?;
            (Some(key(&dir.join(&place[..open]))), l.parse().ok()?, c.parse().ok()?)
        }
    };
    Some(Diagnostic { file, line, column, code, category: category.into(), message: message.into() })
}

const CATEGORIES: [&str; 4] = ["error", "warning", "suggestion", "message"];

fn starts_with_category(text: &str) -> bool {
    CATEGORIES.iter().any(|c| text.strip_prefix(c).is_some_and(|r| r.starts_with(" TS")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        if cfg!(windows) { PathBuf::from(r"C:\Temp\ductus-tsc-1") } else { PathBuf::from("/tmp/ductus-tsc-1") }
    }

    fn at(relative: &str) -> String {
        key(&dir().join(relative))
    }

    /// Captured from TS 7.0.2 (`--pretty false`): one error, an elaborated one, one in a file
    /// outside the working folder, a global one with no file.
    #[test]
    fn reads_tsc_s_output() {
        let out = "a.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                   ../../p/interfaces/a/t.ts(5,40): error TS2322: Type '{ b: string; }' is not assignable to type '{ b: number; }'.\n\
                   \x20 Types of property 'b' are incompatible.\n\
                   \x20   Type 'string' is not assignable to type 'number'.\n\
                   error TS2318: Cannot find global type 'Array'.\n";
        let d = parse(out, &dir(), false);
        assert_eq!(d.len(), 3, "{d:#?}");
        assert_eq!((d[0].file.as_deref(), d[0].line, d[0].column, d[0].code), (Some(at("a.ts").as_str()), 3, 7, 2322));
        assert_eq!(d[0].category, "error");
        assert_eq!(d[0].message, "Type 'string' is not assignable to type 'number'.");
        assert_eq!(d[1].file.as_deref(), Some(key(&dir().join("../../p/interfaces/a/t.ts")).as_str()));
        assert!(!d[1].file.as_deref().unwrap().contains(".."), "{:?}", d[1].file);
        assert_eq!(
            d[1].message,
            "Type '{ b: string; }' is not assignable to type '{ b: number; }'.\n  Types of property 'b' are incompatible.\n    Type 'string' is not assignable to type 'number'."
        );
        assert_eq!((d[2].file.as_deref(), d[2].line, d[2].code), (None, 0, 2318));
    }

    /// A path with brackets or `): ` in it, Windows paths (tsc writes `/`, or a drive when the file
    /// is on another one), CRLF line ends.
    #[test]
    fn reads_awkward_paths() {
        let out = "x (copy)/a): b.ts(1,2): error TS1005: ';' expected.\r\nD:/Projects/p/t.ts(10,1): warning TS6133: 'x' is declared but its value is never read.\r\n";
        let d = parse(out, &dir(), false);
        assert_eq!(d.len(), 2, "{d:#?}");
        assert_eq!(d[0].file.as_deref(), Some(at("x (copy)/a): b.ts").as_str()));
        assert_eq!((d[0].line, d[0].column, d[0].message.as_str()), (1, 2, "';' expected."));
        assert_eq!(d[1].category, "warning");
        if cfg!(windows) {
            assert_eq!(d[1].file.as_deref(), Some("D:/Projects/p/t.ts"));
        }
    }

    /// What it doesn't understand is shown, never dropped: an error when tsc failed.
    #[test]
    fn keeps_lines_it_can_t_read() {
        let d = parse("something new\n  and more\n", &dir(), false);
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].code, d[0].category.as_str(), d[0].message.as_str()),
            (0, "error", "something new\n  and more")
        );
        assert_eq!(parse("a note\n", &dir(), true)[0].category, "message");
    }

    #[test]
    fn where_tsc_is_looked_for() {
        // Only read here, so no other test sees it changed.
        let (places, variable) = Tsc::places();
        if std::env::var_os(TSC_VARIABLE).is_none() {
            assert!(!variable);
            assert!(places.iter().any(|p| p.ends_with(Path::new("typescript").join(EXE))), "{places:?}");
            assert!(places.last().unwrap().to_string_lossy().contains(&format!("typescript-{TSC_VERSION}")));
        }
        let missing = NotFound { looked: vec![PathBuf::from("/nowhere/tsc")], variable: true };
        assert!(missing.to_string().starts_with("DUCTUS_TSC names /nowhere/tsc"), "{missing}");
        let missing = NotFound { looked: places, variable: false };
        assert!(missing.to_string().contains("scripts/dev/tsc.sh"), "{missing}");
    }

    #[test]
    fn the_tsc_script_fetches_the_version_ductus_expects() {
        let script = include_str!("../../../scripts/dev/tsc.sh");
        assert!(
            script.lines().any(|l| l == format!("VERSION={TSC_VERSION}")),
            "scripts/dev/tsc.sh's VERSION isn't {TSC_VERSION}"
        );
    }
}
