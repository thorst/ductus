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
    let suite: Vec<PathBuf> =
        ["hl7", "formats", "services"].iter().map(|s| workspace().join(format!("sdk/tests/{s}.test.ts"))).collect();
    let paths: Vec<&Path> = suite.iter().map(PathBuf::as_path).collect();
    let fixtures = serde_json::json!({ "adt.hl7": ADT }).to_string();
    let mut isolate = rt.block_on(Isolate::tests(&paths, &fixtures, ductus_script::DEFAULT_FORMATS)).unwrap();
    let report: serde_json::Value = serde_json::from_str(&rt.block_on(isolate.run_tests()).unwrap()).unwrap();
    let results = report.as_array().unwrap();
    assert!(results.len() >= 37, "expected the full suite, got {}", results.len());
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
    let mut isolate =
        rt.block_on(Isolate::tests(&[file.as_path()], r#"{"f":"x"}"#, ductus_script::DEFAULT_FORMATS)).unwrap();
    let report: serde_json::Value = serde_json::from_str(&rt.block_on(isolate.run_tests()).unwrap()).unwrap();
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

    let out = rt.block_on(isolate.transform(ADT)).unwrap().unwrap();
    assert!(out.contains("PID|1||123456^^^MRN||SMITH^john\r"), "{out}");
    assert!(out.ends_with("ZDU|relayed\r"), "{out}");

    let a01 = ADT.replace("ADT^A08", "ADT^A01");
    assert_eq!(rt.block_on(isolate.transform(&a01)).unwrap(), None);
}

#[test]
fn one_isolate_hosts_many_interfaces() {
    let mut files = Vec::new();
    for i in 0..50 {
        files.push((
            format!("i{i}/transform.ts"),
            format!(
                "import type {{ Message }} from \"@ductus/sdk\";\nexport function transform(m: Message) {{ m.add(\"ZIF\", [\"{i}\"]); return m; }}"
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let dir = project(&refs);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host();
    let ids: Vec<_> = (0..50)
        .map(|i| rt.block_on(host.add_interface(&dir.path().join(format!("i{i}/transform.ts")))).unwrap())
        .collect();
    for (i, id) in ids.iter().enumerate().rev() {
        let out = rt.block_on(host.transform_with(*id, ADT)).unwrap().unwrap();
        assert!(out.ends_with(&format!("ZIF|{i}\r")), "{out}");
    }
}

/// The shared-pool isolation guarantees documented in docs/shared-pool.md.
#[test]
fn shared_isolate_scoping_rules() {
    let counter = r#"import type { Message } from "@ductus/sdk";
let count = 0;                                   // module scope: private to this interface
export function transform(m: Message) { count++; m.add("ZCT", [String(count)]); return m; }"#;
    let dir = project(&[
        ("a/transform.ts", counter),
        ("b/transform.ts", counter),
        (
            "c/transform.ts",
            r#"import type { Message } from "@ductus/sdk";
import { bump } from "../lib/shared.ts";
export function transform(m: Message) { m.add("ZSH", [String(bump())]); return m; }"#,
        ),
        (
            "d/transform.ts",
            r#"import type { Message } from "@ductus/sdk";
import { bump } from "../lib/shared.ts";
export function transform(m: Message) { m.add("ZSH", [String(bump())]); return m; }"#,
        ),
        ("lib/shared.ts", "let n = 0;\nexport const bump = () => ++n;"),
        ("e/transform.ts", "export function transform(m: unknown) { undeclared = 1; return m; }"),
    ]);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host();
    let mut load = |name: &str| rt.block_on(host.add_interface(&dir.path().join(name))).unwrap();
    let (a, b, c, d, e) = (
        load("a/transform.ts"),
        load("b/transform.ts"),
        load("c/transform.ts"),
        load("d/transform.ts"),
        load("e/transform.ts"),
    );

    let last = |out: String| out.trim_end().rsplit('\r').next().unwrap().to_string();
    // Same variable name in two interfaces: independent.
    assert_eq!(last(rt.block_on(host.transform_with(a, ADT)).unwrap().unwrap()), "ZCT|1");
    assert_eq!(last(rt.block_on(host.transform_with(a, ADT)).unwrap().unwrap()), "ZCT|2");
    assert_eq!(last(rt.block_on(host.transform_with(b, ADT)).unwrap().unwrap()), "ZCT|1");
    // A shared library file is ONE module instance: its state is shared.
    assert_eq!(last(rt.block_on(host.transform_with(c, ADT)).unwrap().unwrap()), "ZSH|1");
    assert_eq!(last(rt.block_on(host.transform_with(d, ADT)).unwrap().unwrap()), "ZSH|2");
    // Modules are strict: assigning an undeclared variable is an error, not a global.
    let err = rt.block_on(host.transform_with(e, ADT)).unwrap_err().to_string();
    assert!(err.contains("undeclared is not defined"), "{err}");
}

/// Stand-in for the engine's services: a slow "database", one variable.
struct FakeServices;

impl ductus_script::Services for FakeServices {
    fn db_query(
        &self,
        resource: &str,
        _sql: &str,
        params: Vec<serde_json::Value>,
    ) -> ductus_script::BoxFuture<Vec<serde_json::Value>> {
        let resource = resource.to_string();
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if resource == "down" {
                anyhow::bail!("{} connection refused", ductus_script::TRANSIENT);
            }
            Ok(vec![serde_json::json!({ "name": "SMITH", "mrn": params[0] })])
        })
    }
    fn db_execute(&self, _: &str, _: &str, _: Vec<serde_json::Value>) -> ductus_script::BoxFuture<u64> {
        Box::pin(async { Ok(1) })
    }
    /// Answers with the status given in the path (`/status/503`) and echoes the request.
    fn fetch(&self, r: ductus_script::FetchRequest) -> ductus_script::BoxFuture<ductus_script::FetchResponse> {
        Box::pin(async move {
            if r.url.contains("unreachable") {
                anyhow::bail!("{} {} {} failed: connection refused", ductus_script::TRANSIENT, r.method, r.url);
            }
            let status = r.url.rsplit_once("/status/").and_then(|(_, s)| s.get(..3)?.parse().ok()).unwrap_or(200);
            let echo = serde_json::json!({ "method": r.method, "url": r.url, "headers": r.headers, "body": r.body });
            Ok(ductus_script::FetchResponse { status, headers: Default::default(), body: echo.to_string() })
        })
    }
    fn variable(&self, name: &str) -> Option<String> {
        (name == "FACILITY").then(|| "FACILITY-A".to_string())
    }
    fn secret(&self, _: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

const LOOKUP: &str = r#"import { db, env, type Message } from "@ductus/sdk";
export async function transform(msg: Message) {
  const row = await db("RESOURCE").one<{ name: string }>("SELECT name FROM patients WHERE mrn = $1", [msg.PID[3][1]]);
  msg.add("ZLK", [row?.name ?? "", env("FACILITY")]);
  return msg;
}"#;

#[test]
fn async_transforms_use_databases_and_variables() {
    let dir = project(&[
        ("ok/transform.ts", &LOOKUP.replace("RESOURCE", "patients")),
        ("down/transform.ts", &LOOKUP.replace("RESOURCE", "down")),
    ]);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host_with(Some(std::rc::Rc::new(FakeServices)));
    let ok = rt.block_on(host.add_interface(&dir.path().join("ok/transform.ts"))).unwrap();
    let down = rt.block_on(host.add_interface(&dir.path().join("down/transform.ts"))).unwrap();

    let out = rt.block_on(host.transform_with(ok, ADT)).unwrap().unwrap();
    assert!(out.ends_with("ZLK|SMITH|FACILITY-A\r"), "{out}");

    let err = rt.block_on(host.transform_with(down, ADT)).unwrap_err().to_string();
    assert!(err.contains(ductus_script::TRANSIENT) && err.contains("connection refused"), "{err}");
}

#[test]
fn fetch_goes_through_the_engine_and_retries_server_errors() {
    let transform = r#"import { fetch, type Message } from "@ductus/sdk";
export async function transform(msg: Message) {
  const r = await fetch("https://api.test/PATH", { body: { mrn: msg.PID[3][1] }, headers: { "X-Key": "k" }, query: { a: "1 2" }, RETRY });
  const echo = r.json();
  msg.add("ZFT", [String(r.status), echo.method, echo.url, echo.headers["content-type"], echo.headers["x-key"], echo.body]);
  return msg;
}"#;
    let make = |path: &str, retry: &str| transform.replace("PATH", path).replace("RETRY", retry);
    let dir = project(&[
        ("ok/transform.ts", &make("patients", "retry: true")),
        ("busy/transform.ts", &make("status/503", "retry: true")),
        ("own/transform.ts", &make("status/503", "retry: false")),
        ("down/transform.ts", &make("unreachable", "retry: true")),
        ("down-own/transform.ts", &make("unreachable", "retry: false")),
    ]);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host_with(Some(std::rc::Rc::new(FakeServices)));
    let mut run = |name: &str| {
        let id = rt.block_on(host.add_interface(&dir.path().join(format!("{name}/transform.ts")))).unwrap();
        rt.block_on(host.transform_with(id, ADT)).map(Option::unwrap).map_err(|e| e.to_string())
    };

    let out = run("ok").unwrap();
    assert!(
        out.ends_with("ZFT|200|POST|https://api.test/patients?a=1%202|application/json|k|{\"mrn\":\"123456\"}\r"),
        "{out}"
    );
    let err = run("busy").unwrap_err();
    assert!(
        err.contains(&format!("{} POST https://api.test/status/503 returned 503", ductus_script::TRANSIENT)),
        "{err}"
    );
    // (The fake echoes the URL in its body, which the message previews; the label itself has no query.)
    assert!(!err.split(" returned").next().unwrap().contains("a=1"), "the query string stays out of the label: {err}");
    assert!(run("own").unwrap().contains("ZFT|503|POST"));
    let err = run("down").unwrap_err();
    assert!(err.contains(ductus_script::TRANSIENT) && err.contains("connection refused"), "{err}");
    let err = run("down-own").unwrap_err();
    assert!(!err.contains(ductus_script::TRANSIENT) && err.contains("connection refused"), "{err}");
}

#[test]
fn kill_and_quarantine_reach_the_engine() {
    let dir = project(&[
        (
            "k/transform.ts",
            r#"import { kill, type Message } from "@ductus/sdk";
export function transform(msg: Message) { return msg.PID[5][1] === "smith" ? kill("test patient") : msg; }"#,
        ),
        (
            "q/transform.ts",
            r#"import { quarantine, type Message } from "@ductus/sdk";
export function transform(msg: Message) { if (!msg.PV1.exists) throw quarantine("no visit"); return msg; }"#,
        ),
    ]);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host();
    let k = rt.block_on(host.add_interface(&dir.path().join("k/transform.ts"))).unwrap();
    let q = rt.block_on(host.add_interface(&dir.path().join("q/transform.ts"))).unwrap();

    let killed = rt.block_on(host.transform_with(k, ADT)).unwrap().unwrap();
    assert_eq!(killed.strip_prefix(ductus_script::KILL), Some("test patient"));
    let err = rt.block_on(host.transform_with(q, ADT)).unwrap_err().to_string();
    assert!(err.contains(&format!("{} no visit", ductus_script::QUARANTINE)), "{err}");
}

#[test]
fn concurrent_async_transforms_share_one_isolate() {
    use ductus_script::Call;
    let dir = project(&[("a/transform.ts", &LOOKUP.replace("RESOURCE", "patients"))]);
    let rt = rt();
    let _guard = rt.enter();
    let mut host = Isolate::host_with(Some(std::rc::Rc::new(FakeServices)));
    let a = rt.block_on(host.add_interface(&dir.path().join("a/transform.ts"))).unwrap();

    // Start 20 transforms before any finishes; each "query" takes 50 ms.
    let mut pending = Vec::new();
    for _ in 0..20 {
        match host.begin(a, ADT).unwrap() {
            Call::Pending(p) => pending.push(Box::pin(p)),
            Call::Done(_) => panic!("expected an async transform"),
        }
    }
    let start = std::time::Instant::now();
    let results = rt.block_on(async {
        let mut results = Vec::new();
        for mut p in pending {
            // Drive the event loop until this call settles (others progress meanwhile).
            let value = loop {
                tokio::select! {
                    v = &mut p => break v,
                    _ = std::future::poll_fn(|cx| host.poll_event_loop(cx)) => {
                        tokio::task::yield_now().await;
                    }
                }
            };
            results.push(host.settle(value).unwrap().unwrap());
        }
        results
    });
    assert_eq!(results.len(), 20);
    assert!(results.iter().all(|r| r.ends_with("ZLK|SMITH|FACILITY-A\r")));
    // Concurrent, not sequential: 20 x 50 ms would take a full second.
    assert!(start.elapsed() < std::time::Duration::from_millis(600), "took {:?}", start.elapsed());
}

#[test]
fn transform_errors_carry_the_js_message() {
    let dir = project(&[("transform.ts", "export function transform() { throw new Error(\"bad mapping\"); }")]);
    let rt = rt();
    let _guard = rt.enter();
    let mut isolate = rt.block_on(Isolate::interface(&dir.path().join("transform.ts"))).unwrap();
    let err = rt.block_on(isolate.transform(ADT)).unwrap_err().to_string();
    assert!(err.contains("bad mapping"), "{err}");
    // The isolate stays usable after an error.
    assert!(rt.block_on(isolate.transform(ADT)).is_err());
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
fn type_check_catches_mapping_mistakes() {
    let rt = rt();
    let _guard = rt.enter();
    let types = rt.block_on(Isolate::generate_types()).unwrap();
    let mut files = std::collections::BTreeMap::new();
    files.insert("/p/.ductus/types/ductus-sdk.d.ts".to_string(), types);
    files.insert(
        "/p/good.ts".to_string(),
        r#"import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  if (msg.PID[8] === "F") msg.PID[5][1] = msg.PID[5][1].toUpperCase();
  for (const obx of msg.OBX) obx[5][1] = obx[5][1].trim();
  msg.PID[3] = "123^^^MRN";
  msg.ZPI[1].value;
  return msg;
}"#
        .to_string(),
    );
    files.insert(
        "/p/bad.ts".to_string(),
        r#"import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  const last: number = msg.PID[5][1];
  msg.PID[5][1].toUpperCasee();
  return msg;
}"#
        .to_string(),
    );

    let diagnostics = ductus_script::type_check(&files, &[]).unwrap();
    let errors: Vec<_> = diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.iter().all(|d| d.file.as_deref() == Some("/p/bad.ts")), "unexpected errors: {errors:#?}");
    assert!(errors.iter().any(|d| d.line == 3 && d.message.contains("not assignable to type 'number'")), "{errors:#?}");
    assert!(errors.iter().any(|d| d.line == 4 && d.message.contains("toUpperCasee")), "{errors:#?}");
}

#[test]
fn validate_warns_about_shared_pool_state() {
    let rt = rt();
    let _guard = rt.enter();
    let mut files = std::collections::BTreeMap::new();
    files.insert("/p/.ductus/types/ductus-sdk.d.ts".to_string(), rt.block_on(Isolate::generate_types()).unwrap());
    files.insert(
        "/p/interfaces/a/transform.ts".to_string(),
        r#"import type { Message } from "@ductus/sdk";
let seen = 0;
export function transform(msg: Message) {
  seen++;
  (globalThis as any).lastMrn = msg.PID[3][1];
  (String.prototype as any).shout = function () { return this.toUpperCase(); };
  Object.assign(globalThis, { x: 1 });
  return msg;
}"#
        .to_string(),
    );
    files.insert(
        "/p/lib/shared.ts".to_string(),
        "let cache = new Map();\nconst ok = 1;\nexport { cache, ok };".to_string(),
    );

    let d = ductus_script::type_check(&files, &["/p/lib/shared.ts".to_string()]).unwrap();
    let lint = |code: u32| {
        d.iter().filter(move |x| x.code == code).map(|x| (x.file.clone().unwrap(), x.line)).collect::<Vec<_>>()
    };
    let a = "/p/interfaces/a/transform.ts".to_string();
    assert_eq!(lint(90001), vec![(a.clone(), 5), (a.clone(), 7)], "{d:#?}");
    assert_eq!(lint(90002), vec![(a.clone(), 6)], "{d:#?}");
    // Module-level `let` inside an interface is private (fine); in a shared module it's flagged.
    assert_eq!(lint(90003), vec![("/p/lib/shared.ts".to_string(), 1)], "{d:#?}");
    assert!(d.iter().all(|x| !x.is_error()), "lints are warnings: {d:#?}");
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
