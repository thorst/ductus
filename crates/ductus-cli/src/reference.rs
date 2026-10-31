//! `ductus reference DOCS` (hidden): the docs site's reference pages, made from the binary itself
//! so they can't drift from it: the command line (clap), the REST API (the OpenAPI document), the
//! configuration files (their JSON Schemas, `ductus_core::schema`), Event Log IDs, Prometheus
//! metrics and audit actions. It also rewrites the reference part of `DOCS/SUMMARY.md`.
//!
//! The pages are committed; a CLI test writes them into a temporary folder and compares, so a
//! change to a command, an API operation or a setting fails until `ductus reference docs` is run.

use anyhow::{Context, Result, bail};
use clap::CommandFactory;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// One page: where under `reference/`, its title in the book's contents, how deep it sits there.
struct Page {
    path: String,
    title: String,
    depth: usize,
    text: String,
}

impl Page {
    fn new(path: &str, title: &str, depth: usize, text: String) -> Self {
        Self { path: path.into(), title: title.into(), depth, text }
    }
}

const BEGIN: &str = "<!-- reference: written by `ductus reference docs`; edit the sources, not this list -->";
const END: &str = "<!-- end of reference -->";

/// The folders under `reference/` this command owns: emptied and written again each time.
const OWNED: [&str; 3] = ["cli", "api", "config"];

pub fn write(docs: &Path) -> Result<()> {
    let pages = pages()?;
    let dir = docs.join("reference");
    for owned in OWNED {
        match std::fs::remove_dir_all(dir.join(owned)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("emptying {}", dir.join(owned).display()));
            }
            _ => {}
        }
    }
    for page in &pages {
        let file = dir.join(&page.path);
        std::fs::create_dir_all(file.parent().expect("a folder"))
            .with_context(|| format!("creating {}", file.display()))?;
        std::fs::write(&file, &page.text).with_context(|| format!("writing {}", file.display()))?;
    }
    let summary = docs.join("SUMMARY.md");
    if summary.is_file() {
        let text = std::fs::read_to_string(&summary).with_context(|| format!("reading {}", summary.display()))?;
        let (Some(start), Some(end)) = (text.find(BEGIN), text.find(END)) else {
            bail!("{} has no reference block (`{BEGIN}` … `{END}`)", summary.display());
        };
        let mut contents = String::new();
        for page in &pages {
            let _ = writeln!(contents, "{}- [{}](reference/{})", "  ".repeat(page.depth), page.title, page.path);
        }
        let _ = writeln!(contents, "  - [Libraries and versions](reference/libraries.md)");
        let updated = format!("{}{BEGIN}\n{contents}{}", &text[..start], &text[end..]);
        std::fs::write(&summary, updated).with_context(|| format!("writing {}", summary.display()))?;
    }
    println!("wrote {} reference pages under {}", pages.len(), dir.display());
    Ok(())
}

fn pages() -> Result<Vec<Page>> {
    let mut pages = vec![Page::new("index.md", "Reference", 0, index())];
    pages.extend(cli());
    pages.extend(api()?);
    pages.extend(config()?);
    pages.push(event_log());
    pages.push(metrics());
    pages.push(audit());
    Ok(pages)
}

fn index() -> String {
    "# Reference

Everything Ductus takes and gives, listed: every command, API operation, setting, Event Log ID, \
metric and audit action. These pages are made from the engine itself (`ductus reference`), so \
they always match the version they come with. The guides explain how the pieces fit together; \
the [nuances](../nuances.md) say what could surprise you.

- [Command line](cli/index.md): every `ductus` command and option.
- [REST API](api/index.md): every operation under `/api/v1`, who may call it, and what it takes \
and returns.
- [Configuration files](config/index.md): every setting of `interface.yaml`, `resources.yaml`, \
`ductus.yaml`, the environment files and `docs-schema.yaml`.
- [Event Log IDs](event-log.md): what the Windows service writes to the Event Log.
- [Prometheus metrics](metrics.md): what `GET /api/v1/metrics` serves.
- [Audit actions](audit.md): what the audit log records.
- [Libraries and versions](libraries.md): what each build contains.
"
    .into()
}

/// Lines joined into paragraphs (doc comments and clap's help wrap at 100 columns), lists and
/// code kept as they are.
fn paragraphs(text: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```");
        let block = trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with('|');
        if out.is_empty() {
            out.push_str(line.trim());
        } else if code || fence || block || line.trim().is_empty() || out.ends_with('\n') {
            out.push('\n');
            out.push_str(if code { line } else { line.trim_end() });
        } else {
            out.push(' ');
            out.push_str(trimmed);
        }
        if line.trim().is_empty() {
            out.push('\n');
        }
        if fence {
            code = !code;
        }
    }
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

/// Text for a table cell: one line, no `|`.
fn cell(text: &str) -> String {
    sentence(&paragraphs(text)).replace('|', "\\|").replace("\n\n", "<br><br>").replace('\n', "<br>")
}

/// Ends with a full stop (clap drops the one doc comments end with).
fn sentence(text: &str) -> String {
    let text = text.trim_end();
    if text.is_empty() || text.ends_with(['.', ':', '!', '?']) || text.ends_with("```") {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

/// The id mdBook gives a heading, for links within a page.
fn anchor(heading: &str) -> String {
    heading
        .chars()
        .filter_map(|c| match c {
            c if c.is_alphanumeric() || c == '_' || c == '-' => Some(c.to_ascii_lowercase()),
            c if c.is_whitespace() => Some('-'),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The command line

fn cli() -> Vec<Page> {
    let mut root = crate::Cli::command();
    root.build();
    let commands: Vec<&clap::Command> =
        root.get_subcommands().filter(|c| !c.is_hide_set() && c.get_name() != "help").collect();
    let mut list = String::new();
    for c in &commands {
        let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
        let first = paragraphs(&about).split(". ").next().unwrap_or_default().trim_end_matches('.').to_string();
        let _ = writeln!(list, "| [`ductus {}`]({}.md) | {} |", c.get_name(), c.get_name(), cell(&first));
    }
    let globals: Vec<&clap::Arg> = root.get_arguments().filter(|a| a.is_global_set() && !a.is_hide_set()).collect();
    let mut overview = format!(
        "# Command line

`ductus` is one program: the engine (`ductus run`), the tools around it (`validate`, `test`, \
`doctor`, `replay`), and the commands that talk to a running engine through its REST API \
(`status`, `drain`, `pause`; these need a token, `--token` or `DUCTUS_TOKEN`). Every command \
explains itself with `--help`; these pages say the same, with every option.

Options can also come from the environment (the variable each one names), or from a file of \
`NAME=value` lines given with `--env-file`, which the Linux packages and the Windows service use \
(`/etc/ductus/ductus.env`, `C:\\ProgramData\\Ductus\\ductus.env`).

| Command | What it does |
|---|---|
{list}"
    );
    if !globals.is_empty() {
        overview.push_str("\n## Options for every command\n\n");
        overview.push_str(&options_table(&globals));
    }
    let mut pages = vec![Page::new("cli/index.md", "Command line", 1, overview)];
    for c in commands {
        let mut text = String::new();
        command_text(&mut text, c, &format!("ductus {}", c.get_name()), 1);
        pages.push(Page::new(&format!("cli/{}.md", c.get_name()), &format!("ductus {}", c.get_name()), 2, text));
    }
    pages
}

fn command_text(out: &mut String, command: &clap::Command, path: &str, level: usize) {
    let _ = writeln!(out, "{} `{path}`\n", "#".repeat(level));
    let about = command.get_long_about().or(command.get_about()).map(|a| a.to_string()).unwrap_or_default();
    if !about.is_empty() {
        let _ = writeln!(out, "{}\n", sentence(&paragraphs(&about)));
    }
    let usage = command.clone().bin_name(path).render_usage().to_string();
    let usage = usage.trim_start_matches("Usage:").trim();
    let _ = writeln!(out, "```text\n{usage}\n```\n");
    let arguments: Vec<&clap::Arg> =
        command.get_arguments().filter(|a| !a.is_hide_set() && !a.is_global_set() && !builtin(a)).collect();
    let (positional, options): (Vec<&clap::Arg>, Vec<&clap::Arg>) =
        arguments.into_iter().partition(|a| a.is_positional());
    if !positional.is_empty() {
        let _ = writeln!(out, "**Arguments**\n\n{}", options_table(&positional));
    }
    if !options.is_empty() {
        let _ = writeln!(out, "**Options**\n\n{}", options_table(&options));
    }
    for sub in command.get_subcommands().filter(|c| !c.is_hide_set() && c.get_name() != "help") {
        command_text(out, sub, &format!("{path} {}", sub.get_name()), level + 1);
    }
}

/// clap's own `--help` and `--version`.
fn builtin(arg: &clap::Arg) -> bool {
    matches!(arg.get_id().as_str(), "help" | "version")
}

fn options_table(args: &[&clap::Arg]) -> String {
    let mut out = String::from("| | What it does | Default | Environment |\n|---|---|---|---|\n");
    for arg in args {
        let values = arg
            .get_value_names()
            .map(|v| v.iter().map(|n| format!("<{n}>")).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let takes_value = arg.get_num_args().is_some_and(|n| n.takes_values());
        let name = if arg.is_positional() {
            values
        } else {
            let mut names: Vec<String> = arg.get_short().map(|s| format!("-{s}")).into_iter().collect();
            names.extend(arg.get_long().map(|l| format!("--{l}")));
            let mut name = names.join(", ");
            if takes_value && !values.is_empty() {
                name = format!("{name} {values}");
            }
            name
        };
        let help = arg.get_long_help().or(arg.get_help()).map(|h| h.to_string()).unwrap_or_default();
        let default: Vec<String> = arg.get_default_values().iter().map(|v| v.to_string_lossy().into_owned()).collect();
        let default = default.join(", ");
        let default = if default.is_empty() || !takes_value { String::new() } else { format!("`{default}`") };
        let env = arg.get_env().map(|e| format!("`{}`", e.to_string_lossy())).unwrap_or_default();
        let _ = writeln!(out, "| `{name}` | {} | {default} | {env} |", cell(&help));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The REST API

fn api() -> Result<Vec<Page>> {
    let doc = serde_json::to_value(ductus_runtime::api::openapi())?;
    let tags: Vec<(String, String)> = doc["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| (t["name"].as_str().unwrap_or_default().into(), t["description"].as_str().unwrap_or_default().into()))
        .collect();
    let mut list = String::new();
    for (name, description) in &tags {
        let _ = writeln!(list, "| [{}]({name}.md) | {} |", title(name), cell(description));
    }
    let info = doc["info"]["description"].as_str().unwrap_or_default();
    let overview = format!(
        "# REST API

{}

The API is served by the engine at `/api/v1`, where `api.listen` in ductus.yaml says \
(`http://127.0.0.1:7700/api/v1` by default; HTTPS with `api.tls`). The web UI uses only this \
API, so anything it does, a script can do. The machine-readable description (OpenAPI 3.1) is at \
`/api/v1/openapi.json` on any engine, and `ductus api-spec` prints it without one.

**Who may call what.** Each operation says who may call it:

| | |
|---|---|
| public | anyone, without signing in (`/health`, for load balancers; signing in) |
| signed in | any person or token |
| a person | a person signed in with a password (their own account's settings) |
| operator | the operator or admin role: changes such as resend, pause, drain |
| admin | the admin role: accounts and tokens |
| audit | the `audit` permission (admins have it) |

A token acts with its own role, and a person's token never above that person. Reading a \
message's payload also needs the `payloads` permission, and is written in the audit log first.

**Errors** come back as JSON, `{{\"error\": \"what went wrong, in words\"}}`, with the HTTP status \
saying what kind: 400 a request that can't be done as asked, 401 not signed in, 403 not \
allowed, 404 nothing there, 409 not now (it changed meanwhile), 503 the engine can't do it now \
(the database is unreachable, or it couldn't write the audit row first).

| Part | What it covers |
|---|---|
{list}",
        paragraphs(info)
    );
    let mut pages = vec![Page::new("api/index.md", "REST API", 1, overview)];
    let schemas = &doc["components"]["schemas"];
    for (tag, description) in &tags {
        let mut text = format!("# {}\n\n{}.\n\n", title(tag), description.trim_end_matches('.'));
        let mut shapes = BTreeSet::new();
        for (path, item) in doc["paths"].as_object().into_iter().flatten() {
            for (method, op) in item.as_object().into_iter().flatten() {
                if op["tags"].as_array().is_some_and(|t| t.iter().any(|t| t == tag.as_str())) {
                    operation(&mut text, method, path, op, &mut shapes);
                }
            }
        }
        // The shapes used, and the ones they use.
        let mut todo: Vec<String> = shapes.iter().cloned().collect();
        while let Some(name) = todo.pop() {
            for used in schema_refs(&schemas[&name]) {
                if shapes.insert(used.clone()) {
                    todo.push(used);
                }
            }
        }
        if !shapes.is_empty() {
            text.push_str("## Shapes\n\nThe JSON bodies the operations above take and return.\n\n");
            for name in &shapes {
                shape(&mut text, name, &schemas[name]);
            }
        }
        pages.push(Page::new(&format!("api/{tag}.md"), &title(tag), 2, text));
    }
    Ok(pages)
}

fn title(tag: &str) -> String {
    match tag {
        "me" => "Your account".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

fn operation(out: &mut String, method: &str, path: &str, op: &Value, shapes: &mut BTreeSet<String>) {
    let method = method.to_uppercase();
    let _ = writeln!(out, "## `{method} /api/v1{path}`\n");
    let summary = op["summary"].as_str().or(op["description"].as_str()).unwrap_or_default();
    let _ = writeln!(out, "{}\n", paragraphs(summary));
    let access = ductus_runtime::api::ACCESS
        .iter()
        .find(|(m, p, _)| *m == method && *p == path)
        .map_or("signed", |(_, _, a)| *a);
    let who = match access {
        "public" => "anyone, without signing in",
        "signed" => "anyone signed in (a person or a token)",
        "user" => "a person signed in with a password",
        "operator" => "an operator or admin",
        "admin" => "an admin",
        "audit" => "anyone with the `audit` permission",
        other => other,
    };
    let _ = writeln!(out, "**Who may call it:** {who}.\n");
    if let Some(params) = op["parameters"].as_array().filter(|p| !p.is_empty()) {
        out.push_str("**Parameters**\n\n| | In | Type | |\n|---|---|---|---|\n");
        for p in params {
            let required = if p["required"] == true { " (required)" } else { "" };
            let _ = writeln!(
                out,
                "| `{}`{required} | {} | {} | {} |",
                p["name"].as_str().unwrap_or_default(),
                p["in"].as_str().unwrap_or_default(),
                type_of(&p["schema"], shapes),
                cell(p["description"].as_str().unwrap_or_default())
            );
        }
        out.push('\n');
    }
    if let Some((content_type, body)) = op["requestBody"]["content"].as_object().and_then(|c| c.iter().next()) {
        let _ = writeln!(out, "**Body** ({content_type}): {}\n", type_of(&body["schema"], shapes));
    }
    if let Some(responses) = op["responses"].as_object() {
        out.push_str("**Answers**\n\n");
        for (status, response) in responses {
            let body = response["content"]
                .as_object()
                .and_then(|c| c.iter().next())
                .map(|(t, b)| format!(" ({t}): {}", type_of(&b["schema"], shapes)))
                .unwrap_or_default();
            let description = response["description"].as_str().filter(|d| !d.is_empty());
            let words = description.map(|d| format!(" {}", paragraphs(d))).unwrap_or_default();
            let _ = writeln!(out, "- `{status}`{words}{body}");
        }
        out.push('\n');
    }
}

/// The components a schema names.
fn schema_refs(schema: &Value) -> Vec<String> {
    match schema {
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, v)| match (k.as_str(), v) {
                ("$ref", Value::String(r)) => vec![r.trim_start_matches("#/components/schemas/").to_string()],
                _ => schema_refs(v),
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(schema_refs).collect(),
        _ => Vec::new(),
    }
}

/// An OpenAPI schema in words: `text`, `list of [Alert](#alert)`, `number or null`.
fn type_of(schema: &Value, shapes: &mut BTreeSet<String>) -> String {
    if let Some(r) = schema["$ref"].as_str() {
        let name = r.trim_start_matches("#/components/schemas/").to_string();
        let link = format!("[`{name}`](#{})", anchor(&name));
        shapes.insert(name);
        return link;
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = schema[key].as_array() {
            let words: Vec<String> = branches.iter().map(|b| type_of(b, shapes)).collect();
            return words.join(" or ");
        }
    }
    if let Some(values) = schema["enum"].as_array() {
        return values
            .iter()
            .map(|v| format!("`{}`", v.as_str().map_or(v.to_string(), str::to_string)))
            .collect::<Vec<_>>()
            .join(", ");
    }
    let one = |t: &str, shapes: &mut BTreeSet<String>| match t {
        "string" => match schema["format"].as_str() {
            Some("date-time") => "time (RFC 3339)".to_string(),
            Some("binary") => "bytes".to_string(),
            _ => "text".to_string(),
        },
        "integer" | "number" => "number".into(),
        "boolean" => "true or false".into(),
        "array" => format!("list of {}", type_of(&schema["items"], shapes)),
        "object" => match &schema["additionalProperties"] {
            Value::Object(_) => format!("map of name to {}", type_of(&schema["additionalProperties"], shapes)),
            _ => "object".into(),
        },
        "null" => "null".into(),
        other => other.into(),
    };
    match &schema["type"] {
        Value::String(t) => one(t, shapes),
        Value::Array(types) => {
            let words: Vec<String> = types.iter().filter_map(Value::as_str).map(|t| one(t, shapes)).collect();
            words.join(" or ")
        }
        _ => "any JSON".into(),
    }
}

fn shape(out: &mut String, name: &str, schema: &Value) {
    let _ = writeln!(out, "### `{name}`\n");
    if let Some(d) = schema["description"].as_str() {
        let _ = writeln!(out, "{}\n", paragraphs(d));
    }
    let mut ignored = BTreeSet::new();
    match schema["properties"].as_object() {
        Some(props) => {
            let required: BTreeSet<&str> =
                schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            out.push_str("| Field | Type | |\n|---|---|---|\n");
            for (field, p) in props {
                let optional = if required.contains(field.as_str()) { "" } else { " (optional)" };
                let _ = writeln!(
                    out,
                    "| `{field}`{optional} | {} | {} |",
                    type_of(p, &mut ignored),
                    cell(p["description"].as_str().unwrap_or_default())
                );
            }
            out.push('\n');
        }
        None => {
            let _ = writeln!(out, "{}\n", type_of(schema, &mut ignored));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The configuration files

fn config() -> Result<Vec<Page>> {
    let mut list = String::new();
    for s in &ductus_core::schema::SCHEMAS {
        let schema: Value = serde_json::from_str(s.text)?;
        let first = schema["description"].as_str().unwrap_or_default();
        let first = paragraphs(first).split("\n\n").next().unwrap_or_default().to_string();
        let _ = writeln!(list, "| [`{}`]({}.md) | {} |", s.title, page_name(s.name), cell(&first));
    }
    let overview = format!(
        "# Configuration files

A project is a folder of plain files, kept in Git: what the engine runs is what's in them. \
These pages list every setting each file takes, with what it does, why and when you'd use it, \
its default and an example.

| File | What it holds |
|---|---|
{list}
**Values from the environment.** In interface.yaml, resources.yaml and the `alerts:` section of \
ductus.yaml, any value may be `${{NAME}}` (from `environments/<env>.yaml`) or `${{secret:NAME}}` \
(from the encrypted `secrets/<env>.json`), filled in for the environment the engine runs in. A \
value that is only a reference keeps the variable's type: `port: ${{EPIC_PORT}}` is a number.

**In your editor.** `ductus types` (and `ductus test`, `ductus validate`) writes these files' \
JSON Schemas into the project (`.ductus/schema/`) and tells VS Code where they are, so with the \
YAML extension (Red Hat's) you get completion, these descriptions on hover, and mistakes \
underlined as you type. The schemas come with the engine you have installed, so they match it; \
run `ductus types` again after an upgrade. They're also in the repository, `docs/schema/`.

**Durations** are written with their unit: `500ms`, `90s`, `15m`, `24h`, `7d`.
"
    );
    let mut pages = vec![Page::new("config/index.md", "Configuration files", 1, overview)];
    for s in &ductus_core::schema::SCHEMAS {
        let schema: Value = serde_json::from_str(s.text)?;
        let mut text = format!("# `{}`\n\n", s.title);
        if let Some(d) = schema["description"].as_str() {
            let _ = writeln!(text, "{}\n", paragraphs(d));
        }
        let _ = writeln!(
            text,
            "Schema for editors: `docs/schema/{}` (written into a project by `ductus types`). Files: `{}`.\n",
            s.name,
            s.files.join("`, `")
        );
        let settings = Settings::of(&schema);
        text.push_str(&settings.out);
        pages.push(Page::new(&format!("config/{}.md", page_name(s.name)), s.title, 2, text));
    }
    Ok(pages)
}

/// The settings a guide must explain, by page (`interface`) and heading (`source.mllp`): each
/// file's top-level settings and their connectors (`source.mllp`, `databases.<name>.postgres`,
/// `format.csv`). A test holds the guides to it, so no setting is only in the reference.
#[cfg(test)]
fn subjects() -> Result<Vec<(&'static str, String)>> {
    let mut out = Vec::new();
    for s in &ductus_core::schema::SCHEMAS {
        let schema: Value = serde_json::from_str(s.text)?;
        let settings = Settings::of(&schema);
        for path in settings.subjects {
            out.push((page_name(s.name), path));
        }
    }
    Ok(out)
}

fn page_name(schema: &str) -> &str {
    schema.trim_end_matches(".json")
}

/// The settings of one file, as headings by their path (`destination.mllp.host`).
struct Settings<'a> {
    root: &'a Value,
    /// Definitions already described, and where.
    seen: BTreeMap<String, String>,
    /// The top-level settings and their connectors, in the order described (`subjects`).
    subjects: Vec<String>,
    out: String,
}

/// The pattern of a `${...}` reference: a branch the generator adds beside every typed value.
const REFERENCE: &str = r"^\$\{[^}]+\}$";

/// A schema with its `$ref` followed (and `allOf: [one]` unwrapped), and the reference branch
/// left out; with the definition's name, if it is one.
fn resolve<'b>(root: &'b Value, node: &'b Value) -> (&'b Value, Option<String>) {
    if let Some(r) = node["$ref"].as_str() {
        let target = root.pointer(r.trim_start_matches('#')).unwrap_or(&Value::Null);
        let name = r.trim_start_matches("#/definitions/");
        let (resolved, _) = resolve(root, target);
        return (resolved, (!name.contains('/')).then(|| name.to_string()));
    }
    if let Some([one]) = node["allOf"].as_array().map(Vec::as_slice) {
        return resolve(root, one);
    }
    // An optional setting (`anyOf: [X, null]`) or one that may be a reference is X.
    if let Some(branches) = node["anyOf"].as_array() {
        let real: Vec<&Value> = branches.iter().filter(|b| b["pattern"] != REFERENCE && b["type"] != "null").collect();
        if let [one] = real.as_slice()
            && real.len() < branches.len()
        {
            return resolve(root, one);
        }
    }
    (node, None)
}

/// The branches a value may take, references and `null` left out.
fn branches(node: &Value) -> Vec<&Value> {
    ["anyOf", "oneOf"]
        .iter()
        .filter_map(|k| node[*k].as_array())
        .flatten()
        .filter(|b| b["pattern"] != REFERENCE && b["type"] != "null")
        .collect()
}

/// A setting's type in words: `duration`, `text`, `` `strict`, `relaxed` ``, `list of text`.
fn setting_type(root: &Value, node: &Value) -> String {
    if let Some(t) = node["x-type"].as_str() {
        return t.to_string();
    }
    let (node, _) = resolve(root, node);
    if let Some(t) = node["x-type"].as_str() {
        return t.to_string();
    }
    let quoted = |v: &Value| format!("`{}`", v.as_str().map_or(v.to_string(), str::to_string));
    if let Some(values) = node["enum"].as_array() {
        return values.iter().map(quoted).collect::<Vec<_>>().join(", ");
    }
    let options = branches(node);
    if !options.is_empty() {
        if options.iter().all(|b| b.get("const").is_some()) {
            return options.iter().map(|b| quoted(&b["const"])).collect::<Vec<_>>().join(", ");
        }
        let words: BTreeSet<String> = options.iter().map(|b| setting_type(root, b)).collect();
        return words.into_iter().collect::<Vec<_>>().join(", or ");
    }
    let one = |t: &str| match t {
        "string" => "text".to_string(),
        "integer" | "number" => "number".into(),
        "boolean" => "`true` or `false`".into(),
        "array" => format!("list of {}", setting_type(root, &node["items"])),
        "object" if node["properties"].is_object() => "settings, below".into(),
        "object" => match &node["additionalProperties"] {
            Value::Object(v) if !v.is_empty() => {
                format!("names, each with {}", setting_type(root, &node["additionalProperties"]))
            }
            _ => "names and values of your own".into(),
        },
        other => other.into(),
    };
    match &node["type"] {
        Value::String(t) => one(t),
        Value::Array(types) => {
            types.iter().filter_map(Value::as_str).filter(|t| *t != "null").map(one).collect::<Vec<_>>().join(" or ")
        }
        _ => "any value".into(),
    }
}

impl<'a> Settings<'a> {
    /// A whole file's settings, described.
    fn of(schema: &'a Value) -> Self {
        let mut settings = Settings { root: schema, seen: BTreeMap::new(), subjects: Vec::new(), out: String::new() };
        settings.children(schema, "");
        settings
    }

    /// Describe the settings under `node` (its properties, those of its branches, a map's values).
    fn children(&mut self, node: &Value, path: &str) {
        let root = self.root;
        let (node, _) = resolve(root, node);
        let node = node.clone();
        let required: BTreeSet<String> =
            node["required"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
        let mut done = BTreeSet::new();
        // (key, schema, required, a connector: the one key a branch requires, `{ mllp: … }`, with
        // or without the options every branch shares beside it, as a destination's)
        let mut properties: Vec<(String, Value, bool, bool)> = Vec::new();
        if let Some(props) = node["properties"].as_object() {
            for (k, v) in props {
                properties.push((k.clone(), v.clone(), required.contains(k), false));
            }
        }
        for branch in branches(&node) {
            let (branch, _) = resolve(root, branch);
            if let Some(props) = branch["properties"].as_object() {
                let connector = match branch["required"].as_array().map(Vec::as_slice) {
                    Some([only]) => only.as_str(),
                    _ => None,
                };
                for (k, v) in props {
                    properties.push((k.clone(), v.clone(), false, connector == Some(k.as_str())));
                }
            }
        }
        // A file's own settings, and the connectors of those (not of their settings in turn).
        let top = path.replace(".<name>", "").split('.').filter(|p| !p.is_empty()).count();
        for (key, value, required, connector) in properties {
            if done.insert(key.clone()) {
                let path = if path.is_empty() { key } else { format!("{path}.{key}") };
                if top == 0 || connector && top == 1 {
                    self.subjects.push(path.clone());
                }
                self.setting(&path, &value, required);
            }
        }
        if let Value::Object(values) = &node["additionalProperties"]
            && !values.is_empty()
        {
            let value = Value::Object(values.clone());
            let (resolved, _) = resolve(root, &value);
            if resolved["properties"].is_object() || !branches(resolved).is_empty() {
                let inner = format!("{path}.<name>");
                self.children(&value, &inner);
            }
        }
        if let Some(items) = node["items"].as_object() {
            self.children(&Value::Object(items.clone()), path);
        }
    }

    fn setting(&mut self, path: &str, node: &Value, required: bool) {
        let root = self.root;
        let _ = writeln!(self.out, "### `{path}`\n");
        let mut facts = vec![format!("**Type:** {}", setting_type(root, node))];
        if required {
            facts.push("**Required**".into());
        }
        let _ = writeln!(self.out, "{}\n", facts.join(" · "));
        let (resolved, definition) = resolve(root, node);
        let description = node["description"].as_str().or(resolved["description"].as_str()).unwrap_or_default();
        if !description.is_empty() {
            let _ = writeln!(self.out, "{}\n", paragraphs(description));
        }
        // Settings of its own: properties, a connector's (a branch with properties), a map's or a
        // list's values with properties.
        let nested = |v: &Value| {
            let (r, _) = resolve(root, v);
            r["properties"].is_object() || branches(r).iter().any(|b| resolve(root, b).0["properties"].is_object())
        };
        let has_children = nested(resolved)
            || resolved["additionalProperties"].is_object() && nested(&resolved["additionalProperties"])
            || resolved["items"].is_object() && nested(&resolved["items"]);
        if !has_children {
            return;
        }
        // A map's values or a list's items that are a definition count as that definition.
        let definition = definition.or_else(|| {
            let inner = resolved["additionalProperties"].as_object().map(|v| Value::Object(v.clone()));
            inner.and_then(|v| v["$ref"].as_str().map(|r| r.trim_start_matches("#/definitions/").to_string()))
        });
        if let Some(name) = &definition {
            if let Some(other) = self.seen.get(name) {
                let _ =
                    writeln!(self.out, "Its settings are those of [`{other}`](#{}).\n", anchor(&format!("`{other}`")));
                return;
            }
            self.seen.insert(name.clone(), path.to_string());
        }
        let node = node.clone();
        self.children(&node, path);
    }
}

// ---------------------------------------------------------------------------------------------
// Event Log IDs, metrics, audit actions

fn event_log() -> Page {
    use crate::service::messages::{ABOUT, MESSAGES};
    let mut text = String::from(
        "# Event Log IDs

On Windows, the service writes what operations teams watch for to the Application log, source \
`Ductus`, with these IDs; Event Viewer shows the text below. Everything else goes only to the \
log files in `C:\\ProgramData\\Ductus\\logs`. Errors not listed here are event 1100, written at \
most once a minute for each kind of message, so the log can't flood. On Linux, the same lines \
are in the journal (`journalctl -u ductus`).

",
    );
    for (id, level, about) in ABOUT {
        let message = MESSAGES.iter().find(|(m, _)| m == id).map_or("", |(_, t)| t);
        let message = message.replace("%n", "\n").replace("%1", "…").replace("%2", "…");
        let first = message.lines().next().unwrap_or_default();
        let _ = writeln!(text, "## {id} · {level}\n\n{about}\n\n> {first}\n");
    }
    Page::new("event-log.md", "Event Log IDs", 1, text)
}

fn metrics() -> Page {
    let mut text = String::from(
        "# Prometheus metrics

`GET /api/v1/metrics` serves these in Prometheus's text format, for Prometheus, Grafana Agent, \
Datadog or anything else that scrapes it. It needs a token like every other call: in \
Prometheus's scrape configuration, `metrics_path: /api/v1/metrics` and `authorization: { \
credentials: dux_… }` (a viewer token: `ductus token create prometheus --role viewer`).

Counters and histograms count from when the engine started (`rate()` and `increase()` handle \
restarts); queue sizes are read from the database at each scrape. Each destination adds a series \
per family, so very many destinations mean many series.

| Metric | Type | Labels | What it measures |
|---|---|---|---|
",
    );
    for m in ductus_runtime::api::METRICS {
        let labels = if m.labels.is_empty() { String::new() } else { format!("`{}`", m.labels.replace(", ", "`, `")) };
        let _ = writeln!(text, "| `{}` | {} | {labels} | {} |", m.name, m.kind, cell(m.help));
    }
    Page::new("metrics.md", "Prometheus metrics", 1, text)
}

fn audit() -> Page {
    let mut text = String::from(
        "# Audit actions

The audit log records who did what, to what, from where, and how it ended: on the Audit page of \
the web UI (with the `audit` permission), through `GET /api/v1/audit`, and with `ductus audit \
export` on the server. Each row has one of these actions. Rows are kept `audit.keep` (six years \
unless ductus.yaml says otherwise).

| Action | What happened |
|---|---|
",
    );
    for (action, about) in ductus_store::AUDIT_ACTIONS {
        let _ = writeln!(text, "| `{action}` | {} |", cell(about));
    }
    Page::new("audit.md", "Audit actions", 1, text)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// The command line's reference is its help: every command and option must say what it does.
    #[test]
    fn every_command_and_option_says_what_it_does() {
        fn walk(command: &clap::Command, path: &str, missing: &mut Vec<String>) {
            if command.get_about().is_none() {
                missing.push(path.to_string());
            }
            for arg in command.get_arguments().filter(|a| !a.is_hide_set() && !super::builtin(a)) {
                if arg.get_help().is_none() && arg.get_long_help().is_none() {
                    missing.push(format!("{path} {}", arg.get_id()));
                }
            }
            for sub in command.get_subcommands().filter(|c| !c.is_hide_set() && c.get_name() != "help") {
                walk(sub, &format!("{path} {}", sub.get_name()), missing);
            }
        }
        let mut root = crate::Cli::command();
        root.build();
        let mut missing = Vec::new();
        walk(&root, "ductus", &mut missing);
        assert!(missing.is_empty(), "{} without help:\n{}", missing.len(), missing.join("\n"));
    }

    fn docs() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
    }

    /// Every page of the book, as (path under docs/, text).
    fn pages() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut todo = vec![docs()];
        while let Some(dir) = todo.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
                if entry.is_dir() {
                    todo.push(entry);
                } else if entry.extension().is_some_and(|e| e == "md") {
                    let name = entry.strip_prefix(docs()).unwrap().to_string_lossy().replace('\\', "/");
                    out.push((name, std::fs::read_to_string(&entry).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// The pages that explain (the guides): not the reference, the nuances, the architecture or
    /// the database's description, which list rather than explain.
    fn guide(page: &str) -> bool {
        const NOT: [&str; 6] =
            ["SUMMARY.md", "STYLE.md", "nuances.md", "architecture.md", "schema.md", "shared-pool.md"];
        !page.starts_with("reference/") && !NOT.contains(&page)
    }

    /// The `#anchor`s of reference/config/<page>.md that a page links to, as (page, anchor).
    fn config_links(text: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for target in text.split("](").skip(1).filter_map(|rest| rest.split(')').next()) {
            let Some((file, anchor)) = target.split_once('#') else { continue };
            if let Some(page) = file.rsplit_once("reference/config/").and_then(|(_, p)| p.strip_suffix(".md")) {
                out.push((page.to_string(), anchor.to_string()));
            }
        }
        out
    }

    /// No orphan setting: every top-level setting of each configuration file, and each of their
    /// connectors, is linked from a guide that explains it, not only listed in the reference.
    #[test]
    fn every_setting_is_explained_in_a_guide() {
        let linked: std::collections::BTreeSet<(String, String)> =
            pages().iter().filter(|(p, _)| guide(p)).flat_map(|(_, t)| config_links(t)).collect();
        let subjects = super::subjects().unwrap();
        // Top-level settings and connectors alike, or the walk has stopped seeing one kind.
        for expected in ["retention", "source.mllp", "destination.http", "databases.<name>.postgres", "format.csv"] {
            assert!(subjects.iter().any(|(_, p)| p == expected), "`{expected}` isn't among the settings to explain");
        }
        let mut orphans = Vec::new();
        for (page, path) in subjects {
            let anchor = super::anchor(&format!("`{path}`"));
            if !linked.contains(&(page.to_string(), anchor.clone())) {
                orphans.push(format!("{page}.md: `{path}` (link it as reference/config/{page}.md#{anchor})"));
            }
        }
        assert!(
            orphans.is_empty(),
            "{} settings no guide explains: explain each where it belongs and link it (docs/STYLE.md):\n{}",
            orphans.len(),
            orphans.join("\n")
        );
    }

    /// The fenced blocks of `lang` in a page, each as its lines joined with `\n`.
    fn fenced(text: &str, lang: &str) -> Vec<String> {
        let (mut blocks, mut open): (Vec<String>, Option<Vec<&str>>) = (Vec::new(), None);
        for line in text.lines() {
            match open.as_mut() {
                None if line.trim_end() == format!("```{lang}") => open = Some(Vec::new()),
                Some(lines) if line.trim_end() == "```" => {
                    blocks.push(lines.join("\n"));
                    open = None;
                }
                Some(lines) => lines.push(line),
                None => {}
            }
        }
        blocks
    }

    /// The code on the "Write interfaces" pages is the guides' example project's: every TypeScript
    /// block there starts with the path of a file in `examples/guides` (`// lib/codes.ts`, with
    /// `, in part` for a piece of it) and is in that file as written. A TypeScript block on any
    /// other guide page that names such a path is checked the same way. The project's own tests
    /// and `validate` run in the CLI tests (`the_guides_example_project_passes_its_tests_and_validate`),
    /// so every example on those pages has run.
    #[test]
    fn the_guides_code_is_the_example_projects() {
        let project = docs().join("../examples/guides");
        let (mut checked, mut wrong) = (0, Vec::new());
        for (page, text) in pages().iter().filter(|(p, _)| guide(p)) {
            for block in fenced(&text.replace("\r\n", "\n"), "ts") {
                let (first, body) = block.split_once('\n').unwrap_or((&block, ""));
                let path = first.strip_prefix("// ").map(|p| p.strip_suffix(", in part").unwrap_or(p));
                let Some(path) = path.filter(|p| p.starts_with("interfaces/") || p.starts_with("lib/")) else {
                    if page.starts_with("write/") {
                        wrong.push(format!("{page}: a block that doesn't start with its file's path: {first}"));
                    }
                    continue;
                };
                checked += 1;
                match std::fs::read_to_string(project.join(path)) {
                    Err(_) => wrong.push(format!("{page}: `{path}` isn't a file of examples/guides")),
                    Ok(file) if !file.replace("\r\n", "\n").contains(body) => {
                        wrong.push(format!("{page}: the block from `{path}` isn't in that file as written"))
                    }
                    Ok(_) => {}
                }
            }
        }
        assert!(checked >= 20, "only {checked} blocks checked: has the way they're marked changed?");
        assert!(wrong.is_empty(), "{} blocks disagree with examples/guides:\n{}", wrong.len(), wrong.join("\n"));
    }

    /// Text decoded twice (UTF-8 read as Windows-1252 and saved again) shows as `Â·` and `â€¦`:
    /// it reached the reference pages once from a source file.
    #[test]
    fn no_page_has_mangled_characters() {
        let mut found = Vec::new();
        for (page, text) in pages() {
            for (n, line) in text.lines().enumerate() {
                if ["Â", "â€", "\u{FFFD}"].iter().any(|bad| line.contains(bad)) {
                    found.push(format!("{page}:{}: {line}", n + 1));
                }
            }
        }
        assert!(found.is_empty(), "mangled characters:\n{}", found.join("\n"));
    }
}
