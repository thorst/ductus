//! What settings parsed from text by hand look like in a schema: durations (`15m`), port ranges,
//! hours, retention windows, formats. Each says the shape the engine's parser takes, so an editor
//! flags `poll: 10 seconds` before `ductus validate` does; the field's own doc comment says what
//! it means. `x-type` is the short name the reference pages show in the Type column.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use std::borrow::Cow;

/// A duration as `config::parse_duration` reads it: `500ms`, `90s`, `15m`, `24h`, `7d`.
pub(crate) struct Duration;

const DURATION: &str = r"[0-9]+\s*(ms|s|m|h|d)";

impl JsonSchema for Duration {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Duration".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": format!(r"^\s*{DURATION}\s*$"), "x-type": "duration" })
    }
}

/// A duration, or `off`.
pub(crate) struct OffOrDuration;

impl JsonSchema for OffOrDuration {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "OffOrDuration".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": format!(r"^\s*([oO][fF][fF]|{DURATION})\s*$"),
            "x-type": "duration, or off"
        })
    }
}

/// A duration, or `0` for never.
pub(crate) struct ZeroOrDuration;

impl JsonSchema for ZeroOrDuration {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "ZeroOrDuration".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [{ "type": "string", "pattern": format!(r"^\s*(0|{DURATION})\s*$") }, { "const": 0 }],
            "x-type": "duration, or 0"
        })
    }
}

/// A duration that may also be in years (`6y`, 365 days each): how long records are kept.
pub(crate) struct Keep;

impl JsonSchema for Keep {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Keep".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": r"^\s*[0-9]+\s*(y|ms|s|m|h|d)\s*$",
            "x-type": "duration (or years: 6y)"
        })
    }
}

/// One text, or a list of them (`email: a@x` or `email: [a@x, b@x]`).
pub(crate) struct OneOrMany;

impl JsonSchema for OneOrMany {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "OneOrMany".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }],
            "x-type": "text, or a list"
        })
    }
}

/// `tls:` on a connection the engine makes: `true`, `false`, or `{ ca, certificate }`
/// (`tls::connect_tls_opt`).
pub(crate) struct TlsSetting;

impl JsonSchema for TlsSetting {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "TlsSetting".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let with = generator.subschema_for::<crate::tls::ConnectTls>();
        json_schema!({ "anyOf": [{ "type": "boolean" }, with], "x-type": "true, or { ca, certificate }" })
    }
}

impl JsonSchema for crate::monitor::Hours {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Hours".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": r"^\s*[0-9]{1,2}:[0-9]{2}\s*-\s*[0-9]{1,2}:[0-9]{2}\s*$",
            "x-type": "hours (06:00-22:00)"
        })
    }
}

impl JsonSchema for crate::monitor::At {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "At".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "x-type": "time (09:00, Mon-Fri 09:00, or cron)" })
    }
}

impl JsonSchema for crate::monitor::Behind {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Behind".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let age = generator.subschema_for::<Duration>();
        json_schema!({
            "anyOf": [
                { "type": "integer", "minimum": 1 },
                { "type": "string", "pattern": format!(r"^\s*([0-9]+|{DURATION})\s*$") },
                {
                    "type": "object",
                    "properties": {
                        "messages": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Behind once this many messages wait for the destination, at least 1. Default: no count limit. Example: `messages: 500`."
                        },
                        "age": {
                            "allOf": [age],
                            "description": "Behind once the oldest waiting message has waited this long, at least `1s`. Default: no age limit. Example: `age: 10m`."
                        }
                    },
                    "additionalProperties": false
                }
            ],
            "x-type": "messages, age, or { messages, age }"
        })
    }
}

impl JsonSchema for crate::Retention {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Retention".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                { "type": "string", "pattern": r"^\s*([137]\s*w|(7|21|49)\s*d?)\s*$" },
                { "enum": [7, 21, 49] }
            ],
            "x-type": "1w, 3w or 7w"
        })
    }
}

impl JsonSchema for crate::settings::PortRange {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "PortRange".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": r"^\s*[0-9]+\s*-\s*[0-9]+\s*$", "x-type": "port range (6661-6699)" })
    }
}

impl JsonSchema for crate::resources::Encrypt {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Encrypt".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": ["mandatory", "strict", true], "x-type": "mandatory or strict" })
    }
}

impl JsonSchema for crate::format::Format {
    fn schema_name() -> Cow<'static, str> {
        "Format".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let csv = generator.subschema_for::<Option<crate::format::CsvOptions>>();
        let fixed = generator.subschema_for::<crate::format::FixedLayoutConfig>();
        json_schema!({
            "anyOf": [
                { "enum": ["hl7", "json", "xml", "text", "csv"] },
                {
                    "type": "object",
                    "properties": { "csv": {
                        "allOf": [csv],
                        "description": "Delimited text with its options (`delimiter`, `header`). `format: csv` alone means commas and a header line. Example: `format: { csv: { delimiter: \"|\", header: false } }`."
                    } },
                    "required": ["csv"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "fixed": {
                        "allOf": [fixed],
                        "description": "Fixed-length records: each field at a set column and width, listed in `fields`. Each line of a file is a record. Example: `format: { fixed: { fields: [{ name: mrn, width: 10 }, { name: name, width: 30 }] } }`."
                    } },
                    "required": ["fixed"],
                    "additionalProperties": false
                }
            ],
            "x-type": "hl7, json, xml, text, csv, or { csv | fixed: options }"
        })
    }
}
