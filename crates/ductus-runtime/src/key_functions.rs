//! Key functions (`ordering: { key: { script: key.ts } }`, 0.7.7.3): keys that paths can't say,
//! read by the interface's own code at intake.
//!
//! Every interface's key functions run on one thread of the engine's, in one isolate, made when
//! the first is loaded: an engine without any has no thread at all. Not a worker's isolate: a key
//! is read while the sender waits for its ACK, and a worker's thread may be busy with a slow
//! transform for seconds. A key function is synchronous and quick (microseconds); one that runs
//! past [`ductus_script::KEY_TIMEOUT`] is stopped, and its message stored quarantined with why, as
//! for one that throws. Intake then goes on: the next message's key is read as usual.
//!
//! A message crosses to the thread and back (a channel each way, the payload copied): measured
//! in history.md (0.7.7.3) against reading a path, which needs no thread at all.

use anyhow::{Context, Result, anyhow};
use ductus_core::InterfaceConfig;
use ductus_core::keys::KeyFunction;
use ductus_script::{FunctionId, Isolate, Runaway};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, mpsc};
use tokio::sync::oneshot;

/// The engine's key thread: started by the first `load`.
pub struct KeyThread {
    commands: OnceLock<mpsc::Sender<Command>>,
    /// The project folder (`console` lines' places are relative to it) and the environment's
    /// `console:`.
    root: PathBuf,
    console: ductus_core::ConsoleLevel,
}

enum Command {
    Load {
        /// The functions (their files and names) and the interface's formats.
        functions: Vec<ductus_core::config::Function>,
        reply: oneshot::Sender<Result<Vec<FunctionId>>>,
    },
    Run {
        functions: Arc<[FunctionId]>,
        /// The interface's project, which errors' places are relative to.
        places: Arc<ductus_script::errors::Root>,
        messages: Vec<String>,
        reply: oneshot::Sender<Vec<Result<Vec<String>, Failed>>>,
    },
}

/// Why a key function gave no key.
#[derive(Debug, Clone, PartialEq)]
pub struct Failed {
    /// Which of the interface's key functions (`InterfaceConfig::key_scripts`).
    pub function: usize,
    /// What it threw (with its place in the project), or that it was stopped.
    pub why: String,
    /// It ran past its time and was stopped.
    pub ran_away: bool,
}

impl KeyThread {
    pub fn new(root: PathBuf, console: ductus_core::ConsoleLevel) -> Arc<Self> {
        Arc::new(Self { commands: OnceLock::new(), root, console })
    }

    fn commands(&self) -> &mpsc::Sender<Command> {
        self.commands.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let (root, console) = (self.root.clone(), self.console);
            std::thread::Builder::new()
                .name("key-functions".into())
                .spawn(move || serve(rx, root, console))
                .expect("starting the key functions' thread");
            tx
        })
    }

    /// Load `config`'s key functions (`InterfaceConfig::key_functions`), its files as they are on
    /// disk now. None when it has none. An error when one doesn't load (its file throws at its top
    /// level, or doesn't export the function).
    pub async fn load(self: &Arc<Self>, config: &InterfaceConfig) -> Result<Option<Arc<Loaded>>> {
        let functions = config.key_functions();
        if functions.is_empty() {
            return Ok(None);
        }
        let (reply, answer) = oneshot::channel();
        self.commands().send(Command::Load { functions, reply }).map_err(|_| anyhow!("the key thread stopped"))?;
        let ids = answer.await.map_err(|_| anyhow!("the key thread stopped"))?;
        let ids = ids.with_context(|| format!("interface `{}`: loading its key functions", config.name))?;
        let written = config.key_scripts().into_iter().cloned().collect();
        let places = Arc::new(ductus_script::errors::Root::new(config.project_root()));
        Ok(Some(Arc::new(Loaded { thread: self.clone(), ids: ids.into(), places, written })))
    }
}

/// One interface's key functions, loaded (`KeyThread::load`).
pub struct Loaded {
    thread: Arc<KeyThread>,
    ids: Arc<[FunctionId]>,
    places: Arc<ductus_script::errors::Root>,
    /// As written in interface.yaml, in `ids`' order.
    pub written: Vec<KeyFunction>,
}

impl Loaded {
    /// Each message's key functions' values (in `written`'s order), or why one gave none. An
    /// error only when the thread is gone (nothing was read: the messages aren't stored).
    pub async fn run(&self, messages: Vec<String>) -> Result<Vec<Result<Vec<String>, Failed>>> {
        let (reply, answer) = oneshot::channel();
        let run = Command::Run { functions: self.ids.clone(), places: self.places.clone(), messages, reply };
        self.thread.commands().send(run).map_err(|_| anyhow!("the key thread stopped"))?;
        answer.await.map_err(|_| anyhow!("the key thread stopped"))
    }
}

/// The key thread: one isolate, commands one at a time.
fn serve(commands: mpsc::Receiver<Command>, root: PathBuf, console: ductus_core::ConsoleLevel) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "the key functions' thread couldn't start");
            return;
        }
    };
    let _inside = runtime.enter(); // V8 posts its delayed tasks to the runtime
    // No services: a key function reads the message, nothing else (`db`, `api` and `fetch` fail).
    let mut isolate = Isolate::new(None, ductus_script::DEFAULT_HEAP_MB);
    isolate.console_to_log(&root, console);
    // V8 knows a module by its URL: a file loaded before (a deploy in place) loads again under a
    // new generation, so its new text is read.
    let (mut loaded, mut generation) = (HashSet::<PathBuf>::new(), 0);
    // A blocking receive: a key call is synchronous, so nothing here needs a runtime between
    // commands. One message a trip costs two threads' wake-ups (~60 µs in WSL, the same through
    // tokio's channel); a batch's messages share them (measured: history.md 0.7.7.3).
    while let Ok(command) = commands.recv() {
        match command {
            Command::Load { functions, reply } => {
                // Every module is recorded (no short-circuit), then any seen before means again.
                let seen_before = functions.iter().filter(|f| !loaded.insert(f.module.clone())).count();
                let again = seen_before > 0;
                if again {
                    generation += 1;
                }
                isolate.set_generation(if again { generation } else { 0 });
                let borrowed: Vec<_> = functions.iter().map(ductus_script::Function::of).collect();
                let ids = runtime.block_on(isolate.add_keys(&borrowed)).map(|(_, ids)| ids);
                let _ = reply.send(ids);
            }
            Command::Run { functions, places, messages, reply } => {
                let keys = messages
                    .iter()
                    .map(|message| {
                        functions
                            .iter()
                            .enumerate()
                            .map(|(i, &f)| isolate.call_key(f, message).map_err(|e| failed(i, &e, &places)))
                            .collect()
                    })
                    .collect();
                let _ = reply.send(keys);
            }
        }
    }
}

/// Why function `function` gave no key, as a quarantine reason says it: what it threw and where,
/// a deliberate `quarantine("…")`'s reason alone, or that it was stopped.
fn failed(function: usize, error: &anyhow::Error, places: &ductus_script::errors::Root) -> Failed {
    if let Some(runaway) = error.downcast_ref::<Runaway>() {
        return Failed { function, why: runaway.to_string(), ran_away: true };
    }
    let text = format!("{error:#}");
    let why = match text.split_once(ductus_script::QUARANTINE) {
        Some((_, reason)) => reason.lines().next().unwrap_or("").trim().to_string(),
        None => places.readable(&text),
    };
    Failed { function, why, ran_away: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An interface keyed by `key` (as written under `ordering`), with `code` as its key.ts.
    fn interface(key: &str, code: &str) -> (tempfile::TempDir, InterfaceConfig) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("key.ts"), code).unwrap();
        let yaml = format!(
            "name: k\nsource: {{ mllp: {{ port: 1 }} }}\nordering: {{ key: {key} }}\ndestinations: {{ f: {{ file: {{ dir: o }} }} }}\n"
        );
        let mut config: InterfaceConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        config.dir = dir.path().to_path_buf();
        (dir, config)
    }

    const ADT: &str =
        "MSH|^~\\&|EPIC|HOSP|LAB|HOSP|20261121||ADT^A08|1|P|2.5\rPID|1||123456^^^^MR~999^^^^SS||SMITH^J\r";

    /// Loading again (a deploy in place) reads the file's new text; a failure names the function.
    #[tokio::test]
    async fn keys_come_back_and_a_reload_reads_the_file_again() {
        let keys = KeyThread::new(".".into(), Default::default());
        let (dir, config) =
            interface("{ script: key.ts }", "export function key(m: any) { return m.get(\"PID-3[.5=MR].1\"); }\n");
        let first = keys.load(&config).await.unwrap().unwrap();
        let got = first.run(vec![ADT.into(), "MSH|^~\\&|\r".into()]).await.unwrap();
        assert_eq!(got, [Ok(vec!["123456".to_string()]), Ok(vec![String::new()])]);

        std::fs::write(dir.path().join("key.ts"), "export function key(m: any) { throw new Error(\"changed\"); }\n")
            .unwrap();
        let second = keys.load(&config).await.unwrap().unwrap();
        let failed = second.run(vec![ADT.into()]).await.unwrap().pop().unwrap().unwrap_err();
        assert_eq!((failed.function, failed.ran_away), (0, false));
        assert!(failed.why.starts_with("Error: changed (") && failed.why.contains("key.ts:1:"), "{}", failed.why);
        assert_eq!(first.run(vec![ADT.into()]).await.unwrap(), [Ok(vec!["123456".to_string()])], "the old one stays");
    }

    /// What a key function costs at intake against a path (release mode: `cargo test --release -p
    /// ductus-runtime measure_a_key_function -- --ignored --nocapture`): one message a trip (as
    /// an MLLP message comes), and a hundred (as a file's batch).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn measure_a_key_function_against_a_path() {
        const N: u32 = 20_000;
        let keys = KeyThread::new(".".into(), Default::default());
        let (_dir, config) =
            interface("{ script: key.ts }", "export function key(m: any) { return m.get(\"PID-3[.5=MR].1\"); }\n");
        let loaded = keys.load(&config).await.unwrap().unwrap();
        for _ in 0..2_000 {
            loaded.run(vec![ADT.into()]).await.unwrap(); // warm: V8 compiles the hot code
        }
        let start = std::time::Instant::now();
        for _ in 0..N {
            loaded.run(vec![ADT.into()]).await.unwrap();
        }
        let one = start.elapsed() / N;
        let start = std::time::Instant::now();
        for _ in 0..N / 100 {
            loaded.run(vec![ADT.to_string(); 100]).await.unwrap();
        }
        let batched = start.elapsed() / N;
        let path = ductus_core::keys::compile(&ductus_core::format::Format::Hl7, &"PID-3[.5=MR].1".into()).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(crate::keys::Document::new(&ductus_core::format::Format::Hl7, ADT).read(&path));
        }
        let read = start.elapsed() / N;
        println!("per message: key function {one:?} (one a trip), {batched:?} (100 a trip); the path {read:?}");
    }
}
