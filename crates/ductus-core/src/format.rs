//! Message formats: how a payload is parsed for interface code and for ordering/dedupe keys.
//!
//! ```yaml
//! format: json                     # hl7 (default), json, xml, text, csv, fixed
//! format: { csv: { delimiter: "|", header: true } }
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
    Xml,
    Text,
    Csv(CsvOptions),
    Fixed(FixedLayout),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CsvOptions {
    #[serde(default = "comma")]
    pub delimiter: char,
    /// First line holds column names (records become objects); otherwise records are arrays.
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
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Right,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixedLayoutConfig {
    fields: Vec<FixedFieldConfig>,
    #[serde(default = "yes")]
    trim: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixedFieldConfig {
    name: String,
    width: usize,
    /// 1-based column where the field starts. Default: right after the previous field.
    start: Option<usize>,
    #[serde(default)]
    align: Align,
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
            Format::Xml => "xml",
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
            "xml" => no_options(Format::Xml),
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

    /// Check that an ordering/dedupe key path makes sense for this format.
    pub fn check_key_path(&self, path: &str) -> Result<()> {
        match self {
            Format::Hl7 => Ok(()), // checked by the HL7 path parser at startup
            Format::Text => bail!("text messages have no fields to use as a key"),
            Format::Fixed(layout) if !layout.fields.iter().any(|f| f.name == path) => {
                bail!("no fixed-length field named `{path}`")
            }
            _ if path.trim().is_empty() => bail!("empty key path"),
            _ => Ok(()),
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
    }

    #[test]
    fn resolves_fixed_layouts() {
        let f = parse("{ fixed: { fields: [ { name: mrn, width: 10 }, { name: last, width: 20 }, { name: dob, start: 41, width: 8 } ] } }")
            .unwrap();
        let Format::Fixed(layout) = &f else { panic!() };
        let spans: Vec<_> = layout.fields.iter().map(|x| (x.name.as_str(), x.start, x.width)).collect();
        assert_eq!(spans, [("mrn", 0, 10), ("last", 10, 20), ("dob", 40, 8)]);
        assert!(f.check_key_path("mrn").is_ok());
        assert!(f.check_key_path("nope").is_err());
        assert!(parse("{ fixed: { fields: [ { name: a, width: 0 } ] } }").is_err());
        assert!(parse("fixed").is_err());
    }

    #[test]
    fn serializes_for_interface_code() {
        let json = serde_json::to_string(&parse("{ csv: { delimiter: ';' } }").unwrap()).unwrap();
        assert_eq!(json, r#"{"kind":"csv","delimiter":";","header":true}"#);
    }
}
