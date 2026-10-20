//! `ductus cert`: certificates for `tls:` on listeners.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// `ductus cert self-signed OUT --name host`: OUT.pem and OUT.key, and the entry to paste.
pub fn self_signed(out: &Path, names: &[String], days: u32) -> Result<()> {
    let with = |ext: &str| -> PathBuf {
        let mut file = out.as_os_str().to_owned();
        file.push(ext);
        file.into()
    };
    let (cert_file, key_file) = (with(".pem"), with(".key"));
    for file in [&cert_file, &key_file] {
        if file.exists() {
            bail!("{} exists already: remove it first, or write somewhere else", file.display());
        }
    }
    let (cert, key) = ductus_runtime::tls::self_signed(names, days)?;
    if let Some(dir) = cert_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&cert_file, cert).with_context(|| format!("writing {}", cert_file.display()))?;
    write_private(&key_file, key.as_bytes())?;
    let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ductus".into());
    println!(
        "wrote {} (the certificate, for {}) and {} (its key: keep it private), valid {days} days\n\n\
         In resources.yaml:\n\n  certificates:\n    {name}:\n      cert_file: {}\n      key_file: {}\n\n\
         It is its own CA: senders must be given {} to trust it.",
        cert_file.display(),
        names.join(", "),
        key_file.display(),
        cert_file.display(),
        key_file.display(),
        cert_file.display()
    );
    Ok(())
}

/// A file only its owner can read (on Unix; Windows files inherit their folder's permissions).
fn write_private(file: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(file).with_context(|| format!("writing {}", file.display()))?;
    f.write_all(contents).with_context(|| format!("writing {}", file.display()))
}
