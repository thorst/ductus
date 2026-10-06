//! Throughput of a realistic transform through @ductus/sdk on one thread:
//! parse, read fields, edit in place, loop over OBX, serialize.
//!
//!   cargo run --release -p ductus-script --example bench

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
    let tests = [("GLU", "Glucose", " 245 ", "mg/dL"), ("NA", "Sodium", "140", "mmol/L"), ("K", "Potassium", "4.1", "mmol/L")];
    for i in 0..12 {
        let (code, name, value, unit) = tests[i % tests.len()];
        m.push_str(&format!("OBX|{}|NM|{code}^{name}||{value}|{unit}|||||F\r", i + 1));
    }
    m
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let _guard = rt.enter();
    let dir = std::env::temp_dir().join("ductus-bench");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("transform.ts");
    std::fs::write(&path, TRANSFORM)?;

    let mut isolate = rt.block_on(Isolate::interface(&path))?;
    let message = oru();
    let out = isolate.transform(&message)?.expect("not filtered");
    assert!(out.contains("PID|1||123456^^^MRN^MR||SMITH^john^q"), "{out}");
    assert!(out.contains("|GLU^Glucose||245|mg/dL||H|"), "{out}"); // OBX-5 trimmed, OBX-8 flagged
    assert!(out.ends_with("ZDU|benchmarked\r"), "{out}");

    for _ in 0..5_000 {
        isolate.transform(&message)?; // warm up the JIT
    }
    let n = 100_000;
    let start = Instant::now();
    for _ in 0..n {
        isolate.transform(&message)?;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{n} ORU messages ({} segments, {} bytes) in {secs:.2}s = {:.0} msg/s on one thread ({:.1} µs each)",
        message.split('\r').filter(|s| !s.is_empty()).count(),
        message.len(),
        n as f64 / secs,
        secs * 1e6 / n as f64
    );
    Ok(())
}
