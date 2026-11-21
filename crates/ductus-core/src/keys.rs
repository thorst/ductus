//! Ordering and dedupe keys as written in interface.yaml (`ordering.key`, `dedupe.keys`), and the
//! paths they compile to for the interface's format. Compiled once: by `InterfaceConfig::validate`
//! (so a malformed path is refused at its place in the file) and by the intake, which reads them
//! from each message (`ductus_runtime::keys`).
//!
//! | format | a path | with a filter |
//! |---|---|---|
//! | hl7 | `PID-3.1`, `PID-3[2].1`, `PID-3.4.1` | `PID-3[.5=MR].1`, `PID-3[.5=MR\|.4=HOSP].1` |
//! | json | `patient.ids[0].mrn` | `identifier[system=urn:oid:1.2.3].value` |
//! | xml | `Patient/MRN`, `Item[2]/@code` | `id[@root=2.16.840.1]/@extension`, `Item[Code=GLU]/Qty` |
//! | csv | a column's name (its number from 1 without a header) | |
//! | fixed | a field's name | |
//! | any | `{ regex: 'MRN=(\d+)' }` over the message's text: the first group, else the match | |
//!
//! A key can have several parts (`key: [MSH-4, PID-3.1]`). The SDK reads the same paths
//! (`msg.get`, `XmlElement.get`, `getPath`); `sdk/tests/key-paths.json` holds the cases both test.

use crate::format::{FixedField, Format};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
use serde_yaml_ng::Value;
use std::fmt;

/// One part of a key: a path in the interface's format, or a regular expression over the text.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyPart {
    Path(String),
    Regex(String),
}

/// `ordering.key`: one part, or several read together (`[MSH-4, PID-3.1]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Key(pub Vec<KeyPart>);

impl KeyPart {
    fn from_yaml(value: &Value) -> std::result::Result<Self, String> {
        const SHAPE: &str = "a key is a path (`PID-3.1`) or `{ regex: 'MRN=(\\d+)' }`";
        match value {
            Value::String(s) => Ok(KeyPart::Path(s.clone())),
            // A CSV column by its number (`key: 2`).
            Value::Number(n) => Ok(KeyPart::Path(n.to_string())),
            Value::Mapping(m) => {
                let mut regex = None;
                for (k, v) in m {
                    match (k.as_str(), v) {
                        (Some("regex"), Value::String(r)) => regex = Some(r.clone()),
                        (Some("regex"), _) => return Err("`regex` is text: `{ regex: 'MRN=(\\d+)' }`".into()),
                        (Some(other), _) => return Err(format!("unknown field `{other}`: {SHAPE}")),
                        (None, _) => return Err(SHAPE.into()),
                    }
                }
                regex.map(KeyPart::Regex).ok_or_else(|| SHAPE.into())
            }
            _ => Err(SHAPE.into()),
        }
    }
}

impl<'de> Deserialize<'de> for KeyPart {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        KeyPart::from_yaml(&Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let parts = match Value::deserialize(d)? {
            Value::Sequence(items) if items.is_empty() => {
                return Err(serde::de::Error::custom(
                    "an empty list: give the key's paths (`key: [MSH-4, PID-3.1]`), or leave `key` out for one lane",
                ));
            }
            Value::Sequence(items) => items.iter().map(KeyPart::from_yaml).collect(),
            one => KeyPart::from_yaml(&one).map(|p| vec![p]),
        };
        parts.map(Key).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for KeyPart {
    /// As written: a path as it is, a regex as `{ regex: '…' }`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyPart::Path(p) => f.write_str(p),
            KeyPart::Regex(r) => write!(f, "{{ regex: '{}' }}", r.replace('\'', "''")),
        }
    }
}

impl fmt::Display for Key {
    /// One part as it is (`PID-3.1`: what lane generations recorded before keys had parts), several
    /// as a list (`[MSH-4, PID-3.1]`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_slice() {
            [one] => one.fmt(f),
            parts => write!(f, "[{}]", parts.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
        }
    }
}

impl From<&str> for KeyPart {
    fn from(path: &str) -> Self {
        KeyPart::Path(path.to_string())
    }
}

/// A key part compiled for a format.
#[derive(Debug, Clone)]
pub enum KeyPath {
    Hl7(ductus_hl7::Path),
    Json(Vec<JsonStep>),
    Xml(Vec<XmlStep>),
    Csv(String),
    Fixed(FixedField),
    Regex(regex::Regex),
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonStep {
    Key(String),
    /// From 0.
    Index(usize),
    /// In an array, the first item the first filter matches, else the second filter's, …; on an
    /// object, the object itself if a filter matches.
    Where(Vec<JsonFilter>),
}

/// `system=urn:oid:1.2.3`: an item whose `system` reads `urn:oid:1.2.3`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonFilter {
    pub path: Vec<JsonStep>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum XmlStep {
    Element { name: String, which: XmlWhich },
    Attribute(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum XmlWhich {
    /// From 1.
    Index(usize),
    /// The first child the first filter matches, else the second filter's, ….
    Where(Vec<XmlFilter>),
}

/// `@root=2.16.840.1` (an attribute) or `Code=GLU` (a child element's text).
#[derive(Debug, Clone, PartialEq)]
pub struct XmlFilter {
    pub attribute: bool,
    pub name: String,
    pub value: String,
}

/// Compile a key part for a format (at startup and in `validate`).
pub fn compile(format: &Format, part: &KeyPart) -> Result<KeyPath> {
    let path = match part {
        KeyPart::Regex(r) => {
            let regex = regex::Regex::new(r).with_context(|| format!("invalid regex `{r}`"))?;
            return Ok(KeyPath::Regex(regex));
        }
        KeyPart::Path(path) => path,
    };
    if path.trim().is_empty() {
        bail!("an empty key path");
    }
    Ok(match format {
        Format::Hl7 => KeyPath::Hl7(path.parse()?),
        Format::Json => KeyPath::Json(json_path(path)?),
        Format::Xml(_) => KeyPath::Xml(xml_path(path)?),
        Format::Csv(_) => KeyPath::Csv(path.clone()),
        Format::Fixed(layout) => {
            KeyPath::Fixed(layout.fields.iter().find(|f| f.name == *path).cloned().with_context(|| {
                let names: Vec<&str> = layout.fields.iter().map(|f| f.name.as_str()).collect();
                format!("no fixed-length field named `{path}` (the fields: {})", names.join(", "))
            })?)
        }
        Format::Text => bail!("text messages have no fields: read the key with `{{ regex: 'MRN=(\\d+)' }}`"),
    })
}

/// Compile every part of a key.
pub fn compile_all<'a>(format: &Format, parts: impl IntoIterator<Item = &'a KeyPart>) -> Result<Vec<KeyPath>> {
    parts.into_iter().map(|p| compile(format, p).with_context(|| format!("key `{p}`"))).collect()
}

/// Where the `]` closing a bracket is (`text` starts inside it), past quoted values.
fn closing(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') if depth == 0 => return Some(i),
            (None, ']') => depth -= 1,
            (None, _) => {}
        }
    }
    None
}

/// `text` split at `sep` where it's outside brackets and quotes.
fn split_outside(text: &str, sep: char) -> Vec<&str> {
    let (mut parts, mut start, mut depth, mut quote) = (Vec::new(), 0, 0, None);
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            (None, c) if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A filter's value: quoted (`'A|B'`) or as written.
fn filter_value(text: &str) -> Result<String> {
    match text.chars().next() {
        Some(q @ ('\'' | '"')) => match text[1..].strip_suffix(q) {
            Some(inner) if !inner.contains(q) => Ok(inner.to_string()),
            _ => bail!("a quoted value runs from one quote to the next: `{text}`"),
        },
        _ if text.contains(['\'', '"', '[', ']']) => bail!("quote a value with brackets or quotes in it: `{text}`"),
        _ => Ok(text.to_string()),
    }
}

/// A number from `min`.
fn number(text: &str, min: usize) -> Option<usize> {
    text.parse().ok().filter(|n| *n >= min && text.bytes().all(|b| b.is_ascii_digit()))
}

/// JSON: names between dots, `[n]` from 0, `[name=value]` filters (`|` between alternatives).
fn json_path(path: &str) -> Result<Vec<JsonStep>> {
    json_steps(path).with_context(|| {
        format!("invalid JSON key path `{path}` (e.g. `patient.mrn`, `ids[0].value`, `identifier[system=urn:oid:1.2.3].value`)")
    })
}

fn json_steps(path: &str) -> Result<Vec<JsonStep>> {
    let mut steps = Vec::new();
    for part in split_outside(path, '.') {
        let name_end = part.find('[').unwrap_or(part.len());
        let name = &part[..name_end];
        if name.contains([']', '=', '|', '\'', '"']) {
            bail!("`{name}` isn't a name");
        }
        if !name.is_empty() {
            steps.push(JsonStep::Key(name.to_string()));
        } else if name_end == part.len() {
            bail!("an empty name between dots");
        }
        let mut rest = &part[name_end..];
        while let Some(inside) = rest.strip_prefix('[') {
            let end = closing(inside).context("a `[` without its `]`")?;
            let bracket = &inside[..end];
            steps.push(match number(bracket, 0) {
                Some(i) => JsonStep::Index(i),
                None if bracket.bytes().all(|b| b.is_ascii_digit()) => bail!("an empty `[]`"),
                None => JsonStep::Where(
                    split_outside(bracket, '|')
                        .into_iter()
                        .map(|alt| {
                            let (on, value) = split_outside(alt, '=')
                                .split_first()
                                .filter(|(_, v)| !v.is_empty())
                                .map(|(on, v)| (*on, v.join("=")))
                                .with_context(|| format!("a filter is `name=value`: `{alt}`"))?;
                            Ok(JsonFilter { path: json_steps(on)?, value: filter_value(&value)? })
                        })
                        .collect::<Result<_>>()?,
                ),
            });
            rest = &inside[end + 1..];
        }
        if !rest.is_empty() {
            bail!("`{rest}` after `]`");
        }
    }
    Ok(steps)
}

/// XML: element names between `/` (by local name unless the format's `names: qualified`), `[n]`
/// from 1, `[@attr=value]` or `[Child=value]` filters, `@attr` last.
fn xml_path(path: &str) -> Result<Vec<XmlStep>> {
    xml_steps(path).with_context(|| {
        format!(
            "invalid XML key path `{path}` (e.g. `Patient/MRN`, `Item[2]/@code`, `id[@root=2.16.840.1]/@extension`)"
        )
    })
}

fn xml_steps(path: &str) -> Result<Vec<XmlStep>> {
    let parts: Vec<&str> = split_outside(path, '/').into_iter().filter(|p| !p.is_empty()).collect();
    let mut steps = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if let Some(attribute) = part.strip_prefix('@') {
            if i != parts.len() - 1 {
                bail!("`@{attribute}` must be the last step");
            }
            name(attribute)?;
            steps.push(XmlStep::Attribute(attribute.to_string()));
            continue;
        }
        let (element, which) = match part.split_once('[') {
            None => (*part, XmlWhich::Index(1)),
            Some((element, inside)) => {
                let end = closing(inside).context("a `[` without its `]`")?;
                if end + 1 != inside.len() {
                    bail!("`{}` after `]`", &inside[end + 1..]);
                }
                let bracket = &inside[..end];
                let which = match number(bracket, 1) {
                    Some(n) => XmlWhich::Index(n),
                    None if bracket.bytes().all(|b| b.is_ascii_digit()) => bail!("XML positions count from 1"),
                    None => XmlWhich::Where(
                        split_outside(bracket, '|')
                            .into_iter()
                            .map(|alt| {
                                let (on, value) = alt
                                    .split_once('=')
                                    .with_context(|| format!("a filter is `@attr=value`: `{alt}`"))?;
                                let (attribute, on) = match on.strip_prefix('@') {
                                    Some(a) => (true, a),
                                    None => (false, on),
                                };
                                Ok(XmlFilter { attribute, name: name(on)?, value: filter_value(value)? })
                            })
                            .collect::<Result<_>>()?,
                    ),
                };
                (element, which)
            }
        };
        steps.push(XmlStep::Element { name: name(element)?, which });
    }
    if steps.is_empty() {
        bail!("no steps");
    }
    Ok(steps)
}

/// An XML name as written in a path.
fn name(text: &str) -> Result<String> {
    if text.is_empty() || text.contains(|c: char| c.is_whitespace() || "[]=|'\"@/".contains(c)) {
        bail!("`{text}` isn't an element or attribute name");
    }
    Ok(text.to_string())
}

/// A name without its namespace prefix (`hl7:Patient` → `Patient`).
pub fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(yaml: &str) -> std::result::Result<KeyPart, String> {
        serde_yaml_ng::from_str(yaml).map_err(|e| e.to_string())
    }

    #[test]
    fn keys_are_written_as_a_path_a_regex_or_a_list() {
        assert_eq!(part("PID-3.1").unwrap(), KeyPart::Path("PID-3.1".into()));
        assert_eq!(part("2").unwrap(), KeyPart::Path("2".into()), "a CSV column's number");
        assert_eq!(part("{ regex: 'MRN=(\\d+)' }").unwrap(), KeyPart::Regex("MRN=(\\d+)".into()));
        assert!(part("{ script: key.ts }").unwrap_err().contains("unknown field `script`"));
        assert!(part("{ regex: 1 }").unwrap_err().contains("`regex` is text"));
        let key: Key = serde_yaml_ng::from_str("[MSH-4, { regex: 'x' }]").unwrap();
        assert_eq!(key.to_string(), "[MSH-4, { regex: 'x' }]");
        let one: Key = serde_yaml_ng::from_str("PID-3.1").unwrap();
        assert_eq!(one.to_string(), "PID-3.1", "one path as written: what lane generations recorded");
        assert!(serde_yaml_ng::from_str::<Key>("[]").unwrap_err().to_string().contains("leave `key` out"));
    }

    #[test]
    fn json_paths() {
        use JsonStep::*;
        let compiled = json_path("identifier[system=urn:oid:1.2.3|type.coding[0].code=MR].value").unwrap();
        assert_eq!(
            compiled,
            [
                Key("identifier".into()),
                Where(vec![
                    JsonFilter { path: vec![Key("system".into())], value: "urn:oid:1.2.3".into() },
                    JsonFilter {
                        path: vec![Key("type".into()), Key("coding".into()), Index(0), Key("code".into())],
                        value: "MR".into()
                    },
                ]),
                Key("value".into()),
            ]
        );
        assert_eq!(json_path("[0].a").unwrap(), [Index(0), Key("a".into())]);
        assert_eq!(
            json_path("a[x='p.q|r']").unwrap()[1],
            Where(vec![JsonFilter { path: vec![Key("x".into())], value: "p.q|r".into() }])
        );
        for bad in ["a..b", "a[", "a[x]", "a[=1]", "a[x=1]b", "a[]", "a[x='1]", "a]b"] {
            assert!(json_path(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn xml_paths() {
        let compiled = xml_path("id[@root=2.16.840.1|Code=X]/@extension").unwrap();
        assert_eq!(
            compiled,
            [
                XmlStep::Element {
                    name: "id".into(),
                    which: XmlWhich::Where(vec![
                        XmlFilter { attribute: true, name: "root".into(), value: "2.16.840.1".into() },
                        XmlFilter { attribute: false, name: "Code".into(), value: "X".into() },
                    ])
                },
                XmlStep::Attribute("extension".into()),
            ]
        );
        assert_eq!(xml_path("a/b[2]").unwrap()[1], XmlStep::Element { name: "b".into(), which: XmlWhich::Index(2) });
        for bad in ["", "a[0]", "@a/b", "a[", "a[x]", "a[1]b", "a b"] {
            assert!(xml_path(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn compiling_names_the_format_and_the_fix() {
        let text = compile(&Format::Text, &"x".into()).unwrap_err().to_string();
        assert!(text.contains("regex"), "{text}");
        assert!(compile(&Format::Text, &KeyPart::Regex("MRN=(\\d+)".into())).is_ok());
        let bad = compile(&Format::Text, &KeyPart::Regex("(".into())).unwrap_err();
        assert!(format!("{bad:#}").contains("invalid regex `(`"), "{bad:#}");
        let hl7 = compile(&Format::Hl7, &"PID-3[.5]".into()).unwrap_err().to_string();
        assert!(hl7.contains("a filter is `.component=value`"), "{hl7}");
    }
}
