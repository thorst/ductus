//! Ordering and dedupe keys for every format, read at ingest without running interface code.
//!
//! | format | key path                         | example            |
//! |--------|----------------------------------|--------------------|
//! | hl7    | segment-field.component          | `PID-3.1`          |
//! | json   | dotted, `[n]` 0-based indexes    | `patient.ids[0].mrn` |
//! | xml    | from the root, `[n]` 1-based, `@attr` | `Patient/MRN`, `Item[2]/@code` |
//! | csv    | column name (or 1-based number without a header) | `mrn` |
//! | fixed  | field name                       | `mrn`              |
//!
//! Missing values read as "". For CSV and fixed-length, the first record is used.

use anyhow::{Context, Result, bail};
use ductus_core::format::{FixedField, Format};

#[derive(Debug, Clone)]
pub enum KeyPath {
    Hl7(ductus_hl7::Path),
    Json(Vec<JsonStep>),
    Xml(Vec<XmlStep>),
    Csv(String),
    Fixed(FixedField),
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonStep {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum XmlStep {
    Element { name: String, index: usize },
    Attribute(String),
}

/// Parse a key path for a format (done once, at startup).
pub fn compile(format: &Format, path: &str) -> Result<KeyPath> {
    Ok(match format {
        Format::Hl7 => KeyPath::Hl7(path.parse()?),
        Format::Json => KeyPath::Json(json_steps(path)?),
        Format::Xml => KeyPath::Xml(xml_steps(path)?),
        Format::Csv(_) => KeyPath::Csv(path.to_string()),
        Format::Fixed(layout) => KeyPath::Fixed(
            layout.fields.iter().find(|f| f.name == path).cloned().with_context(|| format!("no field `{path}`"))?,
        ),
        Format::Text => bail!("text messages have no fields to use as a key"),
    })
}

fn json_steps(path: &str) -> Result<Vec<JsonStep>> {
    let mut steps = Vec::new();
    for part in path.split('.') {
        let (name, mut rest) = part.split_once('[').map(|(n, r)| (n, Some(r))).unwrap_or((part, None));
        if !name.is_empty() {
            steps.push(JsonStep::Key(name.to_string()));
        }
        while let Some(r) = rest {
            let (index, after) = r.split_once(']').with_context(|| format!("unclosed [ in `{path}`"))?;
            steps.push(JsonStep::Index(index.parse().with_context(|| format!("bad index in `{path}`"))?));
            rest = after.strip_prefix('[');
            if rest.is_none() && !after.is_empty() {
                bail!("unexpected `{after}` in `{path}`");
            }
        }
    }
    if steps.is_empty() {
        bail!("empty JSON key path");
    }
    Ok(steps)
}

fn xml_steps(path: &str) -> Result<Vec<XmlStep>> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut steps = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if let Some(attr) = part.strip_prefix('@') {
            if i != parts.len() - 1 {
                bail!("`@{attr}` must be the last step of `{path}`");
            }
            steps.push(XmlStep::Attribute(attr.to_string()));
            continue;
        }
        let (name, index) = match part.split_once('[') {
            Some((n, r)) => {
                let index: usize = r
                    .strip_suffix(']')
                    .and_then(|i| i.parse().ok())
                    .with_context(|| format!("bad index in `{path}`"))?;
                if index == 0 {
                    bail!("XML indexes are 1-based in `{path}`");
                }
                (n, index)
            }
            None => (*part, 1),
        };
        steps.push(XmlStep::Element { name: name.to_string(), index });
    }
    if steps.is_empty() {
        bail!("empty XML key path");
    }
    Ok(steps)
}

/// Read a key's value from a payload. Anything missing or unparseable reads as "".
pub fn extract(format: &Format, key: &KeyPath, payload: &str) -> String {
    match (key, format) {
        (KeyPath::Hl7(path), _) => {
            ductus_hl7::Message::parse(payload).map(|m| m.get(path).to_string()).unwrap_or_default()
        }
        (KeyPath::Json(steps), _) => json_value(payload, steps),
        (KeyPath::Xml(steps), _) => xml_value(payload, steps).unwrap_or_default(),
        (KeyPath::Csv(column), Format::Csv(options)) => csv_value(payload, column, options.delimiter, options.header),
        (KeyPath::Fixed(field), _) => {
            let line = payload.lines().find(|l| !l.is_empty()).unwrap_or("");
            line.chars().skip(field.start).take(field.width).collect::<String>().trim().to_string()
        }
        _ => String::new(),
    }
}

fn json_value(payload: &str, steps: &[JsonStep]) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else { return String::new() };
    for step in steps {
        value = match (step, value) {
            (JsonStep::Key(k), serde_json::Value::Object(mut map)) => map.remove(k).unwrap_or_default(),
            (JsonStep::Index(i), serde_json::Value::Array(mut items)) if *i < items.len() => items.swap_remove(*i),
            _ => return String::new(),
        };
    }
    match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn csv_value(payload: &str, column: &str, delimiter: char, header: bool) -> String {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(u8::try_from(delimiter).unwrap_or(b','))
        .has_headers(header)
        .flexible(true)
        .from_reader(payload.as_bytes());
    let index = if header {
        match reader.headers() {
            Ok(h) => h.iter().position(|c| c == column),
            Err(_) => None,
        }
    } else {
        column.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
    };
    let Some(index) = index else { return String::new() };
    reader.records().next().and_then(|r| r.ok()).and_then(|r| r.get(index).map(str::to_string)).unwrap_or_default()
}

/// A minimal XML element tree with the same rules as the SDK's `parseXml`, so a path means
/// the same thing at ingest as in interface code.
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Node>,
}

enum Node {
    Element(Element),
    Text(String),
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    out
}

struct XmlParser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> XmlParser<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start();
        self.pos = self.text.len() - trimmed.len();
    }

    /// Skip past `end`, returning what came before it.
    fn until(&mut self, end: &str) -> Option<&'a str> {
        let i = self.rest().find(end)?;
        let found = &self.rest()[..i];
        self.pos += i + end.len();
        Some(found)
    }

    fn name(&mut self) -> Option<String> {
        let len = self.rest().find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '='))?;
        if len == 0 {
            return None;
        }
        let name = self.rest()[..len].to_string();
        self.pos += len;
        Some(name)
    }

    fn document(&mut self) -> Option<Element> {
        self.pos = usize::from(self.text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
        loop {
            self.skip_space();
            if self.rest().starts_with("<?") {
                self.until("?>")?;
            } else if self.rest().starts_with("<!--") {
                self.until("-->")?;
            } else if self.rest().starts_with("<!DOCTYPE") {
                self.until(">")?;
            } else {
                break;
            }
        }
        self.element()
    }

    fn element(&mut self) -> Option<Element> {
        self.rest().starts_with('<').then_some(())?;
        self.pos += 1;
        let mut el = Element { name: self.name()?, attributes: Vec::new(), children: Vec::new() };
        loop {
            self.skip_space();
            if self.rest().starts_with("/>") {
                self.pos += 2;
                return Some(el);
            }
            if self.rest().starts_with('>') {
                self.pos += 1;
                break;
            }
            let attr = self.name()?;
            self.skip_space();
            self.rest().starts_with('=').then_some(())?;
            self.pos += 1;
            self.skip_space();
            let quote = self.rest().chars().next().filter(|c| *c == '"' || *c == '\'')?;
            self.pos += 1;
            let value = self.until(&quote.to_string())?;
            el.attributes.push((attr, decode_entities(value)));
        }
        loop {
            if self.rest().starts_with("</") {
                self.pos += 2;
                let name = self.name()?;
                (name == el.name).then_some(())?;
                self.until(">")?;
                return Some(el);
            } else if self.rest().starts_with("<![CDATA[") {
                self.pos += 9;
                el.children.push(Node::Text(self.until("]]>")?.to_string()));
            } else if self.rest().starts_with("<!--") {
                self.until("-->")?;
            } else if self.rest().starts_with("<?") {
                self.until("?>")?;
            } else if self.rest().starts_with('<') {
                el.children.push(Node::Element(self.element()?));
            } else {
                let end = self.rest().find('<')?;
                let raw = &self.rest()[..end];
                if !raw.trim().is_empty() {
                    el.children.push(Node::Text(decode_entities(raw)));
                }
                self.pos += end;
            }
        }
    }
}

fn xml_value(payload: &str, steps: &[XmlStep]) -> Option<String> {
    let root = XmlParser { text: payload, pos: 0 }.document()?;
    let mut current = &root;
    for step in steps {
        match step {
            XmlStep::Element { name, index } => {
                current = current
                    .children
                    .iter()
                    .filter_map(|c| match c {
                        Node::Element(e) if e.name == *name => Some(e),
                        _ => None,
                    })
                    .nth(index - 1)?;
            }
            XmlStep::Attribute(a) => return current.attributes.iter().find(|(k, _)| k == a).map(|(_, v)| v.clone()),
        }
    }
    Some(
        current
            .children
            .iter()
            .filter_map(|c| match c {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(format: &Format, path: &str, payload: &str) -> String {
        extract(format, &compile(format, path).unwrap(), payload)
    }

    #[test]
    fn json_keys() {
        let doc = r#"{"patient":{"mrn":"123","ids":[{"v":"A"},{"v":"B"}],"age":42,"none":null}}"#;
        assert_eq!(key(&Format::Json, "patient.mrn", doc), "123");
        assert_eq!(key(&Format::Json, "patient.ids[1].v", doc), "B");
        assert_eq!(key(&Format::Json, "patient.age", doc), "42");
        assert_eq!(key(&Format::Json, "patient.none", doc), "");
        assert_eq!(key(&Format::Json, "patient.ids[9].v", doc), "");
        assert_eq!(key(&Format::Json, "patient.mrn", "not json"), "");
        assert!(compile(&Format::Json, "a[x]").is_err());
    }

    #[test]
    fn xml_keys() {
        let doc = r#"<?xml version="1.0"?><Order id="A1"><Patient><MRN>123</MRN><Name>O&apos;Brien</Name></Patient><Item code="GLU"><Qty>1</Qty></Item><Item code="NA"><Qty>2</Qty></Item><Note><![CDATA[a < b]]></Note><Empty/></Order>"#;
        assert_eq!(key(&Format::Xml, "Patient/MRN", doc), "123");
        assert_eq!(key(&Format::Xml, "Patient/Name", doc), "O'Brien");
        assert_eq!(key(&Format::Xml, "Item[2]/Qty", doc), "2");
        assert_eq!(key(&Format::Xml, "Item[2]/@code", doc), "NA");
        assert_eq!(key(&Format::Xml, "@id", doc), "A1");
        assert_eq!(key(&Format::Xml, "Note", doc), "a < b");
        assert_eq!(key(&Format::Xml, "Empty", doc), "");
        assert_eq!(key(&Format::Xml, "Item[3]/Qty", doc), "");
        assert_eq!(key(&Format::Xml, "Missing", doc), "");
        assert!(compile(&Format::Xml, "Item[0]").is_err());
        assert!(compile(&Format::Xml, "@a/B").is_err());
    }

    #[test]
    fn csv_and_fixed_keys() {
        let csv = Format::from_yaml(&serde_yaml_ng::from_str("csv").unwrap()).unwrap();
        assert_eq!(key(&csv, "mrn", "name,mrn\n\"SMITH, J\",123\nDOE,456\n"), "123");
        assert_eq!(key(&csv, "missing", "name,mrn\nA,1\n"), "");
        let headerless =
            Format::from_yaml(&serde_yaml_ng::from_str("{ csv: { header: false, delimiter: '|' } }").unwrap()).unwrap();
        assert_eq!(key(&headerless, "2", "A|1\nB|2\n"), "1");

        let fixed = Format::from_yaml(
            &serde_yaml_ng::from_str("{ fixed: { fields: [ { name: mrn, width: 6 }, { name: name, width: 10 } ] } }")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(key(&fixed, "name", "123456SMITH     \n"), "SMITH");
        assert!(compile(&fixed, "nope").is_err());
        assert!(compile(&Format::Text, "x").is_err());
    }
}
