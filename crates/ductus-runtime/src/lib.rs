//! Runs interfaces: receive → persist → ACK, then workers transform and deliver.

mod destination;
mod ingest;
mod worker;

use anyhow::{Context, Result, bail};
use ductus_core::InterfaceConfig;
use ductus_store::Store;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Load every `interfaces/*/interface.yaml` under `root`.
pub fn load_interfaces(root: &Path) -> Result<Vec<InterfaceConfig>> {
    let dir = root.join("interfaces");
    let mut configs = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.join("interface.yaml").is_file() {
            configs.push(InterfaceConfig::load(&path)?);
        }
    }
    configs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names: Vec<_> = configs.iter().map(|c| c.name.as_str()).collect();
    names.dedup();
    if names.len() != configs.len() {
        bail!("two interfaces share a name");
    }
    Ok(configs)
}

/// Start every interface under `root` and run until Ctrl-C.
pub async fn run(root: &Path, database_url: &str) -> Result<()> {
    let configs = load_interfaces(root)?;
    if configs.is_empty() {
        bail!("no interfaces found under {}", root.join("interfaces").display());
    }
    let total_workers: u32 = configs.iter().map(|c| c.workers).sum();
    let store = Store::connect(database_url, total_workers + 8).await?;

    for config in configs {
        let config = Arc::new(config);
        let script = match config.script_path() {
            Some(path) => {
                let ts = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                Some(ductus_script::transpile(&path.to_string_lossy(), &ts)?)
            }
            None => None,
        };
        let wake = Arc::new(Notify::new());

        for w in 0..config.workers {
            worker::spawn(config.clone(), w, script.clone(), store.clone(), wake.clone())?;
        }
        ingest::spawn(config.clone(), store.clone(), wake).await?;
        tracing::info!(interface = %config.name, workers = config.workers, "started");
    }

    let maintenance = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Err(e) = maintenance.ensure_partitions(3).await {
                tracing::error!(error = %e, "partition maintenance failed");
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}

/// Current local time in HL7 TS format.
pub(crate) fn hl7_now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}
