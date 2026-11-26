//! Interfaces per GB: an engine in a process of its own (this binary's `run`, on the bench's
//! schema), with 100 and then 1,000 idle MLLP interfaces, each with a transform importing a
//! shared file, as projects have. Its resident memory once it's up and settled, against the
//! count, gives what one more interface costs; the bench's own process can't say it (what earlier
//! scenarios freed stays counted in its memory).

use super::report::{Measured, secs};
use super::{ENV, project_files};
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

/// The counts compared.
const FEW: usize = 100;
const MANY: usize = 1_000;
/// After `started`, what runs beside the start (code versions, the audit row) settles first.
const SETTLE: Duration = Duration::from_secs(2);
/// The longest a start may take before the bench gives up.
const START_WAIT: Duration = Duration::from_secs(120);

/// The scenario, or None where a process's memory can't be read (said by the caller).
pub async fn run(url: &str) -> Result<Option<Measured>> {
    if !readable() {
        return Ok(None);
    }
    let few = idle(url, FEW).await?;
    let many = idle(url, MANY).await?;
    let each = (many.memory_mb - few.memory_mb) / (MANY - FEW) as f64;
    Ok(Some(Measured {
        id: "memory-idle".into(),
        name: "idle interfaces".into(),
        numbers: vec![
            ("memory_100_mb", round(few.memory_mb, 1)),
            ("memory_1000_mb", round(many.memory_mb, 1)),
            ("per_interface_mb", round(each, 3)),
            ("interfaces_per_gb", if each > 0.0 { (1024.0 / each).round() } else { 0.0 }),
            ("ready_1000_s", secs(many.ready)),
        ],
    }))
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

struct Idle {
    memory_mb: f64,
    /// From the process's launch to its `started` line.
    ready: Duration,
}

/// Start an engine with `count` interfaces, wait for it to be up and settled, read its memory,
/// stop it.
async fn idle(url: &str, count: usize) -> Result<Idle> {
    let dir = tempfile::tempdir().context("making the memory scenario's project folder")?;
    write_project(dir.path(), count)?;
    let exe = std::env::current_exe().context("finding this program, to start an engine of its own")?;
    let launched = Instant::now();
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("run")
        .arg("--root")
        .arg(dir.path())
        .args(["--env", ENV, "--api-listen", "off"])
        .env("DUCTUS_DATABASE_URL", url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Killed when the bench is (Ctrl-C, a failure): never left running on its own.
        .kill_on_drop(true);
    // The engine's own settings only: what this shell sets for its engine isn't the bench's.
    for name in ["RUST_LOG", "DUCTUS_RELEASES", "DUCTUS_PROJECT", "DUCTUS_TAKEOVER", "DUCTUS_API_LISTEN", "DUCTUS_ENV"]
    {
        command.env_remove(name);
    }
    let mut child = command.spawn().context("starting the memory scenario's engine")?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    // Its log read to the end beside the scenario (a full pipe would stall the engine), the
    // start said once it's seen.
    let (started_tx, started) = tokio::sync::oneshot::channel::<()>();
    let log = tokio::spawn(async move {
        let mut said = String::new();
        let mut started_tx = Some(started_tx);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("started interfaces=")
                && let Some(tx) = started_tx.take()
            {
                let _ = tx.send(());
            }
            if said.len() < 64 * 1024 {
                said += &line;
                said.push('\n');
            }
        }
        let mut rest = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut BufReader::new(stderr), &mut rest).await;
        said.push_str(&rest);
        said
    });
    let up = tokio::time::timeout(START_WAIT, started).await;
    let ready = launched.elapsed();
    if !matches!(up, Ok(Ok(()))) {
        let _ = child.kill().await;
        let said = log.await.unwrap_or_default();
        bail!(
            "the memory scenario's engine ({count} interfaces) didn't start{}: {}",
            if up.is_err() { " within 2 minutes" } else { "" },
            said.trim()
        );
    }
    tokio::time::sleep(SETTLE).await;
    let memory = resident(&child).ok_or_else(|| anyhow!("couldn't read the memory scenario's engine's memory"));
    stop(&mut child).await;
    let _ = log.await;
    Ok(Idle { memory_mb: memory? as f64 / 1024.0 / 1024.0, ready })
}

/// `count` interfaces on ports of their own, each with a transform importing a shared table.
fn write_project(root: &Path, count: usize) -> Result<()> {
    let ports = ports(count)?;
    project_files(root)?;
    std::fs::create_dir_all(root.join("lib"))?;
    std::fs::write(
        root.join("lib/codes.ts"),
        "export const CLASS: Record<string, string> = { I: \"IN\", O: \"OUT\", E: \"ER\" };\n\
         export function translate(code: string): string {\n  return CLASS[code] ?? code;\n}\n",
    )?;
    for (i, port) in ports.into_iter().enumerate() {
        let name = format!("idle-{i}");
        let folder = root.join("interfaces").join(&name);
        std::fs::create_dir_all(&folder)?;
        std::fs::write(
            folder.join("interface.yaml"),
            format!(
                "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nordering: {{ key: PID-3.1 }}\nscript: transform.ts\ndestinations: {{ out: {{ file: {{ dir: out }} }} }}\n"
            ),
        )?;
        std::fs::write(
            folder.join("transform.ts"),
            "import type { Message } from \"@ductus/sdk\";\nimport { translate } from \"../../lib/codes.ts\";\n\n\
             export function transform(msg: Message) {\n  msg.PV1[2] = translate(String(msg.PV1[2]));\n  return msg;\n}\n",
        )?;
    }
    Ok(())
}

/// `count` loopback ports nothing listens on now (all held at once while picked, so none twice).
fn ports(count: usize) -> Result<Vec<u16>> {
    let held: Vec<std::net::TcpListener> = (0..count)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<_>>()
        .context("finding free ports for the memory scenario")?;
    held.iter().map(|l| Ok(l.local_addr()?.port())).collect()
}

/// Stop the engine as its service manager would (Ctrl-C's signal on Linux), killing it if it
/// takes longer than a few seconds; killed at once elsewhere (nothing of it is kept).
async fn stop(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) on our own child's pid, which hasn't been waited for yet.
        unsafe { libc::kill(pid as i32, libc::SIGINT) };
        if tokio::time::timeout(Duration::from_secs(10), child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(target_os = "linux")]
fn readable() -> bool {
    true
}

/// The child's resident memory in bytes (VmRSS).
#[cfg(target_os = "linux")]
fn resident(child: &tokio::process::Child) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", child.id()?)).ok()?;
    let kb: u64 =
        status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

#[cfg(windows)]
fn readable() -> bool {
    true
}

/// The child's working set in bytes (what Task Manager shows as its memory).
#[cfg(windows)]
fn resident(child: &tokio::process::Child) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let handle = child.raw_handle()?;
    let mut counters =
        PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: the handle is our running child's; `counters` is the struct the call fills, its size given.
    let ok = unsafe { K32GetProcessMemoryInfo(handle as _, &mut counters, counters.cb) };
    (ok != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn readable() -> bool {
    false
}

#[cfg(not(any(target_os = "linux", windows)))]
fn resident(_: &tokio::process::Child) -> Option<u64> {
    None
}
