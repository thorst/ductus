//! Company-defined documentation fields: `docs-schema.yaml` declares them, `custom:` holds the
//! values (on an interface, a destination, or a resources.yaml entry), and `ductus validate`
//! checks one against the other.
//!
//! ```yaml
//! fields:
//!   baa_signed:
//!     type: enum                 # text, enum, date, url, person, list
//!     values: ["yes", "no", n/a] # enum (and list) only; exact match, case included
//!     required: true             # every place the field applies to must have a value
//!     applies_to: [interface, resource]   # default: [interface]
//!     label: BAA signed          # shown in the UI instead of the key
//!     help: Business associate agreement   # a hint under the field
//! ```

use crate::InterfaceConfig;
use crate::resources::Resources;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "docs-schema.yaml";

/// The values of the declared fields, by key. Plain YAML values (a list is a YAML list).
pub type Custom = BTreeMap<String, serde_json::Value>;

/// Names ductus itself uses in the places `custom` sits beside.
const RESERVED: &[&str] =
    &["name", "description", "owner", "from", "to", "application", "vendor", "hosts", "custom", "about"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Text,
    Enum,
    Date,
    Url,
    Person,
    List,
}

/// Where a field applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Interface,
    Destination,
    Resource,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Interface => "interface",
            Level::Destination => "destination",
            Level::Resource => "resource",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// What the value is:
    ///
    /// - `text`: any text (quote numbers and yes/no, or YAML reads them as such).
    /// - `enum`: one of `values`.
    /// - `date`: a date, written `2026-10-30`.
    /// - `url`: an `http(s)://` address, shown as a link.
    /// - `person`: who, as text: best an email address.
    /// - `list`: several texts (each one of `values`, if given).
    ///
    /// Required. Example: `type: enum`.
    #[serde(rename = "type")]
    pub kind: FieldType,
    /// The values an `enum` may take (and each item of a `list`, if given), matched exactly,
    /// case included. Quote ones YAML would read otherwise (`"yes"`, `"no"`). Default: none.
    /// Example: `values: ["yes", "no", n/a]`.
    #[serde(default)]
    pub values: Vec<String>,
    /// Every interface (destination, resource) the field applies to must have a value: `ductus
    /// validate` reports each one missing. The engine starts anyway. Default: `false`. Example:
    /// `required: true`.
    #[serde(default)]
    pub required: bool,
    /// Where the field may be given: `interface`, `destination` and/or `resource` (a
    /// resources.yaml entry). Default: `[interface]`. Example: `applies_to: [interface,
    /// resource]`.
    #[serde(default = "default_applies_to")]
    pub applies_to: Vec<Level>,
    /// The name the web UI shows instead of the key. Default: the key. Example: `label: BAA
    /// signed`.
    #[serde(default)]
    pub label: Option<String>,
    /// A hint shown under the field in the web UI: what it's for, or where to find the value.
    /// Default: none. Example: `help: Business associate agreement, from the contracts office`.
    #[serde(default)]
    pub help: Option<String>,
}

fn default_applies_to() -> Vec<Level> {
    vec![Level::Interface]
}

/// `docs-schema.yaml`: the documentation fields your company keeps about its interfaces,
/// destinations and resources, beyond what Ductus asks for (a BAA reference, a change ticket,
/// a runbook link, the analyst). Values go in `custom:` beside each one; `ductus validate`
/// checks them, and the web UI shows and filters by them. The engine itself ignores them.
///
/// The file sits at the top of the project and is optional: without it there are no fields.
#[derive(Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct SchemaFile {
    /// The fields, by the key `custom:` uses, in the order the web UI shows them. Keys are your
    /// choice, except names Ductus uses beside `custom` (`name`, `owner`, `vendor`, …). Default:
    /// none. Example: `fields: { baa_signed: { type: enum, values: ["yes", "no"], required: true
    /// } }`.
    #[serde(default)]
    #[cfg_attr(test, schemars(with = "BTreeMap<String, Field>"))]
    fields: serde_yaml_ng::Mapping,
}

/// A declared field with its key.
#[derive(Debug, Clone)]
pub struct Declared {
    pub key: String,
    pub field: Field,
}

impl Declared {
    /// What people read: the label, else the key.
    pub fn title(&self) -> &str {
        self.field.label.as_deref().unwrap_or(&self.key)
    }
}

/// The fields a company declared, in the order the file lists them.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub fields: Vec<Declared>,
}

impl Schema {
    /// `<root>/docs-schema.yaml`, or no fields if there's none.
    pub fn load(root: &Path) -> Result<Self> {
        let file = root.join(FILE);
        if !file.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", file.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let file: SchemaFile = serde_yaml_ng::from_str(text)?;
        let mut fields = Vec::new();
        for (key, value) in file.fields {
            let key = key.as_str().context("a field name must be text")?.to_string();
            let field: Field = serde_yaml_ng::from_value(value).with_context(|| format!("field `{key}`"))?;
            check_declaration(&key, &field)?;
            fields.push(Declared { key, field });
        }
        Ok(Self { fields })
    }

    pub fn get(&self, key: &str) -> Option<&Declared> {
        self.fields.iter().find(|d| d.key == key)
    }

    /// The fields that apply at a level, in declared order.
    pub fn for_level(&self, level: Level) -> impl Iterator<Item = &Declared> {
        self.fields.iter().filter(move |d| d.field.applies_to.contains(&level))
    }

    /// What's wrong with `custom` at `level`, one plain sentence each (the caller says where).
    pub fn check(&self, level: Level, custom: &Custom) -> Vec<String> {
        let mut problems = Vec::new();
        for (key, value) in custom {
            match self.get(key) {
                None => {
                    let mut message = format!("custom.{key} isn't declared in {FILE}");
                    if let Some(near) = self.for_level(level).map(|d| d.key.as_str()).find(|k| close(k, key)) {
                        message += &format!(" (did you mean `{near}`?)");
                    }
                    problems.push(message);
                }
                Some(d) if !d.field.applies_to.contains(&level) => problems.push(format!(
                    "custom.{key} doesn't apply to a {} (it applies to: {})",
                    level.label(),
                    d.field.applies_to.iter().map(|l| l.label()).collect::<Vec<_>>().join(", ")
                )),
                Some(d) => {
                    if !is_blank(value)
                        && let Err(why) = d.check_value(value)
                    {
                        problems.push(format!("custom.{key} {why}"));
                    }
                }
            }
        }
        for d in self.for_level(level) {
            if d.field.required && custom.get(&d.key).is_none_or(is_blank) {
                problems.push(format!("custom.{} is required{}", d.key, d.expects()));
            }
        }
        problems
    }

    /// The keys of required fields that have no value at `level`.
    pub fn missing(&self, level: Level, custom: &Custom) -> Vec<String> {
        self.for_level(level)
            .filter(|d| d.field.required && custom.get(&d.key).is_none_or(is_blank))
            .map(|d| d.key.clone())
            .collect()
    }
}

fn check_declaration(key: &str, field: &Field) -> Result<()> {
    let mut chars = key.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("field `{key}`: names are lower case letters, digits and underscores, starting with a letter");
    }
    if RESERVED.contains(&key) {
        bail!("field `{key}`: ductus already uses that name; pick another");
    }
    if field.applies_to.is_empty() {
        bail!("field `{key}`: `applies_to` is empty; name at least one of interface, destination, resource");
    }
    match field.kind {
        FieldType::Enum if field.values.is_empty() => bail!("field `{key}`: an enum needs `values`"),
        FieldType::Enum | FieldType::List => {}
        _ if !field.values.is_empty() => bail!("field `{key}`: `values` belongs to type enum or list"),
        _ => {}
    }
    Ok(())
}

impl Declared {
    /// ` (one of: a, b)`, or what kind of value it wants.
    fn expects(&self) -> String {
        match self.field.kind {
            FieldType::Enum => format!(" (one of: {})", self.field.values.join(", ")),
            FieldType::Date => " (a date, 2026-10-13)".into(),
            FieldType::Url => " (an http(s) address)".into(),
            FieldType::List if !self.field.values.is_empty() => {
                format!(" (a list of: {})", self.field.values.join(", "))
            }
            FieldType::List => " (a list)".into(),
            FieldType::Text | FieldType::Person => String::new(),
        }
    }

    fn check_value(&self, value: &serde_json::Value) -> std::result::Result<(), String> {
        use serde_json::Value;
        let text = |v: &Value| v.as_str().map(str::trim).map(String::from);
        match self.field.kind {
            FieldType::Text | FieldType::Person => {
                text(value).map(|_| ()).ok_or_else(|| "must be text (put numbers and yes/no in quotes)".to_string())
            }
            FieldType::Enum => {
                let Some(v) = text(value) else { return Err(format!("must be text{}", self.expects())) };
                self.check_allowed(&v)
            }
            FieldType::Date => {
                let v = text(value).ok_or_else(|| "must be a date, written 2026-10-13".to_string())?;
                chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d")
                    .map(|_| ())
                    .map_err(|_| format!("`{v}` isn't a date; write it as 2026-10-13"))
            }
            FieldType::Url => {
                let v = text(value).ok_or_else(|| "must be an http(s) address".to_string())?;
                match url::Url::parse(&v) {
                    Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Ok(()),
                    _ => Err(format!("`{v}` isn't an http(s) address (https://wiki.example.org/runbook)")),
                }
            }
            FieldType::List => {
                let Value::Array(items) = value else { return Err(format!("must be a list{}", self.expects())) };
                for item in items {
                    let v = text(item).filter(|v| !v.is_empty()).ok_or("has an item that isn't text")?;
                    if !self.field.values.is_empty() {
                        self.check_allowed(&v)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn check_allowed(&self, v: &str) -> std::result::Result<(), String> {
        if self.field.values.iter().any(|a| a == v) {
            return Ok(());
        }
        let hint = match self.field.values.iter().find(|a| a.eq_ignore_ascii_case(v)) {
            Some(exact) => format!(" (case matters: write `{exact}`)"),
            None => String::new(),
        };
        Err(format!("is `{v}`; allowed: {}{hint}", self.field.values.join(", ")))
    }
}

/// A value that counts as not filled in.
pub fn is_blank(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// One edit away or a different case: a typo of a declared key.
fn close(declared: &str, typed: &str) -> bool {
    if declared.eq_ignore_ascii_case(typed) {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (declared.chars().collect(), typed.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    // Levenshtein distance, two rows; keys are short.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            row.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b.len()] <= 2
}

/// One documented thing: an interface, one of its destinations, or a resources.yaml entry.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub level: Level,
    /// `adt-lab`, `adt-lab.default`, `api empi`.
    pub name: String,
    /// The interface it belongs to (an interface, or one of its destinations); none for a resource.
    pub interface: Option<String>,
    pub custom: Custom,
}

/// Everything that can carry documentation fields, in interface, destination, resource order.
pub fn entries<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Entry> {
    let mut all = Vec::new();
    for c in configs {
        all.push(Entry {
            level: Level::Interface,
            name: c.name.clone(),
            interface: Some(c.name.clone()),
            custom: c.custom.clone(),
        });
        for (name, d) in c.destinations() {
            all.push(Entry {
                level: Level::Destination,
                name: format!("{}.{name}", c.name),
                interface: Some(c.name.clone()),
                custom: d.custom.clone(),
            });
        }
    }
    for (kind, name, custom) in resources.customs() {
        all.push(Entry {
            level: Level::Resource,
            name: format!("{} {name}", kind.label()),
            interface: None,
            custom: custom.clone(),
        });
    }
    all
}

/// Every problem in the project's values, each starting with where (`adt-lab:`).
pub fn check_project<'a>(
    schema: &Schema,
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    resources: &Resources,
) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in entries(configs, resources) {
        let place = match entry.level {
            Level::Resource => format!("resources.yaml {}", entry.name),
            _ => entry.name.clone(),
        };
        lines.extend(schema.check(entry.level, &entry.custom).into_iter().map(|p| format!("{place}: {p}")));
    }
    lines
}

/// What the documentation snapshot stores (migration 0017): `(kind, name, JSON)` per interface
/// (its description, owner, `custom`, and each destination's `custom`) and per resource.
pub fn snapshot<'a>(
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    resources: &Resources,
) -> Vec<(&'static str, String, serde_json::Value)> {
    use crate::about::given;
    use serde_json::json;
    let mut rows = Vec::new();
    for c in configs {
        let destinations: serde_json::Map<String, serde_json::Value> =
            c.destinations().map(|(name, d)| (name.to_string(), json!({ "custom": d.custom }))).collect();
        rows.push((
            "interface",
            c.name.clone(),
            json!({
                "description": given(&c.about.description),
                "owner": given(&c.about.owner),
                "custom": c.custom,
                "destinations": destinations,
            }),
        ));
    }
    for (kind, name, custom) in resources.customs() {
        rows.push(("resource", format!("{} {name}", kind.label()), json!({ "custom": custom })));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &str = r#"
fields:
  data_classification: { type: enum, values: [PHI, Internal, Public], required: true }
  baa_signed: { type: enum, values: ["yes", "no", n/a], required: true, applies_to: [interface, resource] }
  go_live: { type: date }
  runbook: { type: url, applies_to: [interface, destination] }
  clinical_contact: { type: person }
  downstream: { type: list, values: [lab, billing] }
  notes: { type: text, label: Notes }
"#;

    fn schema() -> Schema {
        Schema::parse(SCHEMA).unwrap()
    }

    fn custom(v: serde_json::Value) -> Custom {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn fields_keep_the_files_order() {
        let keys: Vec<_> = schema().fields.iter().map(|d| d.key.clone()).collect();
        assert_eq!(
            keys,
            ["data_classification", "baa_signed", "go_live", "runbook", "clinical_contact", "downstream", "notes"]
        );
        assert_eq!(schema().get("notes").unwrap().title(), "Notes");
    }

    #[test]
    fn a_complete_interface_has_no_problems() {
        let c = custom(json!({"data_classification": "PHI", "baa_signed": "yes", "go_live": "2026-10-13",
            "runbook": "https://wiki.example.org/x", "downstream": ["lab"], "notes": "hello"}));
        assert_eq!(schema().check(Level::Interface, &c), Vec::<String>::new());
    }

    #[test]
    fn missing_required_values_name_what_is_allowed() {
        let p = schema().check(Level::Interface, &custom(json!({"baa_signed": "  "})));
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p[0].contains("custom.data_classification is required (one of: PHI, Internal, Public)"), "{p:?}");
        assert!(p[1].contains("custom.baa_signed is required (one of: yes, no, n/a)"), "{p:?}");
        assert_eq!(schema().missing(Level::Interface, &Custom::new()), ["data_classification", "baa_signed"]);
    }

    #[test]
    fn enums_match_exactly_and_say_when_only_the_case_differs() {
        let p = schema().check(Level::Interface, &custom(json!({"data_classification": "phi", "baa_signed": "Yes"})));
        assert!(
            p.iter().any(|m| m.contains("is `phi`; allowed: PHI, Internal, Public (case matters: write `PHI`)")),
            "{p:?}"
        );
        assert!(p.iter().any(|m| m.contains("is `Yes`; allowed: yes, no, n/a (case matters: write `yes`)")), "{p:?}");
    }

    #[test]
    fn dates_urls_lists_and_text_are_checked() {
        let all_good = json!({"data_classification": "PHI", "baa_signed": "no"});
        let with = |extra: serde_json::Value| {
            let mut c = custom(all_good.clone());
            c.extend(custom(extra));
            schema().check(Level::Interface, &c)
        };
        assert!(
            with(json!({"go_live": "13/10/2026"}))[0].contains("`13/10/2026` isn't a date; write it as 2026-10-13")
        );
        assert!(with(json!({"go_live": "2026-02-30"}))[0].contains("isn't a date"));
        assert!(with(json!({"runbook": "ftp://x/y"}))[0].contains("isn't an http(s) address"));
        assert!(with(json!({"runbook": "wiki"}))[0].contains("isn't an http(s) address"));
        assert!(with(json!({"downstream": "lab"}))[0].contains("must be a list"));
        assert!(with(json!({"downstream": ["lab", "pharmacy"]}))[0].contains("is `pharmacy`; allowed: lab, billing"));
        assert!(with(json!({"notes": 5}))[0].contains("must be text"));
        assert!(with(json!({"clinical_contact": true}))[0].contains("must be text"));
        assert!(with(json!({"notes": null, "go_live": ""})).is_empty(), "blank optional values are fine");
    }

    #[test]
    fn undeclared_and_misplaced_fields_are_reported() {
        let c = custom(
            json!({"data_clasification": "PHI", "baa_signed": "yes", "data_classification": "PHI", "go_live": "2026-10-13"}),
        );
        let p = schema().check(Level::Interface, &c);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains(
            "custom.data_clasification isn't declared in docs-schema.yaml (did you mean `data_classification`?)"
        ));
        // A destination takes only the fields that apply to it.
        let p =
            schema().check(Level::Destination, &custom(json!({"runbook": "https://a.b/c", "go_live": "2026-10-13"})));
        assert_eq!(p, ["custom.go_live doesn't apply to a destination (it applies to: interface)"]);
        // A resource: required fields that apply to it only.
        let p = schema().check(Level::Resource, &Custom::new());
        assert_eq!(p, ["custom.baa_signed is required (one of: yes, no, n/a)"]);
    }

    #[test]
    fn without_a_schema_every_value_is_undeclared() {
        let p = Schema::default().check(Level::Interface, &custom(json!({"anything": "x"})));
        assert_eq!(p, ["custom.anything isn't declared in docs-schema.yaml"]);
    }

    #[test]
    fn bad_declarations_are_refused_with_the_fix() {
        let err = |text: &str| format!("{:#}", Schema::parse(text).unwrap_err());
        assert!(err("fields: { x: { type: enum } }").contains("an enum needs `values`"));
        assert!(err("fields: { x: { type: text, values: [a] } }").contains("`values` belongs to type enum or list"));
        assert!(err("fields: { owner: { type: text } }").contains("ductus already uses that name"));
        assert!(err("fields: { Bad-Name: { type: text } }").contains("lower case letters"));
        assert!(err("fields: { x: { type: text, applies_to: [] } }").contains("`applies_to` is empty"));
        assert!(err("fields: { x: { type: number } }").contains("number"));
        assert!(err("fields: { x: { type: text, requierd: true } }").contains("requierd"));
        assert!(err("feilds: {}").contains("feilds"));
    }

    #[test]
    fn typo_detection_is_close_not_loose() {
        assert!(close("baa_signed", "baa_sigend") || close("baa_signed", "baa_signd"));
        assert!(close("go_live", "Go_Live"));
        assert!(!close("go_live", "owner_team"));
    }
}
