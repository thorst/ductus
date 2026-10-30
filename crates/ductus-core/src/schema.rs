//! The configuration files' JSON Schemas: every setting with its type and its description, for
//! editors (completion, hover help and checking as you type: `ductus types` writes them into a
//! project for VS Code) and for the reference pages (`ductus reference`).
//!
//! They are made from the config types themselves, so they say what the engine accepts: a test
//! (`generate`, with schemars, a dev-dependency: the binary compiles none of it) writes them to
//! `docs/schema/` and fails when the committed ones were stale; the binary carries that text. The
//! doc comments on config fields are the descriptions, so they are written for the people who
//! configure an engine (`docs/STYLE.md`), and a test fails on a setting without one.

/// One configuration file's schema.
pub struct Schema {
    /// The schema's file name: `interface.json`.
    pub name: &'static str,
    /// The files it describes, as patterns from the project folder (VS Code's `yaml.schemas`).
    pub files: &'static [&'static str],
    /// What those files are, as the reference pages title them.
    pub title: &'static str,
    /// The schema (JSON).
    pub text: &'static str,
}

/// Every configuration file's schema, in the order the reference lists them.
pub const SCHEMAS: [Schema; 5] = [
    Schema {
        name: "interface.json",
        files: &["interfaces/**/interface.yaml"],
        title: "interface.yaml",
        text: include_str!("../../../docs/schema/interface.json"),
    },
    Schema {
        name: "resources.json",
        files: &["resources.yaml"],
        title: "resources.yaml",
        text: include_str!("../../../docs/schema/resources.json"),
    },
    Schema {
        name: "ductus.json",
        files: &["ductus.yaml"],
        title: "ductus.yaml",
        text: include_str!("../../../docs/schema/ductus.json"),
    },
    Schema {
        name: "environment.json",
        files: &["environments/*.yaml"],
        title: "environments/*.yaml",
        text: include_str!("../../../docs/schema/environment.json"),
    },
    Schema {
        name: "docs-schema.json",
        files: &["docs-schema.yaml"],
        title: "docs-schema.yaml",
        text: include_str!("../../../docs/schema/docs-schema.json"),
    },
];

#[cfg(test)]
mod generate;
#[cfg(test)]
pub(crate) mod shapes;
