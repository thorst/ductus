//! Transforms per core: the guides' ORU to FHIR transform (`examples/guides`, built into the
//! binary) run in V8 on one thread, then on one per CPU thread, each with an isolate of its own
//! as the engine's shared pool has. No database and no engine: what the code costs alone.

use super::WARM_UP;
use super::report::Measured;
use anyhow::{Context, Result, anyhow, bail};
use ductus_script::{Function, InterfaceId, Isolate};
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const TRANSFORM: &str = include_str!("../../../../examples/guides/interfaces/results-to-fhir/to-fhir.ts");
const CODES: &str = include_str!("../../../../examples/guides/lib/codes.ts");
const ORU: &str = include_str!("../../../../examples/guides/interfaces/results-to-fhir/fixtures/oru-r01.hl7");
/// HL7 in, the transform's object out as JSON (the interface's `output: json`).
const FORMATS: &str = r#"{"input":{"kind":"hl7"},"output":{"kind":"json"}}"#;

/// One thread, then all of this machine's (when it has more than one).
pub async fn run(seconds: Duration) -> Result<Vec<Measured>> {
    let dir = tempfile::tempdir().context("making the transform's folder")?;
    let module = dir.path().join("interfaces/results-to-fhir/to-fhir.ts");
    std::fs::create_dir_all(dir.path().join("interfaces/results-to-fhir"))?;
    std::fs::create_dir_all(dir.path().join("lib"))?;
    std::fs::write(&module, TRANSFORM)?;
    std::fs::write(dir.path().join("lib/codes.ts"), CODES)?;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut runs = Vec::new();
    for threads in if cores > 1 { vec![1, cores] } else { vec![1] } {
        let module = module.clone();
        let (count, took) = tokio::task::spawn_blocking(move || on_threads(&module, threads, seconds))
            .await
            .map_err(|e| anyhow!("the transform bench failed: {e}"))??;
        let all = count as f64 / took.as_secs_f64();
        let (id, name) = match threads {
            1 => ("transforms-1".to_string(), "1 thread".to_string()),
            n => ("transforms-all".to_string(), format!("{n} threads")),
        };
        runs.push(Measured {
            id,
            name,
            numbers: vec![
                ("transforms_per_second", all.round()),
                ("per_thread_per_second", (all / threads as f64).round()),
            ],
        });
    }
    Ok(runs)
}

/// Transforms done on `threads` threads together for `seconds` (after the warm-up), and how long
/// the measured part took.
fn on_threads(module: &Path, threads: usize, seconds: Duration) -> Result<(u64, Duration)> {
    let message = ORU.replace("\r\n", "\r").replace('\n', "\r");
    // Every thread warmed up before any is measured, and one clock for all: each reaches both
    // barriers exactly once, also when it failed to load.
    let ready = Arc::new(Barrier::new(threads + 1));
    let go = Arc::new(Barrier::new(threads + 1));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (module, message, ready, go) = (module.to_path_buf(), message.clone(), ready.clone(), go.clone());
            std::thread::spawn(move || -> Result<u64> {
                let loaded = load(&module, &message);
                ready.wait();
                go.wait();
                let (rt, mut isolate, interface) = loaded?;
                let until = Instant::now() + seconds;
                let mut count = 0;
                while Instant::now() < until {
                    // A hundred between clock reads.
                    for _ in 0..100 {
                        rt.block_on(isolate.transform_with(interface, &message))?;
                    }
                    count += 100;
                }
                Ok(count)
            })
        })
        .collect();
    ready.wait();
    let began = Instant::now();
    go.wait();
    let mut count = 0;
    for worker in workers {
        count += worker.join().map_err(|_| anyhow!("a transform thread panicked"))??;
    }
    Ok((count, began.elapsed()))
}

/// A thread's isolate with the transform loaded, its result checked to be the guide's Bundle,
/// warmed up (V8 compiles hot code as it runs).
fn load(module: &Path, message: &str) -> Result<(tokio::runtime::Runtime, Isolate, InterfaceId)> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let mut isolate = {
        let _guard = rt.enter();
        Isolate::host()
    };
    let function = Function { module, name: "transform", formats: FORMATS };
    let (interface, _) = rt.block_on(isolate.add_functions(&[Some(function)], Duration::from_secs(5)))?;
    let out = rt.block_on(isolate.transform_with(interface, message))?.unwrap_or_default();
    if !out.contains(r#""resourceType":"Bundle""#) || !out.contains("Observation") {
        bail!("the guides' transform didn't give its FHIR Bundle: {out}");
    }
    let warm = Instant::now() + WARM_UP;
    while Instant::now() < warm {
        rt.block_on(isolate.transform_with(interface, message))?;
    }
    Ok((rt, isolate, interface))
}
