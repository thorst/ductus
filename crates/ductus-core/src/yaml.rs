//! Where things are in the project's YAML files. An error loading one carries the setting it's
//! about (`destination.mllp.port`) or, for a syntax error, the line and column the parser stopped
//! at, and `locate` finds a setting's line and column in the text: `ductus validate --json` reads
//! both, so an editor can underline the key. The text people read is the same as without them.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_yaml_ng::Value;

/// How far loading a file got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The file isn't YAML (or a `${...}` reference in it is malformed).
    Syntax,
    /// A `${NAME}` or `${secret:NAME}` the environment doesn't have.
    Reference,
    /// A setting that doesn't exist, or whose value is the wrong kind.
    Setting,
    /// Settings that parse but break a rule: a value out of range, two that don't go together.
    Rule,
}

/// A place in a file: line and column from 1, the column counted in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// From `start` up to (not including) `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

/// An error about one place in a YAML file. It displays as its message alone, so wrapping an error
/// in it changes nothing people read.
#[derive(Debug)]
pub struct Located {
    pub stage: Stage,
    /// The setting it's about, as keys from the top (`["destination", "mllp", "port"]`; a list
    /// item by its index). Empty for the whole file, or when it isn't known.
    pub key: Vec<String>,
    /// Where the YAML parser stopped (syntax errors).
    pub position: Option<Position>,
    pub message: String,
}

impl std::fmt::Display for Located {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Located {}

impl Located {
    /// A broken rule about `key`.
    pub fn rule(key: &[&str], message: impl Into<String>) -> Self {
        Self {
            stage: Stage::Rule,
            key: key.iter().map(|k| k.to_string()).collect(),
            position: None,
            message: message.into(),
        }
    }

    /// The one in an error's chain, if any.
    pub fn of(error: &anyhow::Error) -> Option<&Located> {
        error.chain().find_map(|e| e.downcast_ref::<Located>())
    }

    /// Where it is in `text`, the file it came from: where the parser stopped, the first missing
    /// reference, or the key (as deep as the file has it).
    pub fn span(&self, text: &str) -> Option<Span> {
        if let Some(p) = self.position {
            return Some(Span { start: p, end: p });
        }
        if self.stage == Stage::Reference
            && let Some((_, names)) = self.message.split_once(": ")
            && let Some(first) = names.split(", ").next()
            && let Some(span) = find_text(text, &format!("${{{first}}}"))
        {
            return Some(span);
        }
        let root = tree(text)?;
        let (found, node, complete) = deepest(&root, &self.key);
        // serde's path stops at a flattened field (a destination's connector): look inside what it
        // reached for the key it names, or the value it quotes.
        match self.wanted() {
            Some(Wanted::Key(name)) if !complete => find_in(node, &|k, _| k == name).or(found),
            Some(Wanted::Value(value)) if !matches!(node.kind, Kind::Scalar(_)) => {
                find_in(node, &|_, v| matches!(&v.kind, Kind::Scalar(s) if *s == value)).or(found)
            }
            _ => found,
        }
    }

    /// What a setting's error names: an unknown key, or the value it refused (serde's words:
    /// "unknown field `retries`", "invalid value: integer `70000`", "invalid type: string \"x\"").
    fn wanted(&self) -> Option<Wanted> {
        if self.stage != Stage::Setting {
            return None;
        }
        let m = &self.message;
        if let Some(rest) = m.strip_prefix("unknown field `") {
            return rest.split('`').next().map(|k| Wanted::Key(k.to_string()));
        }
        let rest = m.strip_prefix("invalid value: ").or_else(|| m.strip_prefix("invalid type: "))?;
        let (_, quoted) = rest.split_once([' '])?;
        let value = match quoted.chars().next()? {
            '`' => quoted[1..].split('`').next()?,
            '"' => quoted[1..].split('"').next()?,
            _ => return None,
        };
        Some(Wanted::Value(value.to_string()))
    }
}

enum Wanted {
    Key(String),
    Value(String),
}

/// What a check across interfaces found (two listeners on one port, a route to nowhere): its text,
/// and the interface.yaml and key it's about, for `validate --json`. It reads as its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub text: String,
    /// The interface whose interface.yaml it's in (None: not one interface's).
    pub interface: Option<String>,
    /// The key in that file (empty: the file).
    pub key: Vec<String>,
}

impl Problem {
    pub fn new(text: impl Into<String>, interface: &str, key: &[&str]) -> Self {
        Self { text: text.into(), interface: Some(interface.into()), key: key.iter().map(|k| k.to_string()).collect() }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// So a list of them joins (`problems.join("\n")`) as their texts.
impl std::borrow::Borrow<str> for Problem {
    fn borrow(&self) -> &str {
        &self.text
    }
}

impl PartialEq<&str> for Problem {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<String> for Problem {
    fn eq(&self, other: &String) -> bool {
        &self.text == other
    }
}

/// A failed check of the settings as a broken rule about `key`, with the same text (`{:#}`).
/// An error that already knows its place keeps it.
pub fn at<T>(key: &[&str], result: Result<T>) -> Result<T> {
    result.map_err(|e| if Located::of(&e).is_some() { e } else { Located::rule(key, format!("{e:#}")).into() })
}

/// An error from reading part of a file (`from_value` on its `alerts:`), with that part's key put
/// in front of the one it names.
pub fn under(prefix: &[&str], error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<Located>() {
        Ok(mut located) => {
            located.key.splice(0..0, prefix.iter().map(|k| k.to_string()));
            located.into()
        }
        Err(other) => other,
    }
}

/// Parse a project file's text (`${...}` references allowed anywhere: `env::parse_yaml`).
pub fn parse(text: &str) -> Result<Value> {
    crate::env::parse_yaml(text).map_err(|e| syntax(e, text))
}

/// A parser's error about `text` as a syntax error at the position it gave. An error that already
/// knows its place keeps it.
pub fn syntax(error: anyhow::Error, text: &str) -> anyhow::Error {
    if Located::of(&error).is_some() {
        return error;
    }
    let position = error
        .downcast_ref::<serde_yaml_ng::Error>()
        .and_then(|y| y.location())
        .map(|l| Position { line: l.line() as u32, column: original_column(text, l.line(), l.column()) as u32 });
    Located { stage: Stage::Syntax, key: Vec::new(), position, message: format!("{error:#}") }.into()
}

/// The environment's values put in (`Environment::substitute`), a missing one reported where it's
/// written.
pub fn substitute(env: &crate::Environment, value: &mut Value) -> Result<()> {
    env.substitute(value).map_err(|e| {
        Located { stage: Stage::Reference, key: Vec::new(), position: None, message: format!("{e:#}") }.into()
    })
}

/// Read settings from a parsed file, keeping which setting a failure is about.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|e| {
        use serde_path_to_error::Segment;
        let mut key: Vec<String> = e
            .path()
            .iter()
            .map_while(|s| match s {
                Segment::Map { key } => Some(key.clone()),
                Segment::Enum { variant } => Some(variant.clone()),
                Segment::Seq { index } => Some(index.to_string()),
                Segment::Unknown => None,
            })
            .collect();
        let message = e.inner().to_string();
        // An unknown setting is about its own key, not the map that has it (the path usually ends
        // with it already; not always, inside a `with = ...` field).
        if let Some(field) = message.strip_prefix("unknown field `").and_then(|r| r.split('`').next())
            && key.last().is_none_or(|last| last != field)
        {
            key.push(field.to_string());
        }
        Located { stage: Stage::Setting, key, position: None, message }.into()
    })
}

/// The column (from 1) in `text`'s line `line` that the parser's `column` stands for: the parser
/// read the text with each reference's `${` as one character (`env::parse_yaml`).
fn original_column(text: &str, line: usize, column: usize) -> usize {
    let Some(written) = text.lines().nth(line.saturating_sub(1)) else { return column };
    let chars: Vec<char> = written.chars().collect();
    let (mut parsed, mut original) = (1, 1);
    while parsed < column && original <= chars.len() {
        let at = |n: usize| chars.get(original - 1 + n).copied();
        // `$${` (a literal `${`) was read as one character, and so was a reference's `${`.
        original += match (at(0), at(1), at(2)) {
            (Some('$'), Some('$'), Some('{')) => 3,
            (Some('$'), Some('{'), _) => 2,
            _ => 1,
        };
        parsed += 1;
    }
    original
}

/// Where `needle` first appears in `text`.
pub fn find_text(text: &str, needle: &str) -> Option<Span> {
    let at = text.find(needle)?;
    let start = position_of(text, at);
    Some(Span { start, end: position_of(text, at + needle.len()) })
}

fn position_of(text: &str, byte: usize) -> Position {
    let before = &text[..byte];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    Position { line: line as u32, column: before[line_start..].chars().count() as u32 + 1 }
}

/// Where `key` is written in YAML `text`: the key itself (`port` in `port: 70000`), or a list
/// item, or the deepest part of `key` the file has (a missing `about.to` points at `about`).
/// None when not even its first part is there, or the text isn't YAML.
pub fn locate(text: &str, key: &[String]) -> Option<Span> {
    deepest(&tree(text)?, key).0
}

/// How far down `key` goes in `root`: where its deepest part found is written, the node there,
/// and whether every part was found.
fn deepest<'a>(root: &'a Node, key: &[String]) -> (Option<Span>, &'a Node, bool) {
    let mut node = root;
    let mut found = None;
    for part in key {
        let next = match &node.kind {
            Kind::Map(entries) => entries.iter().find(|(k, ..)| k == part).map(|(_, s, v)| (*s, v)),
            Kind::Seq(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)).map(|item| (item.span, item)),
            Kind::Scalar(_) => None,
        };
        let Some((span, value)) = next else { return (found, node, false) };
        found = Some(span);
        node = value;
    }
    (found, node, true)
}

/// Under `node`, depth first, where the first key `wanted` accepts (with its value) is written.
fn find_in(node: &Node, wanted: &dyn Fn(&str, &Node) -> bool) -> Option<Span> {
    match &node.kind {
        Kind::Map(entries) => entries
            .iter()
            .find_map(|(k, span, value)| if wanted(k, value) { Some(*span) } else { find_in(value, wanted) }),
        Kind::Seq(items) => items.iter().find_map(|item| find_in(item, wanted)),
        Kind::Scalar(_) => None,
    }
}

struct Node {
    span: Span,
    kind: Kind,
}

enum Kind {
    /// Each key's text and where it's written, with its value.
    Map(Vec<(String, Span, Node)>),
    Seq(Vec<Node>),
    /// Its text (an alias's is empty).
    Scalar(String),
}

/// The first document of `text` as a tree of positions.
fn tree(text: &str) -> Option<Node> {
    use saphyr_parser::{Event, Parser};
    // `{` and `}` in references are flow syntax to a YAML parser: same-length stand-ins keep every
    // position where it is.
    let mut readable: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 1 < readable.len() {
        if readable[i] == '$' && readable[i + 1] == '{' {
            readable[i + 1] = '(';
            if let Some(close) = readable[i + 2..].iter().position(|&c| c == '}') {
                readable[i + 2 + close] = ')';
            }
        }
        i += 1;
    }
    let readable: String = readable.into_iter().collect();
    let mut events = Parser::new_from_str(&readable)
        .map_while(Result::ok)
        .filter(|(e, _)| !matches!(e, Event::StreamStart | Event::DocumentStart(_) | Event::Nothing));
    let position = |m: saphyr_parser::Marker| Position { line: m.line() as u32, column: m.col() as u32 + 1 };
    let span = |s: saphyr_parser::Span| Span { start: position(s.start), end: position(s.end) };

    fn node<'a>(
        first: (Event<'a>, saphyr_parser::Span),
        events: &mut impl Iterator<Item = (Event<'a>, saphyr_parser::Span)>,
        span: &impl Fn(saphyr_parser::Span) -> Span,
    ) -> Option<Node> {
        let (event, at) = first;
        Some(match event {
            Event::MappingStart(..) => {
                let mut entries = Vec::new();
                loop {
                    let next = events.next()?;
                    if matches!(next.0, Event::MappingEnd) {
                        break;
                    }
                    let name = match &next.0 {
                        Event::Scalar(text, ..) => text.to_string(),
                        _ => String::new(),
                    };
                    let key = node(next, events, span)?;
                    let value = node(events.next()?, events, span)?;
                    entries.push((name, key.span, value));
                }
                Node { span: span(at), kind: Kind::Map(entries) }
            }
            Event::SequenceStart(..) => {
                let mut items = Vec::new();
                loop {
                    let next = events.next()?;
                    if matches!(next.0, Event::SequenceEnd) {
                        break;
                    }
                    items.push(node(next, events, span)?);
                }
                Node { span: span(at), kind: Kind::Seq(items) }
            }
            Event::Scalar(text, ..) => Node { span: span(at), kind: Kind::Scalar(text.to_string()) },
            Event::Alias(_) => Node { span: span(at), kind: Kind::Scalar(String::new()) },
            _ => return None,
        })
    }
    let first = events.next()?;
    node(first, &mut events, &span)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Vec<String> {
        k.split('.').map(str::to_string).collect()
    }

    fn at(text: &str, k: &str) -> Option<(u32, u32)> {
        locate(text, &key(k)).map(|s| (s.start.line, s.start.column))
    }

    const YAML: &str = "\
name: adt-in   # a comment
source: { mllp: { port: ${PORT}, tls: { certificate: ours } } }
destinations:
  lab:
    mllp: { host: \"${LAB_HOST}\", port: 70000 }
    transform: toLab
  billing:
    http: { api: billing }
about:
  from:
    hosts:
      - 10.0.0.1
      - 10.0.0.2
";

    #[test]
    fn finds_keys_in_block_and_flow_style_with_references_before_them() {
        assert_eq!(at(YAML, "name"), Some((1, 1)));
        assert_eq!(at(YAML, "source.mllp.port"), Some((2, 19)));
        // After a reference on the same line: the stand-ins keep every column.
        assert_eq!(at(YAML, "source.mllp.tls.certificate"), Some((2, 41)));
        assert_eq!(at(YAML, "destinations.lab.mllp.port"), Some((5, 34)));
        assert_eq!(at(YAML, "destinations.lab.transform"), Some((6, 5)));
        assert_eq!(at(YAML, "about.from.hosts.1"), Some((13, 9)));
    }

    #[test]
    fn a_key_the_file_lacks_points_at_the_deepest_part_it_has() {
        assert_eq!(at(YAML, "about.to"), Some((9, 1)), "about");
        assert_eq!(at(YAML, "destinations.billing.http.path"), Some((8, 5)), "http");
        assert_eq!(at(YAML, "retry.max"), None);
        assert_eq!(at("name: [unclosed", "name"), None, "not YAML");
    }

    #[test]
    fn a_setting_s_error_names_its_key() {
        #[derive(serde::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Mllp {
            host: String,
            port: u16,
        }
        #[derive(serde::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct File {
            mllp: Mllp,
        }
        let text = "mllp:\n  host: lab\n  port: 70000\n";
        let e = from_value::<File>(parse(text).unwrap()).unwrap_err();
        let located = Located::of(&e).unwrap();
        assert_eq!((located.stage, located.key.join(".")), (Stage::Setting, "mllp.port".into()));
        assert_eq!(e.to_string(), "invalid value: integer `70000`, expected u16", "the text as before");
        assert_eq!(located.span(text).map(|s| s.start), Some(Position { line: 3, column: 3 }));

        let text = "mllp:\n  host: lab\n  port: 1\n  retries: 3\n";
        let e = from_value::<File>(parse(text).unwrap()).unwrap_err();
        let located = Located::of(&e).unwrap();
        assert_eq!(located.key.join("."), "mllp.retries", "the unknown key itself");
        assert_eq!(located.span(text).map(|s| (s.start.line, s.start.column)), Some((4, 3)));
    }

    #[test]
    fn a_value_refused_inside_a_destination_points_at_its_key() {
        // serde's path stops at `destination` (its connector is a flattened field): the value it
        // quotes is found under it.
        let text =
            "name: y\nsource: { mllp: { port: 7001 } }\ndestination:\n  mllp: { host: localhost, port: 70000 }\n";
        let e = from_value::<crate::InterfaceConfig>(parse(text).unwrap()).unwrap_err();
        let located = Located::of(&e).unwrap();
        assert_eq!(located.key, ["destination"], "{e:#}");
        assert_eq!(located.span(text).map(|s| (s.start.line, s.start.column)), Some((4, 28)));
    }

    #[test]
    fn a_syntax_error_has_the_parser_s_position_in_the_file_as_written() {
        let text = "name: y\nsource: { mllp: { port: 7001 }\n";
        let e = parse(text).unwrap_err();
        let located = Located::of(&e).unwrap();
        assert_eq!(located.stage, Stage::Syntax);
        assert_eq!(located.position.map(|p| p.line), Some(3));
        // A reference earlier on the line: the parser counted `${` as one character.
        let text = "a: { b: ${X}, c: [ }\n";
        let e = parse(text).unwrap_err();
        let p = Located::of(&e).unwrap().position.unwrap();
        assert_eq!(text.chars().nth(p.column as usize - 1), Some('}'), "{p:?}");
    }

    #[test]
    fn a_missing_reference_points_at_where_it_s_written() {
        let text = "a: 1\nhost: ${LAB_HOST}\n";
        let mut value = parse(text).unwrap();
        let e = substitute(&crate::Environment::with_variables("dev", &[]), &mut value).unwrap_err();
        assert_eq!(e.to_string(), "missing in environment `dev`: LAB_HOST");
        let span = Located::of(&e).unwrap().span(text).unwrap();
        assert_eq!((span.start, span.end), (Position { line: 2, column: 7 }, Position { line: 2, column: 18 }));
    }

    #[test]
    fn a_rule_keeps_the_text_and_takes_the_key() {
        let e =
            super::at::<()>(&["ordering", "partitions"], Err(anyhow::anyhow!("inner").context("outer"))).unwrap_err();
        assert_eq!(e.to_string(), "outer: inner");
        assert_eq!(Located::of(&e).unwrap().key, ["ordering", "partitions"]);
        // One that knows its place already keeps it.
        let e = super::at::<()>(&["x"], Err(Located::rule(&["y"], "m").into())).unwrap_err();
        assert_eq!(Located::of(&e).unwrap().key, ["y"]);
    }
}
