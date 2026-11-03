//! Throughput of a realistic transform through @ductus/sdk on one thread:
//! parse, read fields, edit in place, loop over OBX, serialize.
//!
//!   cargo run --release -p ductus-script --example bench
//!   cargo run --release -p ductus-script --example bench -- console LOG_FILE
//!
//! `console`: the same transform with one `console.log` line a message, written to LOG_FILE as the
//! engine's log writes it (`console: info`), and with `console: warn` (the line dropped).

use ductus_core::ConsoleLevel;
use ductus_script::Isolate;
use std::time::Instant;

const TRANSFORM: &str = r#"import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  if (msg.MSH[9][1] !== "ORU") return null;
  msg.PID[5][1] = msg.PID[5][1].toUpperCase();
  if (msg.PID[8] === "U") msg.PID[8] = "";
  for (const obx of msg.OBX) {
    obx[5][1] = obx[5][1].trim();
    if (obx[3][1] === "GLU" && Number(obx[5][1]) > 200) obx[8] = "H";
  }
  msg.add("ZDU", ["benchmarked"]);
  return msg;
}"#;

fn oru() -> String {
    let mut m = String::from(
        "MSH|^~\\&|LAB|HOSP|EHR|HOSP|20261005120000||ORU^R01^ORU_R01|MSG0001|P|2.5\r\
         PID|1||123456^^^MRN^MR||smith^john^q||19800101|M|||1 MAIN ST^^YORK^PA^17404\r\
         PV1|1|O|LAB^^^HOSP\r\
         ORC|RE|ORD1001\r\
         OBR|1|ORD1001||PANEL^Basic Metabolic Panel\r",
    );
    let tests =
        [("GLU", "Glucose", " 245 ", "mg/dL"), ("NA", "Sodium", "140", "mmol/L"), ("K", "Potassium", "4.1", "mmol/L")];
    for i in 0..12 {
        let (code, name, value, unit) = tests[i % tests.len()];
        m.push_str(&format!("OBX|{}|NM|{code}^{name}||{value}|{unit}|||||F\r", i + 1));
    }
    m
}

/// The transform with a `console.log` line of the message's id and patient before it returns.
fn logging() -> String {
    TRANSFORM
        .replace("  return msg;\n}", "  console.log(\"sent\", msg.MSH[10], \"for\", msg.PID[3][1]);\n  return msg;\n}")
}

/// Microseconds a call, over `n` calls after a warm-up.
fn time(rt: &tokio::runtime::Runtime, isolate: &mut Isolate, message: &str, n: u32) -> anyhow::Result<f64> {
    for _ in 0..5_000 {
        rt.block_on(isolate.transform(message))?; // warm up the JIT
    }
    let start = Instant::now();
    for _ in 0..n {
        rt.block_on(isolate.transform(message))?;
    }
    Ok(start.elapsed().as_secs_f64() * 1e6 / n as f64)
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let _guard = rt.enter();
    let dir = std::env::temp_dir().join("ductus-bench");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("transform.ts");
    std::fs::write(&path, TRANSFORM)?;
    let message = oru();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [mode, log] = args.as_slice()
        && mode == "console"
    {
        return console(&rt, &dir, &message, log);
    }

    let mut isolate = rt.block_on(Isolate::interface(&path))?;
    let out = rt.block_on(isolate.transform(&message))?.expect("not filtered");
    assert!(out.contains("PID|1||123456^^^MRN^MR||SMITH^john^q"), "{out}");
    assert!(out.contains("|GLU^Glucose||245|mg/dL||H|"), "{out}"); // OBX-5 trimmed, OBX-8 flagged
    assert!(out.ends_with("ZDU|benchmarked\r"), "{out}");

    let n = 100_000;
    let each = time(&rt, &mut isolate, &message, n)?;
    println!(
        "{n} ORU messages ({} segments, {} bytes) in {:.2}s = {:.0} msg/s on one thread ({each:.1} µs each)",
        message.split('\r').filter(|s| !s.is_empty()).count(),
        message.len(),
        each * n as f64 / 1e6,
        1e6 / each,
    );
    Ok(())
}

/// What a `console.log` a message costs: the line written to `log` as the engine writes its log
/// (plain text, written as it comes), or dropped by the environment's `console: warn`.
fn console(rt: &tokio::runtime::Runtime, dir: &std::path::Path, message: &str, log: &str) -> anyhow::Result<()> {
    let file = std::fs::File::create(log)?;
    tracing_subscriber::fmt().with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    let plain = dir.join("transform.ts");
    let logged = dir.join("logging.ts");
    std::fs::write(&logged, logging())?;
    let n = 100_000;
    let mut runs = Vec::new();
    for (name, path, level) in [
        ("no console call", &plain, ConsoleLevel::Info),
        ("console.log, written (console: info)", &logged, ConsoleLevel::Info),
        ("console.log, dropped (console: warn)", &logged, ConsoleLevel::Warn),
    ] {
        let mut isolate = rt.block_on(Isolate::interface(path))?;
        isolate.console_to_log(dir, level);
        runs.push((name, time(rt, &mut isolate, message, n)?));
    }
    let base = runs[0].1;
    for (name, each) in runs {
        println!("{name:40} {each:6.2} µs a message ({:+.2} µs)", each - base);
    }
    let size = std::fs::metadata(log)?.len();
    println!("log: {size} bytes for {} lines ({} bytes a line)", n + 5_000, size / (n as u64 + 5_000));
    Ok(())
}
