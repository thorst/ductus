//! Module loading: local `.ts`/`.js` files plus the built-in `@ductus/sdk`.
//! Remote (http) imports are refused: interface code only runs what's in the project.
//!
//! Every project file loaded is also recorded (`Loaded`): its text as hashed and the project files
//! it imports. That's how the engine knows what each interface runs (its code version) without
//! reading or parsing anything a second time.

use deno_ast::swc::ast;
use deno_ast::swc::ecma_visit::{Visit, VisitWith};
use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource, ModuleSourceCode,
    ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};
use deno_error::JsErrorBox;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const SDK_SPECIFIER: &str = "@ductus/sdk";
const SDK_URL: &str = "ductus:///sdk/index.ts";

/// The SDK's TypeScript sources, compiled into the binary.
const SDK_FILES: &[(&str, &str)] = &[
    ("/sdk/index.ts", include_str!("../../../sdk/src/index.ts")),
    ("/sdk/hl7.ts", include_str!("../../../sdk/src/hl7.ts")),
    ("/sdk/formats.ts", include_str!("../../../sdk/src/formats.ts")),
    ("/sdk/http.ts", include_str!("../../../sdk/src/http.ts")),
    ("/sdk/schema.ts", include_str!("../../../sdk/src/schema.ts")),
    ("/sdk/services.ts", include_str!("../../../sdk/src/services.ts")),
    ("/sdk/testing.ts", include_str!("../../../sdk/src/testing.ts")),
    ("/sdk/typegen.ts", include_str!("../../../sdk/src/typegen.ts")),
];

/// Transpile TypeScript to JavaScript (type annotations stripped, no type check).
pub fn transpile(specifier: &ModuleSpecifier, ts: &str) -> anyhow::Result<String> {
    transpile_parsed(parse(specifier, ts, deno_ast::MediaType::TypeScript)?)
}

fn parse(
    specifier: &ModuleSpecifier,
    text: &str,
    media_type: deno_ast::MediaType,
) -> anyhow::Result<deno_ast::ParsedSource> {
    Ok(deno_ast::parse_module(deno_ast::ParseParams {
        specifier: specifier.clone(),
        text: text.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })?)
}

fn transpile_parsed(parsed: deno_ast::ParsedSource) -> anyhow::Result<String> {
    let emitted = parsed.transpile(
        &deno_ast::TranspileOptions {
            imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
            ..Default::default()
        },
        &Default::default(),
        &Default::default(),
    )?;
    Ok(emitted.into_source().text)
}

/// A file's text as its code version counts it: without a byte order mark, lines ending in `\n`,
/// so a Windows checkout (CRLF) and a Linux one of the same commit have the same version.
pub fn normalize(text: &str) -> String {
    text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n")
}

/// SHA-256 of a normalized text.
pub fn hash(normalized: &str) -> [u8; 32] {
    Sha256::digest(normalized.as_bytes()).into()
}

/// A project file the loader read.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// Its text, normalized (`normalize`).
    pub text: String,
    /// `hash(text)`.
    pub hash: [u8; 32],
    /// The project files it imports (`file:` URLs): static imports, re-exports and `import("...")`
    /// of a literal, whether or not they've been loaded yet.
    pub imports: Vec<String>,
}

/// Every project file an isolate's loader read, by `file:` URL.
#[derive(Default)]
pub struct Loaded(RefCell<BTreeMap<String, LoadedModule>>);

impl Loaded {
    pub fn get(&self, url: &str) -> Option<LoadedModule> {
        self.0.borrow().get(url).cloned()
    }

    /// Drop what's recorded (its texts): what's loaded later is recorded from then on.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

#[derive(Default)]
pub struct Loader {
    pub loaded: Rc<Loaded>,
}

impl ModuleLoader for Loader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        if specifier == SDK_SPECIFIER {
            return ModuleSpecifier::parse(SDK_URL).map_err(JsErrorBox::from_err);
        }
        resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        _referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        ModuleLoadResponse::Sync(load(specifier, &self.loaded))
    }
}

fn load(specifier: &ModuleSpecifier, loaded: &Loaded) -> Result<ModuleSource, ModuleLoaderError> {
    let code = match specifier.scheme() {
        "ductus" => {
            let index = SDK_FILES
                .iter()
                .position(|(path, _)| *path == specifier.path())
                .ok_or_else(|| JsErrorBox::generic(format!("no such SDK module: {specifier}")))?;
            sdk_javascript(index, specifier).map_err(generic)?
        }
        "file" => {
            let path =
                specifier.to_file_path().map_err(|_| JsErrorBox::generic(format!("bad file URL: {specifier}")))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| JsErrorBox::generic(format!("cannot read {}: {e}", path.display())))?;
            let media_type = match path.extension().and_then(|e| e.to_str()) {
                Some("ts" | "mts") => deno_ast::MediaType::TypeScript,
                Some("js" | "mjs") => deno_ast::MediaType::JavaScript,
                _ => return Err(JsErrorBox::generic(format!("unsupported module type: {}", path.display()))),
            };
            // JavaScript goes to V8 as written (it reports its own syntax errors): parsed here only
            // for its imports.
            let parsed = parse(specifier, &text, media_type);
            let normalized = normalize(&text);
            let found = parsed.as_ref().map(|p| imports(p, specifier)).unwrap_or_default();
            let module = LoadedModule { hash: hash(&normalized), text: normalized, imports: found };
            loaded.0.borrow_mut().insert(specifier.to_string(), module);
            match media_type {
                deno_ast::MediaType::TypeScript => transpile_parsed(parsed.map_err(generic)?).map_err(generic)?,
                _ => text,
            }
        }
        other => {
            return Err(JsErrorBox::generic(format!(
                "imports from `{other}:` are not allowed ({specifier}); interface code may only import project files and @ductus/sdk"
            )));
        }
    };
    Ok(ModuleSource::new(ModuleType::JavaScript, ModuleSourceCode::String(code.into()), specifier, None))
}

/// The SDK file `SDK_FILES[index]` as JavaScript: transpiled once per process, then shared by every
/// isolate. Each new isolate used to transpile the SDK again (~8 ms, a third of what `validate` and
/// `test` spent per interface, which make one isolate each).
fn sdk_javascript(index: usize, specifier: &ModuleSpecifier) -> anyhow::Result<String> {
    static JAVASCRIPT: [std::sync::OnceLock<String>; SDK_FILES.len()] =
        [const { std::sync::OnceLock::new() }; SDK_FILES.len()];
    if let Some(js) = JAVASCRIPT[index].get() {
        return Ok(js.clone());
    }
    // Two threads may both transpile the first time: the same text, one kept.
    let js = transpile(specifier, SDK_FILES[index].1)?;
    Ok(JAVASCRIPT[index].get_or_init(|| js).clone())
}

/// A project file the loader hasn't read (yet): an `import("...")` that hasn't run. None when it
/// can't be read (that import fails when it runs).
pub(crate) fn read(url: &str) -> Option<LoadedModule> {
    let specifier = ModuleSpecifier::parse(url).ok()?;
    let path = specifier.to_file_path().ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let media_type = match path.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs") => deno_ast::MediaType::JavaScript,
        _ => deno_ast::MediaType::TypeScript,
    };
    let found = parse(&specifier, &text, media_type).map(|p| imports(&p, &specifier)).unwrap_or_default();
    let text = normalize(&text);
    Some(LoadedModule { hash: hash(&text), text, imports: found })
}

/// The project files a parsed module imports, as `file:` URLs (the SDK and anything that won't
/// resolve left out: loading refuses those anyway).
pub(crate) fn imports(parsed: &deno_ast::ParsedSource, referrer: &ModuleSpecifier) -> Vec<String> {
    let mut found = Specifiers::default();
    parsed.program_ref().visit_with(&mut found);
    let mut urls: Vec<String> = found
        .0
        .iter()
        .filter_map(|s| resolve_import(s, referrer.as_str()).ok())
        .filter(|url| url.scheme() == "file")
        .map(|url| url.to_string())
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// Import specifiers written in a module: static imports, re-exports, `import("literal")`.
#[derive(Default)]
struct Specifiers(Vec<String>);

impl Visit for Specifiers {
    fn visit_import_decl(&mut self, n: &ast::ImportDecl) {
        self.0.push(n.src.value.to_string_lossy().to_string());
    }

    fn visit_export_all(&mut self, n: &ast::ExportAll) {
        self.0.push(n.src.value.to_string_lossy().to_string());
    }

    fn visit_named_export(&mut self, n: &ast::NamedExport) {
        if let Some(src) = &n.src {
            self.0.push(src.value.to_string_lossy().to_string());
        }
    }

    fn visit_call_expr(&mut self, n: &ast::CallExpr) {
        if matches!(n.callee, ast::Callee::Import(_))
            && let Some(first) = n.args.first()
            && let ast::Expr::Lit(ast::Lit::Str(s)) = &*first.expr
        {
            self.0.push(s.value.to_string_lossy().to_string());
        }
        n.visit_children_with(self);
    }
}

fn generic(e: anyhow::Error) -> JsErrorBox {
    JsErrorBox::generic(format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sdk_is_transpiled_once_and_as_before() {
        for (index, (path, source)) in SDK_FILES.iter().enumerate() {
            let specifier = ModuleSpecifier::parse(&format!("ductus://{path}")).unwrap();
            let first = sdk_javascript(index, &specifier).unwrap();
            assert_eq!(first, transpile(&specifier, source).unwrap(), "{path}: what transpiling gives");
            // From a thread of its own (isolates on other threads share it): the same text.
            let again = std::thread::spawn(move || sdk_javascript(index, &specifier).unwrap()).join().unwrap();
            assert_eq!(again, first, "{path}");
        }
    }
}
