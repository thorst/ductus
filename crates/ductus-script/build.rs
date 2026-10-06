//! Embeds the vendored TypeScript standard library typings (`lib.*.d.ts`) for the type checker.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let lib_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/typescript/lib");
    println!("cargo::rerun-if-changed={}", lib_dir.display());

    let mut names: Vec<_> = std::fs::read_dir(&lib_dir)
        .expect("vendor/typescript/lib missing")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lib.") && n.ends_with(".d.ts"))
        .collect();
    names.sort();

    let mut out = String::from("pub const TS_LIBS: &[(&str, &str)] = &[\n");
    for name in names {
        let path = lib_dir.join(&name).canonicalize().unwrap();
        writeln!(out, "    ({name:?}, include_str!({:?})),", path.display().to_string()).unwrap();
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("ts_libs.rs");
    std::fs::write(dest, out).unwrap();
}
