//! Message formats: how a payload is parsed for interface code and for ordering/dedupe keys.
//!
//! ```yaml
//! format: json                     # hl7 (default), json, xml, text, csv, fixed
//! format: { csv: { delimiter: "|", header: true } }
//! format: { xml: { names: qualified } }   # names match with their prefix (default: local names)
//! format:
//!   fixed:
//!     fields:
//!       - { name: mrn, width: 10 }
//!       - { name: last_name, width: 20 }
//!       - { name: dob, start: 41, width: 8 }    # start is 1-based; default: right after the previous field
//! output: json                     # optional: serialize results in another format
//! ```

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Format {
    #[default]
    Hl7,
    Json,
    Xml(XmlOptions),
    Text,
    Csv(CsvOptions),
    Fixed(FixedLayout),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct XmlOptions {
    /// How element and attribute names in paths (keys, and `get`, `find`, `set` in code) match the
    /// document's:
    ///
    /// - `local`: by the name without its namespace prefix, on both sides: `Patient` matches
    ///   `<Patient>`, `<hl7:Patient>` and `<v3:Patient>`, so a sender changing its prefixes
    ///   changes nothing. A prefix written in a path is ignored when matching (and kept on
    ///   elements `set` creates).
    /// - `qualified`: exactly as written, prefix included: `hl7:Patient` matches only
    ///   `<hl7:Patient>`. For documents where the same local name means different things under
    ///   two prefixes (a filter on the namespace's attribute often does it too).
    ///
    /// Default: `local`. Example: `format: { xml: { names: qualified } }`.
    #[serde(default)]
    pub names: XmlNames,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum XmlNames {
    #[default]
    Local,
    Qualified,
}

impl XmlNames {
    /// Whether a name in a path matches one in a document.
    pub fn matches(self, path: &str, document: &str) -> bool {
        match self {
            XmlNames::Qualified => path == document,
            XmlNames::Local => crate::keys::local_name(path) == crate::keys::local_name(document),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CsvOptions {
    /// The one character between fields: `,`, `|`, `;` or a tab (`"\t"`). Quote it in YAML.
    /// Default: `","`. Example: `delimiter: "|"`.
    #[serde(default = "comma")]
    pub delimiter: char,
    /// Whether the first line holds the column names. With it, each record is an object by
    /// column name (`row.mrn`) and keys name columns; without it, each record is a list of
    /// fields and keys are column numbers from 1. Default: `true`. Example: `header: false`.
    #[serde(default = "yes")]
    pub header: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self { delimiter: ',', header: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixedLayout {
    /// Resolved fields: 0-based start and width.
    pub fields: Vec<FixedField>,
    /// Trim padding when reading (default true).
    pub trim: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixedField {
    pub name: String,
    pub start: usize,
    pub width: usize,
    /// When writing: `left` (default) or `right` (e.g. amounts).
    pub align: Align,
    /// When writing: padding character (default space; e.g. "0" for numbers).
    pub pad: char,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Right,
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedLayoutConfig {
    /// The record's fields, in order, each with its `name` and `width`: what the code sees as the
    /// record's properties, and what's written back in the same columns. At least one.
    /// Example: `fields: [{ name: mrn, width: 10 }, { name: last_name, width: 20 }]`.
    fields: Vec<FixedFieldConfig>,
    /// Remove the padding around each field's value when reading (`"SMITH     "` reads as
    /// `"SMITH"`). Turn it off where spaces are part of the value. Default: `true`. Example:
    /// `trim: false`.
    #[serde(default = "yes")]
    trim: bool,
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct FixedFieldConfig {
    /// The field's name, as the code reads it (`record.mrn`) and as keys name it. Required.
    /// Example: `name: mrn`.
    name: String,
    /// How many characters the field takes, at least 1. Required. Example: `width: 10`.
    width: usize,
    /// The column the field starts at, counting from 1, for layouts with gaps or fields out of
    /// order. Default: right after the previous field (1 for the first). Example: `start: 41`.
    start: Option<usize>,
    /// When writing: `left` pads after the value (names), `right` before it (amounts, numbers).
    /// Default: `left`. Example: `align: right`.
    #[serde(default)]
    align: Align,
    /// When writing: the character that fills the field's unused width. Default: a space.
    /// Example: `pad: "0"` (amounts as `0000012.50`).
    #[serde(default = "space")]
    pad: char,
}

fn space() -> char {
    ' '
}

fn comma() -> char {
    ','
}

fn yes() -> bool {
    true
}

impl Format {
    pub fn name(&self) -> &'static str {
        match self {
            Format::Hl7 => "hl7",
            Format::Json => "json",
            Format::Xml(_) => "xml",
            Format::Text => "text",
            Format::Csv(_) => "csv",
            Format::Fixed(_) => "fixed",
        }
    }

    /// Parse from YAML: a name (`json`) or a one-key map with options (`{ csv: {...} }`).
    pub fn from_yaml(value: &Value) -> Result<Self> {
        let (name, options) = match value {
            Value::String(s) => (s.as_str(), None),
            Value::Mapping(m) if m.len() == 1 => {
                let (k, v) = m.iter().next().expect("one entry");
                let Some(k) = k.as_str() else { bail!("format must be a name or `{{ name: options }}`") };
                (k, Some(v.clone()))
            }
            _ => bail!("format must be a name (hl7, json, xml, text, csv, fixed) or `{{ name: options }}`"),
        };
        let no_options = |f: Format| match &options {
            None | Some(Value::Null) => Ok(f),
            Some(_) => bail!("format `{name}` takes no options"),
        };
        match name {
            "hl7" => no_options(Format::Hl7),
            "json" => no_options(Format::Json),
            "xml" => Ok(Format::Xml(match options {
                None | Some(Value::Null) => XmlOptions::default(),
                Some(v) => serde_yaml_ng::from_value(v)?,
            })),
            "text" => no_options(Format::Text),
            "csv" => Ok(Format::Csv(match options {
                None | Some(Value::Null) => CsvOptions::default(),
                Some(v) => serde_yaml_ng::from_value(v)?,
            })),
            "fixed" => {
                let Some(v) = options else { bail!("format `fixed` needs `fields`") };
                Ok(Format::Fixed(FixedLayout::resolve(serde_yaml_ng::from_value(v)?)?))
            }
            other => bail!("unknown format `{other}` (hl7, json, xml, text, csv, fixed)"),
        }
    }

    /// How names in XML paths match (`local` for every format but XML's `names: qualified`).
    pub fn xml_names(&self) -> XmlNames {
        match self {
            Format::Xml(o) => o.names,
            _ => XmlNames::Local,
        }
    }
}

impl FixedLayout {
    fn resolve(config: FixedLayoutConfig) -> Result<Self> {
        if config.fields.is_empty() {
            bail!("fixed format needs at least one field");
        }
        let mut next = 0;
        let mut fields = Vec::with_capacity(config.fields.len());
        for f in config.fields {
            if f.width == 0 {
                bail!("fixed field `{}` has width 0", f.name);
            }
            let start = match f.start {
                Some(0) => bail!("fixed field `{}`: start is 1-based", f.name),
                Some(s) => s - 1,
                None => next,
            };
            next = start + f.width;
            fields.push(FixedField { name: f.name, start, width: f.width, align: f.align, pad: f.pad });
        }
        Ok(Self { fields, trim: config.trim })
    }
}

/// serde helper for `format:` / `output:` fields.
pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Format, D::Error> {
    Format::from_yaml(&Value::deserialize(d)?).map_err(serde::de::Error::custom)
}

/// serde helper for an optional `output:` field.
pub fn deserialize_opt<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Format>, D::Error> {
    deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Format> {
        Format::from_yaml(&serde_yaml_ng::from_str(yaml).unwrap())
    }

    #[test]
    fn parses_names_and_options() {
        assert_eq!(parse("json").unwrap(), Format::Json);
        assert_eq!(parse("csv").unwrap(), Format::Csv(CsvOptions::default()));
        assert_eq!(
            parse("{ csv: { delimiter: '|', header: false } }").unwrap(),
            Format::Csv(CsvOptions { delimiter: '|', header: false })
        );
        assert!(parse("yaml").is_err());
        assert!(parse("{ json: { x: 1 } }").is_err());
        assert_eq!(parse("xml").unwrap(), Format::Xml(XmlOptions { names: XmlNames::Local }));
        assert_eq!(parse("{ xml: { names: qualified } }").unwrap().xml_names(), XmlNames::Qualified);
        assert!(parse("{ xml: { names: exact } }").is_err());
    }

    #[test]
    fn xml_names_match_by_local_name_unless_qualified() {
        assert!(XmlNames::Local.matches("Patient", "hl7:Patient"));
        assert!(XmlNames::Local.matches("v3:Patient", "hl7:Patient"), "a prefix in a path is ignored");
        assert!(!XmlNames::Local.matches("Patient", "hl7:Patients"));
        assert!(XmlNames::Qualified.matches("hl7:Patient", "hl7:Patient"));
        assert!(!XmlNames::Qualified.matches("Patient", "hl7:Patient"));
    }

    #[test]
    fn resolves_fixed_layouts() {
        let f = parse("{ fixed: { fields: [ { name: mrn, width: 10 }, { name: last, width: 20 }, { name: dob, start: 41, width: 8 } ] } }")
            .unwrap();
        let Format::Fixed(layout) = &f else { panic!() };
        let spans: Vec<_> = layout.fields.iter().map(|x| (x.name.as_str(), x.start, x.width)).collect();
        assert_eq!(spans, [("mrn", 0, 10), ("last", 10, 20), ("dob", 40, 8)]);
        assert!(crate::keys::compile(&f, &"mrn".into()).is_ok());
        assert!(crate::keys::compile(&f, &"nope".into()).is_err());
        assert!(parse("{ fixed: { fields: [ { name: a, width: 0 } ] } }").is_err());
        assert!(parse("fixed").is_err());
    }

    #[test]
    fn serializes_for_interface_code() {
        let json = serde_json::to_string(&parse("{ csv: { delimiter: ';' } }").unwrap()).unwrap();
        assert_eq!(json, r#"{"kind":"csv","delimiter":";","header":true}"#);
        let xml = serde_json::to_string(&parse("xml").unwrap()).unwrap();
        assert_eq!(xml, r#"{"kind":"xml","names":"local"}"#);
    }
}
