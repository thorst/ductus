//! Spike: transpile TypeScript, run it in embedded V8, measure call throughput.

use ductus_script::{Isolate, transpile};
use std::time::Instant;

const TS: &str = r#"
function transform(msg: string): string {
    const segments: string[] = msg.split("\r");
    const pid = segments.find((s) => s.startsWith("PID|")) ?? "";
    const mrn = pid.split("|")[3]?.split("^")[0] ?? "";
    return `MRN=${mrn}`;
}
"#;

const ADT: &str = "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|20261005120000||ADT^A08|MSG0001|P|2.5\rEVN|A08|20261005120000\rPID|1||123456^^^MRN||SMITH^JOHN||19800101|M\rPV1|1|I|4W^401^A";

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let _guard = rt.enter();

    let js = transpile("spike.ts", TS)?;
    let mut iso = Isolate::new();
    iso.load("spike.js", js)?;

    println!("result: {:?}", iso.call("transform", ADT)?);

    let n = 100_000;
    let start = Instant::now();
    for _ in 0..n {
        iso.call("transform", ADT)?;
    }
    let secs = start.elapsed().as_secs_f64();
    println!("{n} calls in {secs:.2}s = {:.0} calls/s", n as f64 / secs);
    Ok(())
}
