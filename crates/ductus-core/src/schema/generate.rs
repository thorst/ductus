//! Makes `docs/schema/*.json` from the config types, and checks them: current, every setting
//! described for people, and agreeing with what the engine accepts.
//!
//! Agreement is tested on files in `good/` (both the schema and the engine take them) and `bad/`
//! (both refuse them; each starts with a comment saying why), per configuration file. Between them,
//! the good files must use every setting the schema has, so a setting whose shape in the schema is
//! wrong (a duration the schema thinks is a number) can't go unnoticed.

use crate::schema::SCHEMAS;
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A whole-value reference to an environment's variable or secret: what may stand anywhere a
/// number, a yes/no or one of a list is expected, in files read per environment.
const REFERENCE: &str = r"^\$\{[^}]+\}$";

/// Each file's schema, as the engine reads that file.
fn generated() -> Vec<(&'static str, Value)> {
    SCHEMAS
        .iter()
        .map(|s| {
            let schema = match s.name {
                "interface.json" => everywhere(root::<crate::InterfaceConfig>()),
                "resources.json" => everywhere(root::<crate::Resources>()),
                "ductus.json" => only_under(root::<crate::settings::Settings>(), "alerts"),
                "environment.json" => root::<crate::env::EnvironmentFile>(),
                "docs-schema.json" => root::<crate::docs::SchemaFile>(),
                other => panic!("no type for {other}"),
            };
            (s.name, finish(schema, s.title))
        })
        .collect()
}

fn root<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft07().into_generator().into_root_schema_for::<T>();
    serde_json::to_value(schema).expect("schemas are JSON")
}

/// What people read, finished: the title; a connector's description on its key (`mllp:`), where an
/// editor shows it, rather than on the one-key object around it; descriptions as paragraphs (doc
/// comments wrap at 100 columns) and also as `markdownDescription` (VS Code's YAML extension shows
/// that one formatted: code in backticks, lists); defaults as they're written in YAML (`4h`, not
/// seconds and nanoseconds) and none where there is nothing (`null`).
fn finish(mut schema: Value, title: &str) -> Value {
    schema["title"] = json!(title);
    fn walk(node: &mut Value) {
        match node {
            Value::Object(map) => {
                if let Some(Value::Array(variants)) = map.get_mut("oneOf") {
                    variants.iter_mut().filter_map(Value::as_object_mut).for_each(connector_description);
                }
                if let Some(Value::String(d)) = map.get("description") {
                    let d = paragraphs(d);
                    map.insert("description".into(), Value::String(d.clone()));
                    map.insert("markdownDescription".into(), Value::String(d));
                }
                match map.get("default") {
                    Some(Value::Null) => drop(map.remove("default")),
                    Some(Value::Object(d)) if d.contains_key("secs") && d.contains_key("nanos") => {
                        let years = map.get("pattern").and_then(Value::as_str).is_some_and(|p| p.contains("(y|"));
                        let text = duration_text(&d["secs"], &d["nanos"], years);
                        map.insert("default".into(), Value::String(text));
                    }
                    _ => {}
                }
                map.values_mut().for_each(walk);
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut schema);
    schema
}

/// `{ mllp: {...} }` with the variant's description outside: move it onto `mllp`.
fn connector_description(map: &mut Map<String, Value>) {
    let one_key = map.get("properties").and_then(Value::as_object).filter(|p| p.len() == 1).is_some();
    if !one_key || map.get("required").and_then(Value::as_array).is_none_or(|r| r.len() != 1) {
        return;
    }
    let Some(Value::String(description)) = map.remove("description") else { return };
    map.remove("markdownDescription");
    let property = map["properties"].as_object_mut().unwrap().values_mut().next().unwrap();
    if property.get("description").is_none() {
        if property.get("$ref").is_some() {
            *property = json!({ "allOf": [property.take()] });
        }
        property["description"] = Value::String(description);
    }
}

/// A doc comment as paragraphs: lines joined, except blank lines, list items, table rows and code.
fn paragraphs(text: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```");
        let block = trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with('|');
        if out.is_empty() {
            out.push_str(line);
        } else if code || fence || block || line.trim().is_empty() || out.ends_with('\n') {
            out.push('\n');
            out.push_str(line);
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
    out.replace("\n\n\n", "\n\n").trim().to_string()
}

/// A duration as YAML would say it: `4h`, `2m`, `500ms`, `6y` (where years are allowed).
fn duration_text(secs: &Value, nanos: &Value, years: bool) -> String {
    let (secs, millis) = (secs.as_u64().unwrap_or(0), nanos.as_u64().unwrap_or(0) / 1_000_000);
    if millis > 0 || secs == 0 {
        return format!("{}ms", secs * 1000 + millis);
    }
    let units: &[(&str, u64)] = &[("y", 365 * 86_400), ("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)];
    let (unit, size) = units.iter().find(|(u, size)| (years || *u != "y") && secs % size == 0).unwrap();
    format!("{}{unit}", secs / size)
}

/// A file read per environment: `${NAME}` may stand for any value.
fn everywhere(mut schema: Value) -> Value {
    allow_references(&mut schema);
    schema
}

/// ductus.yaml: only `alerts:` is read per environment.
fn only_under(mut schema: Value, property: &str) -> Value {
    let mut names = BTreeSet::new();
    let mut todo = vec![schema["properties"][property].clone()];
    while let Some(node) = todo.pop() {
        for name in refs(&node) {
            if names.insert(name.clone()) {
                todo.push(schema["definitions"][&name].clone());
            }
        }
    }
    allow_references(&mut schema["properties"][property]);
    for name in names {
        allow_references(&mut schema["definitions"][&name]);
    }
    schema
}

/// The definitions a schema node refers to.
fn refs(node: &Value) -> Vec<String> {
    match node {
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, v)| match (k.as_str(), v) {
                ("$ref", Value::String(r)) => vec![definition_of(r).to_string()],
                _ => refs(v),
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(refs).collect(),
        _ => Vec::new(),
    }
}

/// The definition a `$ref` is in: `X` for `#/definitions/X` and `#/definitions/X/properties/y`.
fn definition_of(reference: &str) -> &str {
    reference.trim_start_matches("#/definitions/").split('/').next().unwrap_or_default()
}

/// A `$ref` as a setting's name: `X`, or `X.y` for a property of a definition.
fn reference_name(reference: &str) -> String {
    reference.trim_start_matches("#/definitions/").replace("/properties/", ".")
}

/// Where a number, yes/no or one of a list is expected, a `${...}` reference is fine too.
fn allow_references(node: &mut Value) {
    let Value::Object(map) = node else {
        if let Value::Array(items) = node {
            items.iter_mut().for_each(allow_references);
        }
        return;
    };
    let typed = |t: &Value| matches!(t.as_str(), Some("integer" | "number" | "boolean"));
    // An enum whose values have descriptions is a `oneOf` of constants: one reference for all.
    let constants = map
        .get("oneOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| branches.iter().all(|b| b.get("const").is_some() || b.get("enum").is_some()));
    // Keywords by their values: `properties` holds settings named `pattern` and `enum` too.
    let scalar = constants
        || map.get("enum").is_some_and(Value::is_array)
        || map.get("pattern").is_some_and(Value::is_string)
        || map.get("type").is_some_and(|t| typed(t) || t.as_array().is_some_and(|a| a.iter().any(typed)));
    if !scalar {
        map.values_mut().for_each(allow_references);
        return;
    }
    let mut outer = Map::new();
    for key in ["description", "x-type", "default", "title"] {
        if let Some(v) = map.remove(key) {
            outer.insert(key.into(), v);
        }
    }
    let inner = Value::Object(std::mem::take(map));
    outer.insert("anyOf".into(), json!([inner, { "type": "string", "pattern": REFERENCE }]));
    *map = outer;
}

fn docs_schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema")
}

#[test]
fn the_schemas_are_current() {
    let mut stale = Vec::new();
    for (name, schema) in generated() {
        let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        let committed = SCHEMAS.iter().find(|s| s.name == name).unwrap().text;
        if committed.replace("\r\n", "\n") != text {
            std::fs::write(docs_schema_dir().join(name), &text).unwrap();
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "updated docs/schema/{{{}}}: run the tests again (the binary embeds them)",
        stale.join(",")
    );
}

/// Every property in a schema, as `where.name`, with its node.
fn properties(schema: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    fn walk<'a>(at: &str, node: &'a Value, out: &mut Vec<(String, &'a Value)>) {
        match node {
            Value::Object(map) => {
                if let Some(Value::Object(props)) = map.get("properties") {
                    for (k, v) in props {
                        out.push((format!("{at}.{k}"), v));
                        walk(&format!("{at}.{k}"), v, out);
                    }
                }
                for (k, v) in map {
                    if k == "properties" {
                        continue;
                    }
                    let at = if k == "definitions" { String::new() } else { at.to_string() };
                    match v {
                        Value::Object(defs) if k == "definitions" => {
                            for (name, d) in defs {
                                walk(name, d, out);
                            }
                        }
                        _ => walk(&at, v, out),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(at, i, out)),
            _ => {}
        }
    }
    walk("", schema, &mut out);
    out
}

#[test]
fn every_setting_is_described_for_people() {
    let mut problems = Vec::new();
    for (name, schema) in generated() {
        if schema.get("description").is_none() {
            problems.push(format!("{name}: the file itself has no description"));
        }
        for (path, node) in properties(&schema) {
            // A setting defined once and pointed at (a destination's options) is described there.
            if node.get("$ref").and_then(Value::as_str).is_some_and(|r| r.contains("/properties/")) {
                continue;
            }
            let Some(text) = node.get("description").and_then(Value::as_str) else {
                problems.push(format!("{name}: {path} has no description"));
                continue;
            };
            let code = ["`crate::", ".rs`", ".rs ", "Option<", "Vec<", "Self::", "None`"];
            if let Some(c) = code.iter().find(|c| text.contains(*c)) {
                problems.push(format!("{name}: {path} reads as code ({c}): {text}"));
            }
            if !text.trim_end().ends_with(['.', ')', ':']) && !text.trim_end().ends_with("```") {
                problems.push(format!("{name}: {path} isn't a finished sentence: {text}"));
            }
        }
    }
    assert!(problems.is_empty(), "{} settings to describe:\n{}", problems.len(), problems.join("\n"));
}

/// The test files for one configuration file (`good/interface/*.yaml`).
fn cases(kind: &str, which: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/schema").join(which).join(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
        .collect()
}

/// The variables `${...}` in the test files refer to.
fn environment() -> crate::Environment {
    crate::Environment::with_variables(
        "test",
        &[
            ("PORT", "6661"),
            ("HOST", "lab.example.org"),
            ("POOL", "8"),
            ("STRICT", "true"),
            ("TLS", "verify-full"),
            ("EVERY", "5m"),
        ],
    )
}

/// `${secret:NAME}` replaced by a value, as a secrets file would give it (the test environment
/// has none).
fn with_secrets(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${secret:") {
        let end = rest[start..].find('}').map_or(rest.len(), |e| start + e + 1);
        out.push_str(&rest[..start]);
        out.push_str("secret-value");
        rest = &rest[end..];
    }
    out + rest
}

/// What the engine says about a file's shape (Ok, or why not): the same reading as at start, up to
/// the checks that need other files (a script, a certificate) or other settings.
fn engine(kind: &str, text: &str, env: &crate::Environment) -> Result<(), String> {
    let text = &with_secrets(text);
    let substituted = || -> anyhow::Result<serde_yaml_ng::Value> {
        let mut value = crate::env::parse_yaml(text)?;
        env.substitute(&mut value)?;
        Ok(value)
    };
    let result: anyhow::Result<()> = (|| {
        match kind {
            "interface" => drop(serde_yaml_ng::from_value::<crate::InterfaceConfig>(substituted()?)?),
            "resources" => drop(serde_yaml_ng::from_value::<crate::Resources>(substituted()?)?),
            "ductus" => {
                let value = crate::env::parse_yaml(text)?;
                let mut alerts = value.get("alerts").cloned();
                drop(serde_yaml_ng::from_value::<crate::settings::Settings>(value)?);
                if let Some(alerts) = &mut alerts {
                    env.substitute(alerts)?;
                    drop(serde_yaml_ng::from_value::<crate::alerts::AlertSettings>(alerts.clone())?);
                }
            }
            "environment" => drop(serde_yaml_ng::from_str::<crate::env::EnvironmentFile>(text)?),
            "docs-schema" => drop(crate::docs::Schema::parse(text)?),
            other => panic!("no reader for {other}"),
        }
        Ok(())
    })();
    result.map_err(|e| format!("{e:#}"))
}

/// What the schema says about a file (Ok, or its first complaint).
fn schema_says(validator: &jsonschema::Validator, text: &str) -> Result<(), String> {
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    let json = serde_json::to_value(yaml).map_err(|e| e.to_string())?;
    match validator.iter_errors(&json).next() {
        None => Ok(()),
        Some(e) => Err(format!("{} at {}", e, e.instance_path())),
    }
}

fn kind_of(name: &str) -> &str {
    name.trim_end_matches(".json")
}

#[test]
fn the_schemas_agree_with_the_engine() {
    let mut problems = Vec::new();
    for (name, schema) in generated() {
        let kind = kind_of(name);
        let validator = jsonschema::draft7::new(&schema).unwrap_or_else(|e| panic!("{name}: {e}"));
        let env = environment();
        for (file, text) in cases(kind, "good") {
            if let Err(e) = schema_says(&validator, &text) {
                problems.push(format!("good/{kind}/{file}: the schema refuses it: {e}"));
            }
            if let Err(e) = engine(kind, &text, &env) {
                problems.push(format!("good/{kind}/{file}: the engine refuses it: {e}"));
            }
        }
        for (file, text) in cases(kind, "bad") {
            if schema_says(&validator, &text).is_ok() {
                problems.push(format!("bad/{kind}/{file}: the schema takes it"));
            }
            if engine(kind, &text, &env).is_ok() {
                problems.push(format!("bad/{kind}/{file}: the engine takes it"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// `at.k`, or `k` at the top.
fn join(at: &str, k: &str) -> String {
    if at.is_empty() { k.to_string() } else { format!("{at}.{k}") }
}

/// Every setting a schema has, once, as a path from the definition it's written in
/// (`Destination.mllp.tls`, `Retry.max_attempts`; the file's own at the top: `name`): a setting
/// shared by `destination` and `destinations` is one setting.
fn setting_paths(schema: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, at: &str, out: &mut BTreeSet<String>) {
        let Value::Object(map) = node else { return };
        if let Some(Value::Object(props)) = map.get("properties") {
            for (k, v) in props {
                out.insert(join(at, k));
                walk(v, &join(at, k), out);
            }
        }
        if let Some(v @ Value::Object(_)) = map.get("additionalProperties") {
            walk(v, &join(at, "*"), out);
        }
        if let Some(v) = map.get("items") {
            walk(v, at, out);
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(items)) = map.get(key) {
                items.iter().for_each(|i| walk(i, at, out));
            }
        }
    }
    let mut out = BTreeSet::new();
    let mut top = schema.clone();
    let definitions = top.as_object_mut().unwrap().remove("definitions").unwrap_or(json!({}));
    walk(&top, "", &mut out);
    for (name, definition) in definitions.as_object().unwrap() {
        walk(definition, name, &mut out);
    }
    out
}

/// The settings a file uses, named as `setting_paths` names them: the file read along the
/// schema, each `$ref` starting from its definition.
fn used_paths(root: &Value, node: &Value, value: &Value, at: &str, out: &mut BTreeSet<String>) {
    let Value::Object(map) = node else { return };
    if let Some(Value::String(r)) = map.get("$ref") {
        let name = reference_name(r);
        if name.contains('.') {
            out.insert(name.clone());
        }
        let target = root.pointer(r.trim_start_matches('#')).unwrap_or(&Value::Null);
        used_paths(root, target, value, &name, out);
    }
    match value {
        Value::Object(fields) => {
            let props = map.get("properties").and_then(Value::as_object);
            for (k, v) in fields {
                match (props.and_then(|p| p.get(k)), map.get("additionalProperties")) {
                    (Some(property), _) => {
                        out.insert(join(at, k));
                        used_paths(root, property, v, &join(at, k), out);
                    }
                    (None, Some(any @ Value::Object(_))) => used_paths(root, any, v, &join(at, "*"), out),
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            if let Some(schema) = map.get("items") {
                items.iter().for_each(|i| used_paths(root, schema, i, at, out));
            }
        }
        _ => {}
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = map.get(key) {
            branches.iter().for_each(|b| used_paths(root, b, value, at, out));
        }
    }
}

#[test]
fn the_good_files_use_every_setting() {
    let mut problems = Vec::new();
    for (name, schema) in generated() {
        let kind = kind_of(name);
        let mut used = BTreeSet::new();
        for (_, text) in cases(kind, "good") {
            let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
            used_paths(&schema, &schema, &serde_json::to_value(yaml).unwrap(), "", &mut used);
        }
        for setting in setting_paths(&schema).difference(&used) {
            problems.push(format!("{kind}: no file in good/{kind} sets {setting}"));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// A fenced `yaml` block in a guide.
struct Example {
    /// Where it is: `configure/tls.md:42`.
    at: String,
    /// The file its first line names (`# resources.yaml`), as written.
    file: String,
    /// Which configuration file that is (`resources`), if it's one of ours.
    kind: Option<&'static str>,
    /// `, in part`: some of the file's top-level settings, not the whole file.
    part: bool,
    text: String,
}

/// Which configuration file a name in an example's first line is.
fn kind_of_file(file: &str) -> Option<&'static str> {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match name {
        "interface.yaml" => Some("interface"),
        "resources.yaml" => Some("resources"),
        "ductus.yaml" => Some("ductus"),
        "docs-schema.yaml" => Some("docs-schema"),
        _ if file.starts_with("environments/") && name.ends_with(".yaml") => Some("environment"),
        _ => None,
    }
}

/// Every YAML example in the guides (every page under docs/ but the generated reference): each
/// starts with a comment naming its file (`docs/STYLE.md`), or it's a problem.
fn guide_examples(problems: &mut Vec<String>) -> Vec<Example> {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let mut pages = Vec::new();
    let mut todo = vec![docs.clone()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
            if entry.is_dir() && entry.file_name().is_some_and(|n| n != "reference" && n != "schema") {
                todo.push(entry);
            } else if entry.extension().is_some_and(|e| e == "md") {
                pages.push(entry);
            }
        }
    }
    pages.sort();
    let mut examples = Vec::new();
    for page in pages {
        let name = page.strip_prefix(&docs).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&page).unwrap();
        let mut lines = text.lines().enumerate();
        while let Some((n, line)) = lines.next() {
            let fence = line.trim_start();
            if !(fence.starts_with("```yaml") || fence.starts_with("```yml")) {
                continue;
            }
            let indent = line.len() - fence.len();
            let body: Vec<&str> = lines
                .by_ref()
                .map(|(_, l)| l)
                .take_while(|l| l.trim_start() != "```")
                .map(|l| l.get(indent..).unwrap_or(l.trim_start()))
                .collect();
            let at = format!("{name}:{}", n + 1);
            let first = body.first().copied().unwrap_or_default();
            let Some(said) = first.strip_prefix("# ") else {
                problems.push(format!("{at}: start the example with the file it belongs to (`# resources.yaml`)"));
                continue;
            };
            let (file, part) = match said.split_once(", in part") {
                Some((file, _)) => (file.trim(), true),
                None => (said.split_whitespace().next().unwrap_or_default(), false),
            };
            if !file.ends_with(".yaml") && !file.ends_with(".yml") {
                problems.push(format!("{at}: start the example with the file it belongs to, not `{first}`"));
                continue;
            }
            examples.push(Example {
                at,
                file: file.to_string(),
                kind: kind_of_file(file),
                part,
                text: body.join("\n"),
            });
        }
    }
    examples
}

/// YAML read as the engine reads it (`${...}` anywhere), with the references written out again
/// as text, as the schema expects them.
fn read_example(text: &str) -> Result<serde_yaml_ng::Value, String> {
    fn restore(value: &mut serde_yaml_ng::Value) {
        use serde_yaml_ng::Value as V;
        match value {
            V::String(s) => *s = s.replace(crate::env::OPEN, "${").replace(crate::env::CLOSE, "}"),
            V::Sequence(items) => items.iter_mut().for_each(restore),
            V::Mapping(map) => map.iter_mut().for_each(|(_, v)| restore(v)),
            V::Tagged(t) => restore(&mut t.value),
            _ => {}
        }
    }
    let mut value = crate::env::parse_yaml(text).map_err(|e| format!("{e:#}"))?;
    restore(&mut value);
    Ok(value)
}

/// An example of some of a file's settings, made a whole file: what the file needs besides them
/// (an interface's name, source and destination).
fn whole(kind: &str, part: serde_yaml_ng::Value) -> Result<serde_yaml_ng::Value, String> {
    use serde_yaml_ng::Value as V;
    let V::Mapping(part) = part else { return Err("an example `in part` must be settings (`key: value`)".into()) };
    let mut base = match kind {
        "interface" => serde_yaml_ng::from_str::<serde_yaml_ng::Mapping>(
            "name: example\nsource: { mllp: { port: 6661 } }\ndestination: { file: { dir: out } }",
        )
        .unwrap(),
        _ => serde_yaml_ng::Mapping::new(),
    };
    if part.contains_key("destinations") {
        base.remove("destination");
    }
    for (k, v) in part {
        base.insert(k, v);
    }
    Ok(V::Mapping(base))
}

/// The variables the guides' examples use: those their `environments/*.yaml` examples give,
/// with the agreement test's.
fn guide_environment(examples: &[Example]) -> crate::Environment {
    let mut variables: Vec<(String, String)> = Vec::new();
    for e in examples.iter().filter(|e| e.kind == Some("environment")) {
        let Ok(file) = serde_yaml_ng::from_str::<crate::env::EnvironmentFile>(&e.text) else { continue };
        for (name, value) in file.variables {
            let text = match value {
                serde_yaml_ng::Value::String(s) => s,
                other => serde_yaml_ng::to_string(&other).unwrap_or_default().trim().to_string(),
            };
            variables.push((name, text));
        }
    }
    for (name, value) in [("PORT", "6661"), ("HOST", "lab.example.org"), ("POOL", "8"), ("STRICT", "true")] {
        variables.push((name.into(), value.into()));
    }
    let pairs: Vec<(&str, &str)> = variables.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    crate::Environment::with_variables("docs", &pairs)
}

/// The guides' YAML is what the engine takes: every example of a configuration file passes the
/// schema and the engine's reading, as a file in `good/` must (`docs/STYLE.md`, "Examples").
#[test]
fn every_yaml_example_in_the_guides_is_valid() {
    let mut problems = Vec::new();
    let examples = guide_examples(&mut problems);
    let env = guide_environment(&examples);
    let schemas = generated();
    let mut checked = 0;
    for e in &examples {
        let Some(kind) = e.kind else { continue };
        let (_, schema) = schemas.iter().find(|(name, _)| kind_of(name) == kind).unwrap();
        let validator = jsonschema::draft7::new(schema).unwrap();
        let value = match read_example(&e.text).and_then(|v| if e.part { whole(kind, v) } else { Ok(v) }) {
            Ok(v) => v,
            Err(why) => {
                problems.push(format!("{}: {}: isn't YAML the engine reads: {why}", e.at, e.file));
                continue;
            }
        };
        let json = serde_json::to_value(&value).unwrap();
        if let Some(why) = validator.iter_errors(&json).next() {
            problems.push(format!("{}: {}: the schema refuses it: {why} at {}", e.at, e.file, why.instance_path()));
        }
        let text = serde_yaml_ng::to_string(&value).unwrap();
        if let Err(why) = engine(kind, &text, &env) {
            problems.push(format!("{}: {}: the engine refuses it: {why}", e.at, e.file));
        }
        checked += 1;
    }
    assert!(problems.is_empty(), "{} examples to fix:\n{}", problems.len(), problems.join("\n"));
    assert!(checked >= 40, "only {checked} examples found: is the guides' folder where the test looks?");
}
