use ductus_script::Isolate;
use std::path::{Path, PathBuf};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Write files into a fresh temp directory and return it.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

const ADT: &str = "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|20261005120000||ADT^A08|1|P|2.5\rPID|1||123456^^^MRN||smith^john\r";

#[test]
fn sdk_test_suite_passes() {
    let rt = rt();
    let _guard = rt.enter();
    let suite = workspace().join("sdk/tests/hl7.test.ts");
    let fixtures = serde_json::json!({ "adt.hl7": ADT }).to_string();
    let mut isolate = rt.block_on(Isolate::tests(&[suite.as_path()], &fixtures)).unwrap();
    let report: serde_json::Value = serde_json::from_str(&isolate.run_tests().unwrap()).unwrap();
    let results = report.as_array().unwrap();
    assert!(results.len() >= 15, "expected the full suite, got {}", results.len());
    let failures: Vec<_> = results.iter().filter(|r| r["ok"] != true).collect();
    assert!(failures.is_empty(), "SDK test failures:\n{failures:#?}");
}

#[test]
fn minimal_test_suite_runs() {
    let dir = project(&[(
        "a.test.ts",
        "import { test, expect, fixture } from \"@ductus/sdk\";\ntest(\"one\", () => expect(fixture(\"f\")).toBe(\"x\"));\ntest(\"two\", () => expect(1).toBe(2));",
    )]);
    let rt = rt();
    let _guard = rt.enter();
    let file = dir.path().join("a.test.ts");
    let mut isolate = rt.block_on(Isolate::tests(&[file.as_path()], r#"{"f":"x"}"#)).unwrap();
    let report: serde_json::Value = serde_json::from_str(&isolate.run_tests().unwrap()).unwrap();
    assert_eq!(report[0]["ok"], true, "{report}");
    assert_eq!(report[1]["ok"], false, "{report}");
    assert!(report[1]["error"].as_str().unwrap().contains("expected 2, got 1"), "{report}");
}

#[test]
fn transform_edits_filters_and_imports_siblings() {
    let dir = project(&[
        (
            "transform.ts",
            r#"import type { Message } from "@ductus/sdk";
import { tag } from "./lib/tag.ts";
export function transform(msg: Message) {
  if (msg.MSH[9][2] === "A01") return null;
  msg.PID[5][1] = msg.PID[5][1].toUpperCase();
  msg.add("ZDU", [tag()]);
  return msg;
}"#,
        ),
        ("lib/tag.ts", "export const tag = (): string => \"relayed\";"),
    ]);
    let rt = rt();
    let _guard = rt.enter();
    let mut isolate = rt.block_on(Isolate::interface(&dir.path().join("transform.ts"))).unwrap();

    let out = isolate.transform(ADT).unwrap().unwrap();
    assert!(out.contains("PID|1||123456^^^MRN||SMITH^john\r"), "{out}");
    assert!(out.ends_with("ZDU|relayed\r"), "{out}");

    let a01 = ADT.replace("ADT^A08", "ADT^A01");
    assert_eq!(isolate.transform(&a01).unwrap(), None);
}

#[test]
fn transform_errors_carry_the_js_message() {
    let dir = project(&[(
        "transform.ts",
        "export function transform() { throw new Error(\"bad mapping\"); }",
    )]);
    let rt = rt();
    let _guard = rt.enter();
    let mut isolate = rt.block_on(Isolate::interface(&dir.path().join("transform.ts"))).unwrap();
    let err = isolate.transform(ADT).unwrap_err().to_string();
    assert!(err.contains("bad mapping"), "{err}");
    // The isolate stays usable after an error.
    assert!(isolate.transform(ADT).is_err());
}

#[test]
fn missing_transform_export_is_reported() {
    let dir = project(&[("transform.ts", "export const nope = 1;")]);
    let rt = rt();
    let _guard = rt.enter();
    let err = rt.block_on(Isolate::interface(&dir.path().join("transform.ts"))).err().unwrap().to_string();
    assert!(err.contains("must export a function named `transform`"), "{err}");
}

#[test]
fn remote_imports_are_refused() {
    let dir = project(&[(
        "transform.ts",
        "import { x } from \"https://example.com/x.ts\";\nexport function transform(m: unknown) { return x(m); }",
    )]);
    let rt = rt();
    let _guard = rt.enter();
    let err = rt.block_on(Isolate::interface(&dir.path().join("transform.ts"))).err().unwrap().to_string();
    assert!(err.contains("not allowed"), "{err}");
}

#[test]
fn generates_typings_with_field_docs() {
    let rt = rt();
    let _guard = rt.enter();
    let types = rt.block_on(Isolate::generate_types()).unwrap();
    assert!(types.contains("declare module \"@ductus/sdk\""));
    assert!(types.contains("/** PID-3 Patient Identifier List (CX) */"));
    assert!(types.contains("get 3(): CX;"));
    assert!(types.contains("/** CX.1 ID Number (ST) */"));
    assert!(types.contains("/** PID-8 Administrative Sex (IS) */\n    8: string;"));
}
