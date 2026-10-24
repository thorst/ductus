//! `ductus cert`: certificates for `tls:` (`certificates:` in resources.yaml): a request for the
//! hospital's CA, a look at what a file holds, a self-signed pair for labs.

use anyhow::{Context, Result, bail};
use ductus_core::resources::Pem;
use ductus_core::{Environment, Resources};
use ductus_runtime::cert_show::{self, Key};
use ductus_runtime::doctor::Outcome;
use ductus_runtime::tls::{Certificates, KeyType};
use std::path::{Path, PathBuf};

/// `out` with `ext` added: `tls/engine` + `.pem`.
fn with(out: &Path, ext: &str) -> PathBuf {
    let mut file = out.as_os_str().to_owned();
    file.push(ext);
    file.into()
}

/// The entry's name: the last part of `out`.
fn entry_name(out: &Path) -> String {
    out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ductus".into())
}

/// Refuse to write over anything: a key overwritten is a certificate lost.
fn none_exist(files: &[&PathBuf]) -> Result<()> {
    for file in files {
        if file.exists() {
            bail!("{} exists already: remove it first, or write somewhere else", file.display());
        }
    }
    if let Some(dir) = files[0].parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    Ok(())
}

/// `ductus cert request OUT --dns host [--ip addr]`: a new key (OUT.key, private) and a request
/// for the hospital's CA (OUT.csr), and what to do with each.
pub fn request(out: &Path, dns: &[String], ips: &[String], subject: &str, key: KeyType) -> Result<()> {
    for ip in ips {
        ip.parse::<std::net::IpAddr>()
            .map_err(|_| anyhow::anyhow!("--ip {ip}: not an IP address (names go in --dns)"))?;
    }
    if let Some(ip) = dns.iter().find(|d| d.parse::<std::net::IpAddr>().is_ok()) {
        bail!("--dns {ip} is an IP address: give it as --ip {ip}");
    }
    let names: Vec<String> = dns.iter().chain(ips).cloned().collect();
    let (csr_file, key_file, cert_file) = (with(out, ".csr"), with(out, ".key"), with(out, ".pem"));
    none_exist(&[&key_file, &csr_file])?;
    let (csr, key_pem) = ductus_runtime::tls::request(&names, subject, key)?;
    write_private(&key_file, key_pem.as_bytes())?;
    std::fs::write(&csr_file, &csr).with_context(|| format!("writing {}", csr_file.display()))?;
    let kind = match key {
        KeyType::Rsa => "RSA 2048",
        KeyType::Ecdsa => "ECDSA P-256",
    };
    println!(
        "wrote {key} (the new {kind} private key: it stays on this server; only you can read it) and {csr} (the request)\n\n\
         Send {csr} to the hospital's CA (its PKI or security team). It asks for a certificate for {names}, \
         for a TLS server and client (one certificate can serve listeners and be the engine's client certificate). \
         Never send {key}.\n\n\
         What comes back: the certificate, and usually the CA's own certificates (intermediates). Put them in {pem} \
         as PEM text (-----BEGIN CERTIFICATE-----), ours first, then the intermediates, and check it:\n\n  \
         ductus cert show {pem}\n\n\
         In resources.yaml:\n\n  certificates:\n    {name}:\n      cert_file: {pem}\n      key_file: {key}\n\n\
         To renew later: a new request under another name, then copy its .pem and .key over these two: the engine \
         uses them for new connections within a minute, no restart.",
        key = key_file.display(),
        csr = csr_file.display(),
        pem = cert_file.display(),
        names = names.join(", "),
        name = entry_name(out),
    );
    Ok(())
}

/// `ductus cert show FILE|NAME`: what a certificate file (or a `certificates:` entry) holds,
/// judged. False when something is wrong (the engine would refuse it, or senders would).
pub fn show(target: &str, key: Option<&Path>, root: &Path, env: &str) -> Result<bool> {
    let file = Path::new(target);
    let project = || -> Result<Resources> {
        let environment = Environment::load(root, env)?;
        Resources::load(root, &environment).context("resources.yaml")
    };
    // (what it is, the certificate's PEM, the key: where from and its PEM)
    let (title, pem, key, resources) = if file.is_file() {
        // The key named, else the one beside it (tls/engine.pem → tls/engine.key).
        let beside = file.with_extension("key");
        let key = key.map(Path::to_path_buf).or_else(|| (beside != file && beside.is_file()).then_some(beside));
        let key = key.map(|k| Ok::<_, anyhow::Error>((k.display().to_string(), read(&k)?))).transpose()?;
        // The project's CAs, if there is a project here: a chain ending at one is complete.
        (file.display().to_string(), read(file)?, key, project().ok())
    } else {
        let resources =
            project().with_context(|| format!("{target} isn't a file, so it would be a `certificates:` entry"))?;
        let Some(entry) = resources.certificates.get(target) else {
            let names: Vec<&str> = resources.certificates.keys().map(String::as_str).collect();
            let known = if names.is_empty() { "there are none".to_string() } else { names.join(", ") };
            bail!("{target}: no such file, and no `certificates:` entry by that name in resources.yaml ({known})");
        };
        let named = format!("certificate `{target}` (resources.yaml)");
        let (title, pem, key) = match entry.pem() {
            Some(Pem::Files { cert, key }) => {
                (format!("{named}: {}", cert.display()), read(cert)?, Some((key.display().to_string(), read(key)?)))
            }
            Some(Pem::Text { cert, key }) => (
                format!("{named}: from its text"),
                cert.as_bytes().to_vec(),
                Some(("its key text".into(), key.as_bytes().to_vec())),
            ),
            Some(Pem::CaFile(f)) => (format!("{named}: a CA, {}", f.display()), read(f)?, None),
            Some(Pem::CaText(text)) => (format!("{named}: a CA, from its text"), text.as_bytes().to_vec(), None),
            None => {
                bail!("{named}: neither a certificate with its key nor a CA (`ductus validate` says what's missing)")
            }
        };
        (title, pem, key, Some(resources))
    };
    let certificates = resources.map(|r| Certificates::load_what_loads(&r.certificates)).unwrap_or_default();
    let key = key.as_ref().map(|(from, pem)| Key { from: from.clone(), pem });
    let shown = cert_show::show(&pem, key, &certificates);
    println!("{} {title}", shown.outcome().mark());
    print!("{}", shown.to_text());
    Ok(shown.outcome() != Outcome::Fail)
}

fn read(file: &Path) -> Result<Vec<u8>> {
    std::fs::read(file).with_context(|| format!("reading {}", file.display()))
}

/// `ductus cert self-signed OUT --name host`: OUT.pem and OUT.key, and the entry to paste.
pub fn self_signed(out: &Path, names: &[String], days: u32) -> Result<()> {
    let (cert_file, key_file) = (with(out, ".pem"), with(out, ".key"));
    none_exist(&[&cert_file, &key_file])?;
    let (cert, key) = ductus_runtime::tls::self_signed(names, days)?;
    std::fs::write(&cert_file, cert).with_context(|| format!("writing {}", cert_file.display()))?;
    write_private(&key_file, key.as_bytes())?;
    let name = entry_name(out);
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
