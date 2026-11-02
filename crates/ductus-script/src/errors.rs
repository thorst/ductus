//! Errors from interface code as people should read them: where in their own `.ts` it happened
//! (stack traces are source-mapped already), without the SDK's and the engine's own frames, with
//! paths relative to the project.

use std::path::{Path, PathBuf};

/// A place one stack-trace line points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The file's `file://` URL, as the stack has it.
    pub url: String,
    pub line: u32,
    pub column: u32,
}

/// The place in one stack-trace line (`    at f (file:///p/x.ts:6:11)`, `    at file:///p/x.ts:6:11`).
pub fn frame(line: &str) -> Option<Frame> {
    let rest = &line[line.find("file:///")?..];
    let located = &rest[..rest.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(rest.len())];
    let (before, column) = located.rsplit_once(':')?;
    let (url, line) = before.rsplit_once(':')?;
    Some(Frame { url: url.to_string(), line: line.parse().ok()?, column: column.parse().ok()? })
}

/// A frame of the SDK (`ductus:///sdk/...`) or of the engine's glue modules: never where a
/// developer should look. (The SDK's test runner has the same rule, `internal` in testing.ts.)
pub fn internal(line: &str) -> bool {
    line.contains("ductus:///") || line.contains(".__ductus__.") || line.contains("__ductus_main__")
}

fn is_frame(line: &str) -> bool {
    line.trim_start().starts_with("at ")
}

/// The project folder, as module URLs name it: made once, then `relative` for each place.
pub struct Root(PathBuf);

impl Root {
    pub fn new(root: &Path) -> Self {
        // Module URLs are canonical paths (`file_url`): compare with the canonical root, through a
        // URL as they went (on Windows canonical paths are `\\?\D:\...`, a URL's path `D:\...`).
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let path = deno_core::ModuleSpecifier::from_directory_path(&canonical)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or(canonical);
        Self(path)
    }

    /// A `file://` URL as a path relative to the project (`interfaces/adt/route.ts`), with `/` on
    /// every platform; a file elsewhere keeps its whole path.
    pub fn relative(&self, url: &str) -> String {
        let Some(path) = deno_core::ModuleSpecifier::parse(url).ok().and_then(|u| u.to_file_path().ok()) else {
            return url.to_string();
        };
        match path.strip_prefix(&self.0) {
            Ok(inside) => inside.to_string_lossy().replace('\\', "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }
}

/// `Root::relative` for one URL.
pub fn relative(url: &str, root: &Path) -> String {
    Root::new(root).relative(url)
}

/// An error from interface code (a thrown error's stack trace) as people should read it: the
/// SDK's and the engine's frames left out, paths relative to `root`, and the first frame in
/// project code at the end of the first line, so a one-line view (a list, a log line) says where:
/// `Error: no PID (interfaces/adt/route.ts:6:11)`. An error without frames is as it was.
pub fn readable(error: &str, root: &Path) -> String {
    Root::new(root).readable(error)
}

impl Root {
    /// `readable`, for this project.
    pub fn readable(&self, error: &str) -> String {
        let mut first = None;
        let mut lines: Vec<String> = Vec::new();
        for line in error.lines() {
            if !is_frame(line) {
                lines.push(line.to_string());
                continue;
            }
            if internal(line) {
                continue;
            }
            match frame(line) {
                Some(f) => {
                    let shown = self.relative(&f.url);
                    lines.push(line.replacen(&f.url, &shown, 1));
                    first.get_or_insert((shown, f.line, f.column));
                }
                None => lines.push(line.to_string()),
            }
        }
        if let (Some((file, line, column)), Some(head)) = (first, lines.first_mut()) {
            head.push_str(&format!(" ({file}:{line}:{column})"));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_frames_with_and_without_a_function_name() {
        let f = frame("    at transform (file:///p/interfaces/x/t.ts:8:11)").unwrap();
        assert_eq!((f.url.as_str(), f.line, f.column), ("file:///p/interfaces/x/t.ts", 8, 11));
        let f = frame("    at async file:///D:/p/x.ts:3:1").unwrap();
        assert_eq!((f.url.as_str(), f.line, f.column), ("file:///D:/p/x.ts", 3, 1));
        assert_eq!(frame("    at __invoke (ductus:///sdk/index.ts:134:18)"), None);
    }

    #[test]
    fn an_error_says_where_in_project_code_and_drops_the_engine_s_frames() {
        let root = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(root.path()).unwrap();
        let url = deno_core::ModuleSpecifier::from_file_path(canonical.join("interfaces/adt/t.ts")).unwrap();
        let glue =
            deno_core::ModuleSpecifier::from_file_path(canonical.join("interfaces/adt/t.ts.__ductus__.js")).unwrap();
        let error = format!(
            "Error: no PID\n    at transform ({url}:6:11)\n    at __invoke (ductus:///sdk/index.ts:134:18)\n    at {glue}:6:25"
        );
        assert_eq!(
            readable(&error, root.path()),
            "Error: no PID (interfaces/adt/t.ts:6:11)\n    at transform (interfaces/adt/t.ts:6:11)"
        );
        assert_eq!(readable("Uncaught boom", root.path()), "Uncaught boom", "nothing to point at");
        // Outside the project: the whole path.
        #[cfg(unix)]
        assert_eq!(
            readable("Error: x\n    at f (file:///opt/lib/y.ts:2:3)", root.path()),
            "Error: x (/opt/lib/y.ts:2:3)\n    at f (/opt/lib/y.ts:2:3)"
        );
    }
}
