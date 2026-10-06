//! Module loading: local `.ts`/`.js` files plus the built-in `@ductus/sdk`.
//! Remote (http) imports are refused: interface code only runs what's in the project.

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource, ModuleSourceCode,
    ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};
use deno_error::JsErrorBox;

const SDK_SPECIFIER: &str = "@ductus/sdk";
const SDK_URL: &str = "ductus:///sdk/index.ts";

/// The SDK's TypeScript sources, compiled into the binary.
const SDK_FILES: &[(&str, &str)] = &[
    ("/sdk/index.ts", include_str!("../../../sdk/src/index.ts")),
    ("/sdk/hl7.ts", include_str!("../../../sdk/src/hl7.ts")),
    ("/sdk/formats.ts", include_str!("../../../sdk/src/formats.ts")),
    ("/sdk/schema.ts", include_str!("../../../sdk/src/schema.ts")),
    ("/sdk/services.ts", include_str!("../../../sdk/src/services.ts")),
    ("/sdk/testing.ts", include_str!("../../../sdk/src/testing.ts")),
    ("/sdk/typegen.ts", include_str!("../../../sdk/src/typegen.ts")),
];

/// Transpile TypeScript to JavaScript (type annotations stripped, no type check).
pub fn transpile(specifier: &ModuleSpecifier, ts: &str) -> anyhow::Result<String> {
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier: specifier.clone(),
        text: ts.into(),
        media_type: deno_ast::MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })?;
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

#[derive(Default)]
pub struct Loader;

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
        ModuleLoadResponse::Sync(load(specifier))
    }
}

fn load(specifier: &ModuleSpecifier) -> Result<ModuleSource, ModuleLoaderError> {
    let code = match specifier.scheme() {
        "ductus" => {
            let (_, source) = SDK_FILES
                .iter()
                .find(|(path, _)| *path == specifier.path())
                .ok_or_else(|| JsErrorBox::generic(format!("no such SDK module: {specifier}")))?;
            transpile(specifier, source).map_err(generic)?
        }
        "file" => {
            let path =
                specifier.to_file_path().map_err(|_| JsErrorBox::generic(format!("bad file URL: {specifier}")))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| JsErrorBox::generic(format!("cannot read {}: {e}", path.display())))?;
            match path.extension().and_then(|e| e.to_str()) {
                Some("ts" | "mts") => transpile(specifier, &text).map_err(generic)?,
                Some("js" | "mjs") => text,
                _ => return Err(JsErrorBox::generic(format!("unsupported module type: {}", path.display()))),
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

fn generic(e: anyhow::Error) -> JsErrorBox {
    JsErrorBox::generic(format!("{e:#}"))
}
