//! Ordering and dedupe keys read at intake without running interface code. The paths themselves
//! (their syntax per format, filters, `{ regex }`) are `ductus_core::keys`; here they're read from a
//! message, parsed once for all its keys (`Document`).
//!
//! Missing values read as "". For CSV and fixed-length, the first record is used. An HL7
//! component's escape sequences are decoded (`\T\` → `&`), as the SDK's `msg.get` reads it. XML
//! names match by local name unless the format says `names: qualified`. `sdk/tests/key-paths.json`
//! holds cases the SDK's readers are tested against too, so a path means the same in config and
//! code.

use ductus_core::format::{Format, XmlNames};
pub use ductus_core::keys::{JsonStep, KeyFunction, KeyPath, XmlFilter, XmlStep, XmlWhich, compile, compile_all};
use std::cell::OnceCell;

/// A message, parsed for its format the first time a key needs it (a regex reads the text alone).
pub struct Document<'a> {
    format: &'a Format,
    text: &'a str,
    parsed: OnceCell<Option<Parsed<'a>>>,
    /// Its key functions' values, already read (the key thread), with the functions as written.
    scripted: Option<(&'a [KeyFunction], &'a [String])>,
}

enum Parsed<'a> {
    Hl7(ductus_hl7::Message<'a>),
    Json(serde_json::Value),
    Xml(Element),
    /// The header (when the format has one) and the first record.
    Csv(Option<csv::StringRecord>, csv::StringRecord),
    /// The first line.
    Fixed(&'a str),
    Text,
}

impl<'a> Document<'a> {
    pub fn new(format: &'a Format, text: &'a str) -> Self {
        Self { format, text, parsed: OnceCell::new(), scripted: None }
    }

    /// With what its key functions answered for it: `values[i]` is `functions[i]`'s.
    pub fn with_scripts(self, functions: &'a [KeyFunction], values: &'a [String]) -> Self {
        Self { scripted: Some((functions, values)), ..self }
    }

    /// None: not a message of the format (not HL7, not JSON, not XML; an empty file).
    fn parsed(&self) -> Option<&Parsed<'a>> {
        self.parsed
            .get_or_init(|| {
                let text = self.text;
                Some(match self.format {
                    Format::Hl7 => Parsed::Hl7(ductus_hl7::Message::parse(text).ok()?),
                    Format::Json => Parsed::Json(serde_json::from_str(text).ok()?),
                    Format::Xml(_) => Parsed::Xml(XmlParser { text, pos: 0 }.document()?),
                    _ if text.trim().is_empty() => return None,
                    Format::Csv(options) => {
                        let mut reader = csv::ReaderBuilder::new()
                            .delimiter(u8::try_from(options.delimiter).unwrap_or(b','))
                            .has_headers(options.header)
                            .flexible(true)
                            .from_reader(text.as_bytes());
                        let header = if options.header { reader.headers().ok().cloned() } else { None };
                        let record = reader.records().next().and_then(|r| r.ok()).unwrap_or_default();
                        Parsed::Csv(header, record)
                    }
                    Format::Fixed(_) => Parsed::Fixed(text.lines().find(|l| !l.is_empty()).unwrap_or("")),
                    Format::Text => Parsed::Text,
                })
            })
            .as_ref()
    }

    /// A key part's value, or `None` when the message isn't one of the format at all: what
    /// `validate` tells apart when it reads fixtures, since a test's expected output in another
    /// format says nothing about the key. Missing fields in a message of the format read as
    /// `Some("")`.
    pub fn read(&self, key: &KeyPath) -> Option<String> {
        if let KeyPath::Script(function) = key {
            // None without the values (`validate`'s fixtures): nothing can be said about it here.
            let (functions, values) = self.scripted?;
            return functions.iter().position(|f| f == function).and_then(|i| values.get(i)).cloned();
        }
        if let KeyPath::Regex(regex) = key {
            if self.text.trim().is_empty() {
                return None;
            }
            let group = usize::from(regex.captures_len() > 1);
            let found = regex.captures(self.text).and_then(|c| c.get(group));
            return Some(found.map(|m| m.as_str().to_string()).unwrap_or_default());
        }
        Some(match (key, self.parsed()?) {
            (KeyPath::Hl7(path), Parsed::Hl7(message)) => message.read(path).into_owned(),
            (KeyPath::Json(steps), Parsed::Json(value)) => json_text(json_at(value, steps)),
            (KeyPath::Xml(steps), Parsed::Xml(root)) => {
                xml_value(root, steps, self.format.xml_names()).unwrap_or_default()
            }
            (KeyPath::Csv(column), Parsed::Csv(header, record)) => {
                let index = match header {
                    Some(h) => h.iter().position(|c| c == column),
                    None => column.parse::<usize>().ok().and_then(|n| n.checked_sub(1)),
                };
                index.and_then(|i| record.get(i)).unwrap_or_default().to_string()
            }
            (KeyPath::Fixed(field), Parsed::Fixed(line)) => {
                line.chars().skip(field.start).take(field.width).collect::<String>().trim().to_string()
            }
            _ => String::new(),
        })
    }

    /// A key's value with all its parts (`key: [MSH-4, PID-3.1]`): joined by `/`, and empty when
    /// any part is (the key doesn't identify the message without all of them). `None` as `read`.
    pub fn read_all(&self, parts: &[KeyPath]) -> Option<String> {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            values.push(self.read(part)?);
        }
        Some(if values.iter().any(String::is_empty) { String::new() } else { values.join(KEY_SEPARATOR) })
    }
}

/// Between a composite key's values: visible where keys are shown (the message's page, hotspots).
/// A value holding it can only merge two keys' lanes, which keeps each in order.
pub const KEY_SEPARATOR: &str = "/";

/// Read one key part from a payload. Anything missing or unparseable reads as "".
pub fn extract(format: &Format, key: &KeyPath, payload: &str) -> String {
    Document::new(format, payload).read(key).unwrap_or_default()
}

/// `Document::read` on a payload of its own.
pub fn read(format: &Format, key: &KeyPath, payload: &str) -> Option<String> {
    Document::new(format, payload).read(key)
}

fn json_at<'v>(value: &'v serde_json::Value, steps: &[JsonStep]) -> Option<&'v serde_json::Value> {
    use serde_json::Value;
    let mut current = value;
    for step in steps {
        current = match (step, current) {
            (JsonStep::Key(k), Value::Object(map)) => map.get(k)?,
            (JsonStep::Index(i), Value::Array(items)) => items.get(*i)?,
            (JsonStep::Where(filters), Value::Array(items)) => {
                filters.iter().find_map(|f| items.iter().find(|item| json_text(json_at(item, &f.path)) == f.value))?
            }
            (JsonStep::Where(filters), item @ Value::Object(_)) => {
                filters.iter().any(|f| json_text(json_at(item, &f.path)) == f.value).then_some(item)?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// A JSON value as a key: text as it is, nothing or `null` as "", anything else as JSON.
fn json_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        None | Some(serde_json::Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
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

impl Element {
    fn elements<'e>(&'e self, name: &'e str, names: XmlNames) -> impl Iterator<Item = &'e Element> {
        self.children.iter().filter_map(move |c| match c {
            Node::Element(e) if names.matches(name, &e.name) => Some(e),
            _ => None,
        })
    }

    /// An attribute by name; namespace declarations (`xmlns:hl7`) are never one.
    fn attribute(&self, name: &str, names: XmlNames) -> Option<&str> {
        let declaration = |k: &str| k == "xmlns" || k.starts_with("xmlns:");
        self.attributes.iter().find(|(k, _)| !declaration(k) && names.matches(name, k)).map(|(_, v)| v.as_str())
    }

    /// Text directly inside it.
    fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|c| match c {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    fn matches(&self, filter: &XmlFilter, names: XmlNames) -> bool {
        let value = match filter.attribute {
            true => self.attribute(&filter.name, names).unwrap_or_default().to_string(),
            false => self.elements(&filter.name, names).next().map(Element::text).unwrap_or_default(),
        };
        value == filter.value
    }
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

/// From the root's children: the text at a path, or an attribute at its last step.
fn xml_value(root: &Element, steps: &[XmlStep], names: XmlNames) -> Option<String> {
    let mut current = root;
    for step in steps {
        match step {
            XmlStep::Element { name, which } => {
                current = match which {
                    XmlWhich::Index(n) => current.elements(name, names).nth(n - 1)?,
                    XmlWhich::Where(filters) => {
                        filters.iter().find_map(|f| current.elements(name, names).find(|e| e.matches(f, names)))?
                    }
                };
            }
            XmlStep::Attribute(a) => return current.attribute(a, names).map(str::to_string),
        }
    }
    Some(current.text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ductus_core::format::XmlOptions;
    use ductus_core::keys::KeyPart;

    fn key(format: &Format, path: &str, payload: &str) -> String {
        extract(format, &compile(format, &path.into()).unwrap(), payload)
    }

    fn xml() -> Format {
        Format::Xml(XmlOptions::default())
    }

    #[test]
    fn json_keys() {
        let doc = r#"{"patient":{"mrn":"123","ids":[{"v":"A"},{"v":"B"}],"age":42,"none":null}}"#;
        assert_eq!(key(&Format::Json, "patient.mrn", doc), "123");
        assert_eq!(key(&Format::Json, "patient.ids[1].v", doc), "B");
        assert_eq!(key(&Format::Json, "patient.age", doc), "42");
        assert_eq!(key(&Format::Json, "patient.none", doc), "");
        assert_eq!(key(&Format::Json, "patient.ids[9].v", doc), "");
        assert_eq!(key(&Format::Json, "patient.ids[v=B].v", doc), "B");
        assert_eq!(key(&Format::Json, "patient.mrn", "not json"), "");
        assert!(compile(&Format::Json, &"a[x]".into()).is_err());
    }

    #[test]
    fn xml_keys() {
        let doc = r#"<?xml version="1.0"?><Order id="A1"><Patient><MRN>123</MRN><Name>O&apos;Brien</Name></Patient><Item code="GLU"><Qty>1</Qty></Item><Item code="NA"><Qty>2</Qty></Item><Note><![CDATA[a < b]]></Note><Empty/></Order>"#;
        assert_eq!(key(&xml(), "Patient/MRN", doc), "123");
        assert_eq!(key(&xml(), "Patient/Name", doc), "O'Brien");
        assert_eq!(key(&xml(), "Item[2]/Qty", doc), "2");
        assert_eq!(key(&xml(), "Item[2]/@code", doc), "NA");
        assert_eq!(key(&xml(), "Item[@code=NA]/Qty", doc), "2");
        assert_eq!(key(&xml(), "Item[Qty=1]/@code", doc), "GLU");
        assert_eq!(key(&xml(), "@id", doc), "A1");
        assert_eq!(key(&xml(), "Note", doc), "a < b");
        assert_eq!(key(&xml(), "Empty", doc), "");
        assert_eq!(key(&xml(), "Item[3]/Qty", doc), "");
        assert_eq!(key(&xml(), "Missing", doc), "");
        assert!(compile(&xml(), &"Item[0]".into()).is_err());
        assert!(compile(&xml(), &"@a/B".into()).is_err());
    }

    #[test]
    fn hl7_keys_read_as_the_sdk_does() {
        let adt = "MSH|^~\\&|A|B|C|D|1||ADT^A08|1|P|2.5\rPID|1||12\\T\\34^^^MRN\r";
        assert_eq!(key(&Format::Hl7, "PID-3.1", adt), "12&34", "escapes decoded, as msg.get reads it");
        assert_eq!(key(&Format::Hl7, "PID-2", adt), "");
    }

    #[test]
    fn regex_keys_read_the_first_group_of_any_format() {
        let regex = |r: &str| compile(&Format::Text, &KeyPart::Regex(r.into())).unwrap();
        let text = "ORDER 77 FOR MRN=12345 AT 10:00";
        assert_eq!(read(&Format::Text, &regex(r"MRN=(\d+)"), text).unwrap(), "12345");
        assert_eq!(read(&Format::Text, &regex(r"ORDER \d+"), text).unwrap(), "ORDER 77", "no group: the match");
        assert_eq!(read(&Format::Text, &regex(r"MRN=(\d+)"), "nothing").unwrap(), "");
        assert_eq!(read(&Format::Text, &regex(r"X(Y)?"), "X").unwrap(), "", "the group didn't take part");
        assert_eq!(read(&Format::Text, &regex(r"MRN=(\d+)"), "  "), None, "an empty file is no message");
        let in_hl7 = compile(&Format::Hl7, &KeyPart::Regex(r"\|MRN(\d+)\^".into())).unwrap();
        assert_eq!(read(&Format::Hl7, &in_hl7, "MSH|^~\\&|A\rPID|1||MRN42^^^X\r").unwrap(), "42");
    }

    #[test]
    fn a_composite_key_is_empty_when_any_part_is() {
        let parts = compile_all(&Format::Hl7, &[KeyPart::from("MSH-4"), "PID-3.1".into()]).unwrap();
        let doc = |text| Document::new(&Format::Hl7, text).read_all(&parts);
        assert_eq!(doc("MSH|^~\\&|A|EAST\rPID|1||123\r").unwrap(), "EAST/123");
        assert_eq!(doc("MSH|^~\\&|A|EAST\rPID|1||\r").unwrap(), "", "no MRN: no key");
        assert_eq!(doc("not HL7"), None);
    }

    #[test]
    fn reading_tells_a_missing_field_from_another_format() {
        let read =
            |format: &Format, path: &str, payload: &str| read(format, &compile(format, &path.into()).unwrap(), payload);
        assert_eq!(read(&Format::Hl7, "PID-3.1", "MSH|^~\\&|A\rEVN|A08\r"), Some(String::new()));
        assert_eq!(read(&Format::Hl7, "PID-3.1", "{\"mrn\":1}"), None);
        assert_eq!(read(&Format::Json, "mrn", "{\"id\":1}"), Some(String::new()));
        assert_eq!(read(&Format::Json, "mrn", "MSH|^~\\&|A\r"), None);
        assert_eq!(read(&xml(), "MRN", "<a><MRN>1</MRN></a>"), Some("1".into()));
        assert_eq!(read(&xml(), "MRN", "not xml"), None);
        let csv = Format::from_yaml(&serde_yaml_ng::from_str("csv").unwrap()).unwrap();
        assert_eq!(read(&csv, "mrn", "  \n"), None);
        assert_eq!(read(&csv, "mrn", "name\nA\n"), Some(String::new()));
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
        assert!(compile(&fixed, &"nope".into()).is_err());
        assert!(compile(&Format::Text, &"x".into()).is_err());
    }

    /// What a key costs intake per message, the message parsed each time as intake does: a plain
    /// path, a filter reading the same value, and a regex. Run in release:
    /// `cargo test --release -p ductus-runtime --lib keys::tests::measure -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_filters_against_plain_paths() {
        let table: serde_json::Value = serde_json::from_str(include_str!("../../../sdk/tests/key-paths.json")).unwrap();
        let message = |name: &str| match &table["messages"][name] {
            serde_json::Value::String(text) => text.clone(),
            document => document.to_string(),
        };
        let regex = |r: &str| KeyPart::Regex(r.into());
        let cases: [(Format, &str, KeyPart, KeyPart); 4] = [
            (Format::Hl7, "adt", "PID-3[2].1".into(), "PID-3[.5=MR].1".into()),
            (Format::Json, "patient", "identifier[1].value".into(), "identifier[system=urn:oid:1.2.3].value".into()),
            (
                xml(),
                "cda",
                "recordTarget/patientRole/id[2]/@extension".into(),
                "recordTarget/patientRole/id[@root=1.2.3]/@extension".into(),
            ),
            (Format::Hl7, "adt", "PID-3[2].1".into(), regex(r"~([^~^]*)\^\^\^HOSP")),
        ];
        const N: u32 = 200_000;
        for (format, name, plain, other) in cases {
            let text = message(name);
            let time = |part: &KeyPart| {
                let key = compile(&format, part).unwrap();
                assert_eq!(extract(&format, &key, &text), "123", "{part}");
                let start = std::time::Instant::now();
                for _ in 0..N {
                    std::hint::black_box(extract(&format, &key, std::hint::black_box(&text)));
                }
                start.elapsed().as_nanos() as f64 / f64::from(N)
            };
            let (a, b) = (time(&plain), time(&other));
            println!("{:5} {} bytes: `{plain}` {a:.0} ns, `{other}` {b:.0} ns", format.name(), text.len());
        }
    }

    /// The cases the SDK's test suite reads too (`sdk/tests/paths.test.ts`): a path reads the same
    /// value at intake as in interface code, and what one refuses the other does.
    #[test]
    fn the_shared_table_of_key_paths() {
        let table: serde_json::Value = serde_json::from_str(include_str!("../../../sdk/tests/key-paths.json")).unwrap();
        let cases = table["cases"].as_array().unwrap();
        assert!(cases.len() >= 40, "the whole table: {}", cases.len());
        for case in cases {
            let path = case["path"].as_str().unwrap();
            let format = match (case["format"].as_str().unwrap(), case["names"].as_str()) {
                ("hl7", _) => Format::Hl7,
                ("json", _) => Format::Json,
                ("xml", Some("qualified")) => Format::Xml(XmlOptions { names: XmlNames::Qualified }),
                ("xml", _) => xml(),
                (other, _) => panic!("format {other}"),
            };
            let compiled = compile(&format, &path.into());
            if case["refused"] == true {
                assert!(compiled.is_err(), "{path} should be refused: {case}");
                continue;
            }
            let compiled = compiled.unwrap_or_else(|e| panic!("{path}: {e:#}"));
            let message = match &table["messages"][case["message"].as_str().unwrap()] {
                serde_json::Value::String(text) => text.clone(),
                document => document.to_string(),
            };
            assert_eq!(extract(&format, &compiled, &message), case["value"].as_str().unwrap(), "{case}");
        }
    }
}
