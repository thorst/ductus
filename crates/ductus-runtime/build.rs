//! Embeds the web UI (`ui/dist`, made by `npm run build` in `ui/`) in the binary, each file with
//! a gzipped copy when that's smaller, so the engine stays one file to install.
//!
//! Cargo never runs npm (no network or Node in a Rust build): without a built UI, a page saying
//! how to build it is embedded instead, so working on the Rust side needs nothing more.
//! `DUCTUS_REQUIRE_UI=1` (CI, releases) makes a missing UI a build error.

use flate2::{Compression, write::GzEncoder};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{env, fs};

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ductus</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;color:#1f2933;background:#fff}
@media (prefers-color-scheme:dark){body{color:#e4e7eb;background:#111418}}code{font-size:.95em}</style></head>
<body><h1>Ductus</h1><p>This engine was built without its web UI. Build it, then the engine again:</p>
<pre><code>cd ui &amp;&amp; npm ci &amp;&amp; npm run build
cargo build --release</code></pre>
<p>The REST API works either way: <a href="/api/v1/openapi.json">/api/v1/openapi.json</a>.</p></body></html>
"#;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-env-changed=DUCTUS_REQUIRE_UI");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ui");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
    } else if env::var_os("DUCTUS_REQUIRE_UI").is_some_and(|v| v != "0") {
        panic!("the web UI isn't built ({} has no index.html): cd ui && npm ci && npm run build", dist.display());
    } else {
        println!("cargo:warning=building without the web UI (cd ui && npm ci && npm run build to include it)");
        files.push(("/index.html".into(), PLACEHOLDER.as_bytes().to_vec()));
    }
    files.sort();

    let mut table = String::from("pub static ASSETS: &[Asset] = &[\n");
    for (n, (path, body)) in files.iter().enumerate() {
        fs::write(out.join(n.to_string()), body).unwrap();
        let gzip = if compressible(path) {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
            encoder.write_all(body).unwrap();
            let gz = encoder.finish().unwrap();
            // Not worth a `Content-Encoding` for less than a tenth.
            (gz.len() < body.len() * 9 / 10).then_some(gz)
        } else {
            None
        };
        let gzip = match gzip {
            Some(gz) => {
                fs::write(out.join(format!("{n}.gz")), gz).unwrap();
                format!("Some(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ui/{n}.gz\")))")
            }
            None => "None".into(),
        };
        table.push_str(&format!(
            "    Asset {{ path: {path:?}, body: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ui/{n}\")), gzip: {gzip} }},\n"
        ));
    }
    table.push_str("];\n");
    fs::write(out.join("assets.rs"), table).unwrap();
}

/// Every file under `dir`, as a URL path from `root`.
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            files.push((format!("/{rel}"), fs::read(&path).unwrap()));
        }
    }
}

/// Already-compressed formats gain nothing.
fn compressible(path: &str) -> bool {
    ![".png", ".jpg", ".jpeg", ".webp", ".avif", ".woff", ".woff2", ".gz", ".br", ".zip"]
        .iter()
        .any(|ext| path.ends_with(ext))
}
