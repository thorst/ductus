//! Environments: per-environment variables and secrets, substituted into project config.
//!
//! ```text
//! environments/dev.yaml     variables for `--env dev`
//! environments/prod.yaml    variables for `--env prod`
//! secrets/prod.json         encrypted secrets for `--env prod` (see `ductus secret`)
//! ```
//!
//! Config files reference them as `${NAME}` and `${secret:NAME}`. Substitution happens on
//! the parsed YAML, so values with `:`, `#` or quotes can't break the file. A scalar that is
//! exactly `${NAME}` takes the variable's type (so `port: ${EPIC_PORT}` is a number);
//! secrets are always strings.

use crate::secrets::SecretStore;
use anyhow::{Context, Result, bail};
use serde_yaml_ng::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Variables and secrets for one environment.
#[derive(Default)]
pub struct Environment {
    pub name: String,
    variables: BTreeMap<String, Value>,
    secrets: Option<SecretStore>,
}

/// `environments/<name>.yaml`: one environment's values (`dev.yaml`, `test.yaml`,
/// `prod.yaml`). The engine reads the one it's started with (`--env prod`, or `DUCTUS_ENV`), and
/// every `${NAME}` in interface.yaml, resources.yaml and ductus.yaml's `alerts:` takes its value
/// from here. The same project then runs anywhere, with only this file differing.
///
/// Secrets (passwords, tokens, keys) don't go here: they're in `secrets/<name>.json`,
/// encrypted (`ductus secret set NAME`), and written `${secret:NAME}`. `ductus validate` checks
/// every environment, so a variable missing from one is found before it's deployed.
#[derive(serde::Deserialize, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentFile {
    /// The environment's values by name, used as `${NAME}` in the configuration. A value that
    /// stands alone keeps its type (`port: ${EPIC_PORT}` is a number when `EPIC_PORT: 6661`);
    /// inside text it's text (`url: "https://${EMPI_HOST}/api"`). Give every environment the same
    /// names. Default: none. Example: `variables: { PATIENT_DB_HOST: db1.hospital.local,
    /// EPIC_PORT: 6661 }`.
    #[serde(default)]
    #[cfg_attr(test, schemars(with = "BTreeMap<String, serde_json::Value>"))]
    variables: BTreeMap<String, Value>,
}

impl Environment {
    /// Load `<root>/environments/<name>.yaml` and `<root>/secrets/<name>.json` (both optional).
    pub fn load(root: &Path, name: &str) -> Result<Self> {
        let file = root.join("environments").join(format!("{name}.yaml"));
        let variables = if file.is_file() {
            let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            let parsed: EnvironmentFile =
                serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
            parsed.variables
        } else if root.join("environments").is_dir() {
            bail!("unknown environment `{name}`: {} doesn't exist", file.display());
        } else {
            BTreeMap::new()
        };
        let secrets = SecretStore::open(root, name)?;
        Ok(Self { name: name.to_string(), variables, secrets })
    }

    /// An environment with just these variables (for tests).
    pub fn with_variables(name: &str, variables: &[(&str, &str)]) -> Self {
        let variables = variables
            .iter()
            .map(|(k, v)| (k.to_string(), serde_yaml_ng::from_str(v).unwrap_or(Value::String(v.to_string()))))
            .collect();
        Self { name: name.to_string(), variables, secrets: None }
    }

    pub fn variable(&self, name: &str) -> Option<String> {
        self.variables.get(name).map(scalar_text)
    }

    pub fn secret(&self, name: &str) -> Result<Option<String>> {
        match &self.secrets {
            Some(store) => store.get(name),
            None => Ok(None),
        }
    }

    /// Replace `${NAME}` / `${secret:NAME}` throughout a parsed YAML document. Every missing
    /// reference is reported at once, so a deployment check lists all of them.
    pub fn substitute(&self, value: &mut Value) -> Result<()> {
        let mut missing = BTreeSet::new();
        self.walk(value, &mut missing)?;
        if !missing.is_empty() {
            let list: Vec<_> = missing.into_iter().collect();
            bail!("missing in environment `{}`: {}", self.name, list.join(", "));
        }
        Ok(())
    }

    fn walk(&self, value: &mut Value, missing: &mut BTreeSet<String>) -> Result<()> {
        match value {
            Value::String(s) => {
                if let Some(replacement) = self.expand(s, missing)? {
                    *value = replacement;
                }
            }
            Value::Sequence(items) => {
                for item in items {
                    self.walk(item, missing)?;
                }
            }
            Value::Mapping(map) => {
                for (_, v) in map.iter_mut() {
                    self.walk(v, missing)?;
                }
            }
            Value::Tagged(tagged) => self.walk(&mut tagged.value, missing)?,
            _ => {}
        }
        Ok(())
    }

    /// Expand references in one string scalar. Returns the new value, or None if unchanged.
    fn expand(&self, text: &str, missing: &mut BTreeSet<String>) -> Result<Option<Value>> {
        if !text.contains([OPEN, LITERAL]) {
            return Ok(None);
        }
        let refs = references(text);
        // Exactly one variable reference and nothing else: keep the variable's own type.
        if let [(start, end, Ref::Variable(name))] = refs.as_slice()
            && *start == 0
            && *end == text.len()
        {
            return Ok(Some(match self.variables.get(name) {
                Some(v) => v.clone(),
                None => {
                    missing.insert(name.clone());
                    Value::Null
                }
            }));
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (start, end, r) in &refs {
            out.push_str(&text[last..*start]);
            match r {
                Ref::Variable(name) => match self.variable(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        missing.insert(name.clone());
                    }
                },
                Ref::Secret(name) => match self.secret(name)? {
                    Some(v) => out.push_str(&v),
                    None => {
                        missing.insert(format!("secret:{name}"));
                    }
                },
                Ref::Literal => out.push_str("${"),
            }
            last = *end;
        }
        out.push_str(&text[last..]);
        Ok(Some(Value::String(out)))
    }
}

enum Ref {
    Variable(String),
    Secret(String),
    /// `$${` escapes a literal `${`.
    Literal,
}

// `${` and `}` are YAML flow syntax (`{ port: ${PORT} }` wouldn't parse), so before parsing,
// references are rewritten with private-use characters that YAML treats as plain text.
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';
const LITERAL: char = '\u{E002}';

/// Parse YAML text that may contain `${...}` references (in any position, quoted or not).
pub fn parse_yaml(text: &str) -> Result<Value> {
    Ok(serde_yaml_ng::from_str(&protect(text)?)?)
}

fn protect(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("${") {
        if rest[..pos].ends_with('$') {
            out.push_str(&rest[..pos - 1]);
            out.push(LITERAL);
            rest = &rest[pos + 2..];
            continue;
        }
        out.push_str(&rest[..pos]);
        let Some(close) = rest[pos..].find('}') else { bail!("unclosed `${{`") };
        let inner = &rest[pos + 2..pos + close];
        let valid = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !(valid(inner) || inner.strip_prefix("secret:").is_some_and(valid)) {
            bail!("invalid reference `${{{inner}}}` (use ${{NAME}} or ${{secret:NAME}})");
        }
        out.push(OPEN);
        out.push_str(inner);
        out.push(CLOSE);
        rest = &rest[pos + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Find (protected) references in a scalar: (start, end, reference), as byte offsets.
fn references(text: &str) -> Vec<(usize, usize, Ref)> {
    let mut out = Vec::new();
    for (start, c) in text.char_indices() {
        match c {
            LITERAL => out.push((start, start + LITERAL.len_utf8(), Ref::Literal)),
            OPEN => {
                let inner_start = start + OPEN.len_utf8();
                let Some(len) = text[inner_start..].find(CLOSE) else { continue };
                let inner = &text[inner_start..inner_start + len];
                let r = match inner.strip_prefix("secret:") {
                    Some(name) => Ref::Secret(name.to_string()),
                    None => Ref::Variable(inner.to_string()),
                };
                out.push((start, inner_start + len + CLOSE.len_utf8(), r));
            }
            _ => {}
        }
    }
    out
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => serde_yaml_ng::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(yaml: &str) -> Value {
        parse_yaml(yaml).unwrap()
    }

    fn plain(yaml: &str) -> Value {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn substitutes_and_keeps_types() {
        let env = Environment::with_variables("dev", &[("HOST", "10.1.1.5"), ("PORT", "6661"), ("TLS", "true")]);
        let mut v =
            doc("a: ${HOST}\nb: ${PORT}\nc: 'tcp://${HOST}:${PORT}/x'\nd: ${TLS}\ne: [${PORT}]\nf: 'cost $${5'");
        env.substitute(&mut v).unwrap();
        assert_eq!(v, plain("a: 10.1.1.5\nb: 6661\nc: 'tcp://10.1.1.5:6661/x'\nd: true\ne: [6661]\nf: 'cost ${5'"));
        assert!(v["b"].is_u64());
    }

    #[test]
    fn works_inside_yaml_flow_style() {
        let env = Environment::with_variables("dev", &[("HOST", "10.1.1.5"), ("PORT", "6661")]);
        let mut v = doc("source: { mllp: { host: ${HOST}, port: ${PORT} } }");
        env.substitute(&mut v).unwrap();
        assert_eq!(v, plain("source: { mllp: { host: 10.1.1.5, port: 6661 } }"));
    }

    #[test]
    fn reports_every_missing_reference() {
        let env = Environment::with_variables("prod", &[("HOST", "h")]);
        let mut v = doc("a: ${HOST}\nb: ${PORT}\nc: 'x-${OTHER}'\nd: '${secret:DB_PASSWORD}'");
        let err = env.substitute(&mut v).unwrap_err().to_string();
        assert_eq!(err, "missing in environment `prod`: OTHER, PORT, secret:DB_PASSWORD");
    }

    #[test]
    fn values_with_yaml_syntax_cannot_break_the_document() {
        let env = Environment::with_variables("dev", &[("TRICKY", "'a: b # c'")]);
        let mut v = doc("x: 'pre ${TRICKY}'\ny: other");
        env.substitute(&mut v).unwrap();
        assert_eq!(v["x"], Value::String("pre a: b # c".into()));
        assert_eq!(v["y"], Value::String("other".into()));
    }

    #[test]
    fn rejects_malformed_references() {
        assert!(parse_yaml("a: '${'").is_err());
        assert!(parse_yaml("a: '${not valid}'").is_err());
        assert!(parse_yaml("a: '${secret:}'").is_err());
    }
}
