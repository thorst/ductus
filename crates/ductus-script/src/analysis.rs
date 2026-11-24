//! What a project's code does, read from its syntax: the `db("…")`/`api("…")` names it uses, the
//! imports between its files, its calls of the global `console`, what each file exports (where,
//! whether a function is async, what it waits for), and the lints `validate` reports as `DX1`–`DX5`.
//!
//! No type checker. A name is followed through imports, aliases, namespace imports and re-exports
//! to where it's declared; deno_ast's scope analysis tells a local `db` (or `console`) from the
//! SDK's (or the global one). So nothing here depends on a TypeScript compiler's API, and it runs
//! without V8. Type errors are the type checker's (`check.rs`).

use deno_ast::swc::ast::{self, Expr, Id};
use deno_ast::swc::common::{BytePos, Spanned, SyntaxContext};
use deno_ast::swc::ecma_visit::{Visit, VisitWith};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

const SDK: &str = "@ductus/sdk";

#[derive(Debug, Clone, Deserialize)]
pub struct Diagnostic {
    /// Absolute path, or `None` for global errors.
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
    pub code: u32,
    /// "error", "warning", "suggestion" or "message".
    pub category: String,
    pub message: String,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.category == "error"
    }
}

/// A resource named in code: `db("patient_db")` or `api("empi")`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceReference {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// `db` or `api`.
    pub kind: String,
    pub name: String,
}

/// What the analysis (and, when asked, the type checker) found in a set of files.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Analysis {
    /// Type errors and the lints (`DX`); empty unless diagnostics were asked for.
    pub diagnostics: Vec<Diagnostic>,
    pub references: Vec<ResourceReference>,
    /// Each file's imports that are project files (same path form as the keys passed in).
    pub imports: BTreeMap<String, Vec<String>>,
    /// Calls of the global `console` (empty unless diagnostics were asked for).
    #[serde(default)]
    pub console: Vec<ConsoleCall>,
    /// What each file exports and where it's declared (empty unless diagnostics were asked for).
    #[serde(default)]
    pub exports: Vec<Export>,
}

/// A name a file exports, with where it's declared: in the file itself, or in the file a
/// re-export (`export { f } from "./other.ts"`) leads to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Export {
    /// The exporting file (same path form as the keys passed in).
    pub module: String,
    pub name: String,
    /// Where it's declared: the file, and the line and column of its name (from 1).
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// A function that answers a promise: `async`, or a declared return type of `Promise<…>` or
    /// `PromiseLike<…>`. Read from the syntax: a plain function returning another async
    /// function's result, with no return type written, isn't one here.
    #[serde(default)]
    pub r#async: bool,
    /// What a function's body waits for (`await`, `db()`, `api()`, `fetch()`), each in `file`.
    #[serde(default)]
    pub waits: Vec<Wait>,
}

/// An `await`, or a call of the SDK's `db`, `api` or `fetch`, in an exported function.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Wait {
    pub line: u32,
    pub column: u32,
    /// `await`, `db()`, `api()` or `fetch()`.
    pub what: String,
}

/// `console.log(...)` in code: where, and which method.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConsoleCall {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// `log`, `warn`, ...
    pub method: String,
    /// Outside every function: it runs once, when the module loads (each start), not per call.
    #[serde(rename = "topLevel")]
    pub top_level: bool,
}

impl Analysis {
    /// The resources code reachable from `entry` (through its imports) names, sorted, each once.
    pub fn references_from(&self, entry: &str) -> Vec<&ResourceReference> {
        let mut seen = std::collections::BTreeSet::from([entry]);
        let mut todo = vec![entry];
        while let Some(file) = todo.pop() {
            for next in self.imports.get(file).into_iter().flatten() {
                if seen.insert(next) {
                    todo.push(next);
                }
            }
        }
        let mut found: Vec<_> = self.references.iter().filter(|r| seen.contains(r.file.as_str())).collect();
        found.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
        found.dedup_by(|a, b| (&a.kind, &a.name) == (&b.kind, &b.name));
        found
    }
}

/// The analysis of `files` (keys: absolute paths with `/` separators; `.d.ts` files are skipped).
/// `shared_modules` are files imported by interfaces but not inside one: top-level mutable state in
/// them is flagged. With `full`, also the lints, `console` calls and exports (what `validate`
/// needs); without, only the names and imports (what `impact`, `doctor` and the engine need).
/// A file that doesn't parse adds nothing: the type checker reports its syntax error.
pub fn analyze_syntax(files: &BTreeMap<String, String>, shared_modules: &[String], full: bool) -> Analysis {
    let modules: Vec<Module> =
        files.iter().filter(|(k, _)| !k.ends_with(".d.ts")).filter_map(|(k, t)| Module::parse(k, t)).collect();
    let by_key = modules.iter().enumerate().map(|(i, m)| (m.key.clone(), i)).collect();
    let project = Project { files, modules, by_key };
    let mut analysis = Analysis::default();
    for (i, module) in project.modules.iter().enumerate() {
        let mut scan = Scan::new(&project, i);
        module.program().visit_with(&mut scan);
        let place = |pos: BytePos| module.lines.at(pos);
        for (pos, kind, name) in &scan.references {
            let (line, column) = place(*pos);
            analysis.references.push(ResourceReference {
                file: module.key.clone(),
                line,
                column,
                kind: kind.clone(),
                name: name.clone(),
            });
        }
        let mut imports: Vec<String> = Vec::new();
        for specifier in &scan.specifiers {
            if let Some(key) = resolve(&module.key, specifier, |k| files.contains_key(k))
                && !imports.contains(&key)
            {
                imports.push(key);
            }
        }
        analysis.imports.insert(module.key.clone(), imports);
        if !full {
            continue;
        }
        let shared = shared_modules.contains(&module.key);
        let top_level = if shared { module.top_level_state() } else { Vec::new() };
        let lints = scan.shared_pool.iter().chain(&top_level).chain(&scan.resource_lints).chain(&scan.debugger);
        for (pos, code, message) in lints {
            let (line, column) = place(*pos);
            analysis.diagnostics.push(Diagnostic {
                file: Some(module.key.clone()),
                line,
                column,
                code: *code,
                category: "warning".into(),
                message: message.clone(),
            });
        }
        for (pos, method, top_level) in &scan.console {
            let (line, column) = place(*pos);
            analysis.console.push(ConsoleCall {
                file: module.key.clone(),
                line,
                column,
                method: method.clone(),
                top_level: *top_level,
            });
        }
        analysis.exports.extend(project.exports_of(i));
    }
    analysis
}

/// A parsed project file and what it imports and exports, by name.
struct Module {
    key: String,
    parsed: deno_ast::ParsedSource,
    lines: Lines,
    /// The context of names declared nowhere in the file: globals (`console`, `globalThis`).
    unresolved: SyntaxContext,
    /// Each name an import declares, and what it imports.
    imports: HashMap<Id, Imported>,
    /// Each name the file exports, in order.
    exports: Vec<(String, Exported)>,
    /// `export * from "…"`, in order.
    stars: Vec<String>,
}

enum Imported {
    /// `import { name as local } from "spec"` (and a default import, as `default`).
    Named(String, String),
    /// `import * as local from "spec"`.
    Namespace(String),
}

enum Exported {
    /// Something the file declares or imports, by its name there.
    Local(Id),
    /// `export { name as … } from "spec"`.
    From(String, String),
    /// `export * as … from "spec"`.
    NamespaceFrom(String),
    /// `export default` of a declaration or an expression that isn't a name.
    Default,
}

impl Module {
    fn parse(key: &str, text: &str) -> Option<Self> {
        let specifier = deno_core::ModuleSpecifier::parse(&format!("file:///{}", key.trim_start_matches('/')))
            .or_else(|_| deno_core::ModuleSpecifier::parse("file:///unnamed.ts"))
            .ok()?;
        let media_type = match key.rsplit('.').next() {
            Some("js" | "mjs") => deno_ast::MediaType::JavaScript,
            _ => deno_ast::MediaType::TypeScript,
        };
        let parsed = deno_ast::parse_module(deno_ast::ParseParams {
            specifier,
            // deno_ast wants the byte order mark gone (as the loader does).
            text: text.strip_prefix('\u{feff}').unwrap_or(text).into(),
            media_type,
            capture_tokens: false,
            scope_analysis: true,
            maybe_syntax: None,
        })
        .ok()?;
        let lines = Lines::new(parsed.text().clone());
        let unresolved = parsed.unresolved_context();
        let mut module = Self {
            key: key.to_string(),
            parsed,
            lines,
            unresolved,
            imports: HashMap::new(),
            exports: Vec::new(),
            stars: Vec::new(),
        };
        module.read_imports_and_exports();
        Some(module)
    }

    fn program(&self) -> deno_ast::ProgramRef<'_> {
        self.parsed.program_ref()
    }

    fn body(&self) -> &[ast::ModuleItem] {
        match self.program() {
            deno_ast::ProgramRef::Module(m) => &m.body,
            deno_ast::ProgramRef::Script(_) => &[],
        }
    }

    fn read_imports_and_exports(&mut self) {
        let (mut imports, mut exports, mut stars) = (HashMap::new(), Vec::new(), Vec::new());
        for item in self.body() {
            let ast::ModuleItem::ModuleDecl(decl) = item else { continue };
            match decl {
                ast::ModuleDecl::Import(i) => {
                    let spec = text_of(&i.src);
                    for s in &i.specifiers {
                        let (local, imported) = match s {
                            ast::ImportSpecifier::Named(n) => {
                                let name =
                                    n.imported.as_ref().map(export_name).unwrap_or_else(|| n.local.sym.to_string());
                                (&n.local, Imported::Named(spec.clone(), name))
                            }
                            ast::ImportSpecifier::Default(d) => {
                                (&d.local, Imported::Named(spec.clone(), "default".into()))
                            }
                            ast::ImportSpecifier::Namespace(n) => (&n.local, Imported::Namespace(spec.clone())),
                        };
                        imports.insert(local.to_id(), imported);
                    }
                }
                ast::ModuleDecl::ExportDecl(e) => {
                    for id in declared(&e.decl) {
                        exports.push((id.0.to_string(), Exported::Local(id)));
                    }
                }
                ast::ModuleDecl::ExportNamed(n) if !n.type_only => {
                    let src = n.src.as_deref().map(text_of);
                    for s in &n.specifiers {
                        match s {
                            ast::ExportSpecifier::Named(s) if !s.is_type_only => {
                                let orig = export_name(&s.orig);
                                let name = s.exported.as_ref().map(export_name).unwrap_or_else(|| orig.clone());
                                let exported = match (&src, &s.orig) {
                                    (Some(src), _) => Exported::From(src.clone(), orig),
                                    (None, ast::ModuleExportName::Ident(i)) => Exported::Local(i.to_id()),
                                    (None, ast::ModuleExportName::Str(_)) => continue,
                                };
                                exports.push((name, exported));
                            }
                            ast::ExportSpecifier::Namespace(s) => {
                                if let Some(src) = &src {
                                    exports.push((export_name(&s.name), Exported::NamespaceFrom(src.clone())));
                                }
                            }
                            ast::ExportSpecifier::Default(d) => {
                                if let Some(src) = &src {
                                    exports.push((
                                        d.exported.sym.to_string(),
                                        Exported::From(src.clone(), "default".into()),
                                    ));
                                }
                            }
                            ast::ExportSpecifier::Named(_) => {}
                        }
                    }
                }
                ast::ModuleDecl::ExportDefaultDecl(_) => exports.push(("default".into(), Exported::Default)),
                ast::ModuleDecl::ExportDefaultExpr(e) => {
                    let exported = match unwrap(&e.expr) {
                        Expr::Ident(i) => Exported::Local(i.to_id()),
                        _ => Exported::Default,
                    };
                    exports.push(("default".into(), exported));
                }
                ast::ModuleDecl::ExportAll(a) if !a.type_only => stars.push(text_of(&a.src)),
                _ => {}
            }
        }
        (self.imports, self.exports, self.stars) = (imports, exports, stars);
    }

    /// Top-level `let`/`var` (`DX3`, for a shared module): where each statement starts.
    fn top_level_state(&self) -> Vec<(BytePos, u32, String)> {
        let mut found = Vec::new();
        for item in self.body() {
            let var = match item {
                ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Var(v))) => v,
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(ast::ExportDecl {
                    decl: ast::Decl::Var(v),
                    ..
                })) => v,
                _ => continue,
            };
            if var.kind != ast::VarDeclKind::Const {
                found.push((
                    item.span().lo,
                    90003,
                    "top-level `let`/`var` in a shared module: every interface importing this file shares one copy of it; use const and pure functions (see docs/shared-pool.md)".to_string(),
                ));
            }
        }
        found
    }
}

/// The names a declaration declares (values only: a type or interface isn't called).
fn declared(decl: &ast::Decl) -> Vec<Id> {
    match decl {
        ast::Decl::Fn(f) => vec![f.ident.to_id()],
        ast::Decl::Class(c) => vec![c.ident.to_id()],
        ast::Decl::TsEnum(e) => vec![e.id.to_id()],
        ast::Decl::Var(v) => {
            let mut ids = Vec::new();
            for d in &v.decls {
                pattern_names(&d.name, &mut |i| ids.push(i.to_id()));
            }
            ids
        }
        _ => Vec::new(),
    }
}

/// Each name a binding pattern declares (`a`, `{ a, b: [c] }`, `...rest`, `a = 1`).
fn pattern_names<'a>(pat: &'a ast::Pat, found: &mut dyn FnMut(&'a ast::Ident)) {
    match pat {
        ast::Pat::Ident(b) => found(&b.id),
        ast::Pat::Array(a) => a.elems.iter().flatten().for_each(|p| pattern_names(p, found)),
        ast::Pat::Rest(r) => pattern_names(&r.arg, found),
        ast::Pat::Assign(a) => pattern_names(&a.left, found),
        ast::Pat::Object(o) => {
            for prop in &o.props {
                match prop {
                    ast::ObjectPatProp::KeyValue(kv) => pattern_names(&kv.value, found),
                    ast::ObjectPatProp::Assign(a) => found(&a.key.id),
                    ast::ObjectPatProp::Rest(r) => pattern_names(&r.arg, found),
                }
            }
        }
        ast::Pat::Expr(_) | ast::Pat::Invalid(_) => {}
    }
}

fn text_of(s: &ast::Str) -> String {
    s.value.to_string_lossy().to_string()
}

fn export_name(n: &ast::ModuleExportName) -> String {
    match n {
        ast::ModuleExportName::Ident(i) => i.sym.to_string(),
        ast::ModuleExportName::Str(s) => text_of(s),
    }
}

/// See through `(x)`, `x as T`, `<T>x`, `x!`, `x satisfies T` and `x as const`.
fn unwrap(mut e: &Expr) -> &Expr {
    loop {
        e = match e {
            Expr::Paren(p) => &p.expr,
            Expr::TsAs(a) => &a.expr,
            Expr::TsTypeAssertion(a) => &a.expr,
            Expr::TsNonNull(n) => &n.expr,
            Expr::TsSatisfies(s) => &s.expr,
            Expr::TsConstAssertion(c) => &c.expr,
            _ => return e,
        };
    }
}

/// A string written out: `"x"`, `'x'` or `` `x` `` (a template without `${…}`).
fn literal(e: &Expr) -> Option<String> {
    match unwrap(e) {
        Expr::Lit(ast::Lit::Str(s)) => Some(text_of(s)),
        Expr::Tpl(t) if t.exprs.is_empty() => {
            t.quasis.first().and_then(|q| q.cooked.as_ref()).map(|c| c.to_string_lossy().to_string())
        }
        _ => None,
    }
}

/// The property a member expression names: `x.name`, or `x["name"]` written out.
fn property(prop: &ast::MemberProp) -> Option<String> {
    match prop {
        ast::MemberProp::Ident(i) => Some(i.sym.to_string()),
        ast::MemberProp::Computed(c) => literal(&c.expr),
        ast::MemberProp::PrivateName(_) => None,
    }
}

/// What a name or expression stands for, followed across files.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// The SDK's export of that name.
    Sdk(String),
    /// `import * as sdk from "@ductus/sdk"`.
    SdkNamespace,
    /// A project file's namespace (`import * as lib from "./lib.ts"`).
    Namespace(usize),
    /// Declared in that file under that name.
    Decl(usize, Id),
    /// That file's `export default`.
    Default(usize),
    Unknown,
}

/// How deep re-exports are followed (a cycle of re-exports stops here).
const MAX_DEPTH: u8 = 32;

/// Where an import specifier leads.
enum Source {
    Sdk,
    Module(usize),
}

struct Project<'f> {
    files: &'f BTreeMap<String, String>,
    modules: Vec<Module>,
    by_key: HashMap<String, usize>,
}

impl Project<'_> {
    fn source(&self, from: usize, spec: &str) -> Option<Source> {
        if spec == SDK {
            return Some(Source::Sdk);
        }
        let key = resolve(&self.modules[from].key, spec, |k| self.files.contains_key(k))?;
        self.by_key.get(&key).map(|&i| Source::Module(i))
    }

    /// What module `m` exports as `name`.
    fn export(&self, m: usize, name: &str, depth: u8) -> Target {
        if depth > MAX_DEPTH {
            return Target::Unknown;
        }
        let module = &self.modules[m];
        if let Some((_, exported)) = module.exports.iter().find(|(n, _)| n == name) {
            return match exported {
                Exported::Local(id) => self.binding(m, id, depth + 1),
                Exported::From(spec, orig) => match self.source(m, spec) {
                    Some(Source::Sdk) => Target::Sdk(orig.clone()),
                    Some(Source::Module(n)) => self.export(n, orig, depth + 1),
                    None => Target::Unknown,
                },
                Exported::NamespaceFrom(spec) => match self.source(m, spec) {
                    Some(Source::Sdk) => Target::SdkNamespace,
                    Some(Source::Module(n)) => Target::Namespace(n),
                    None => Target::Unknown,
                },
                Exported::Default => Target::Default(m),
            };
        }
        if name == "default" {
            return Target::Unknown; // `export *` doesn't pass a default on
        }
        // Project files first: the SDK's `export *` can't say which names it has.
        let mut sdk = false;
        for spec in &module.stars {
            match self.source(m, spec) {
                Some(Source::Module(n)) => {
                    let found = self.export(n, name, depth + 1);
                    if found != Target::Unknown {
                        return found;
                    }
                }
                Some(Source::Sdk) => sdk = true,
                None => {}
            }
        }
        if sdk { Target::Sdk(name.to_string()) } else { Target::Unknown }
    }

    /// What a name in module `m` stands for: an import followed, or a declaration there.
    fn binding(&self, m: usize, id: &Id, depth: u8) -> Target {
        match self.modules[m].imports.get(id) {
            Some(Imported::Named(spec, name)) => match self.source(m, spec) {
                Some(Source::Sdk) => Target::Sdk(name.clone()),
                Some(Source::Module(n)) => self.export(n, name, depth + 1),
                None => Target::Unknown,
            },
            Some(Imported::Namespace(spec)) => match self.source(m, spec) {
                Some(Source::Sdk) => Target::SdkNamespace,
                Some(Source::Module(n)) => Target::Namespace(n),
                None => Target::Unknown,
            },
            None => Target::Decl(m, id.clone()),
        }
    }

    /// What an expression in module `m` stands for, when it's a name or a member of a namespace.
    fn expression(&self, m: usize, e: &Expr) -> Target {
        match unwrap(e) {
            Expr::Ident(i) if i.ctxt != self.modules[m].unresolved => self.binding(m, &i.to_id(), 0),
            Expr::Member(member) => {
                let Some(name) = property(&member.prop) else { return Target::Unknown };
                match self.expression(m, &member.obj) {
                    Target::SdkNamespace => Target::Sdk(name),
                    Target::Namespace(n) => self.export(n, &name, 0),
                    _ => Target::Unknown,
                }
            }
            _ => Target::Unknown,
        }
    }

    /// The SDK function a callee is, through any import alias or namespace (`db`, `api`, …).
    fn sdk_function(&self, m: usize, callee: &Expr) -> Option<String> {
        match self.expression(m, callee) {
            Target::Sdk(name) => Some(name),
            _ => None,
        }
    }

    /// Every name module `m` exports: its own, then those `export *` passes on, each once.
    fn export_names(&self, m: usize, depth: u8, names: &mut Vec<String>) {
        if depth > MAX_DEPTH {
            return;
        }
        let module = &self.modules[m];
        for (name, _) in &module.exports {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        for spec in &module.stars {
            if let Some(Source::Module(n)) = self.source(m, spec) {
                let mut more = Vec::new();
                self.export_names(n, depth + 1, &mut more);
                for name in more.into_iter().filter(|n| n != "default") {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
    }

    /// What module `m` exports, each with where it's declared (the SDK's left out: no place in
    /// the project).
    fn exports_of(&self, m: usize) -> Vec<Export> {
        let mut names = Vec::new();
        self.export_names(m, 0, &mut names);
        let mut found = Vec::new();
        for name in names {
            let (n, declaration) = match self.export(m, &name, 0) {
                Target::Decl(n, id) => (n, self.declaration(n, &id)),
                Target::Default(n) => (n, self.default_declaration(n)),
                _ => continue,
            };
            let Some((pos, function)) = declaration else { continue };
            let module = &self.modules[n];
            let (line, column) = module.lines.at(pos);
            let (r#async, waits) = match function {
                Some(f) => (f.is_async(), self.waits(n, f)),
                None => (false, Vec::new()),
            };
            found.push(Export {
                module: self.modules[m].key.clone(),
                name,
                file: module.key.clone(),
                line,
                column,
                r#async,
                waits,
            });
        }
        found
    }

    /// Where module `n` declares `id` (its name's start), and the function it is, if it's one.
    fn declaration(&self, n: usize, id: &Id) -> Option<(BytePos, Option<FunctionRef<'_>>)> {
        for item in self.modules[n].body() {
            let decl = match item {
                ast::ModuleItem::Stmt(ast::Stmt::Decl(d)) => d,
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDecl(e)) => &e.decl,
                _ => continue,
            };
            match decl {
                ast::Decl::Fn(f) if f.ident.to_id() == *id => {
                    return Some((f.ident.span.lo, Some(FunctionRef::Function(&f.function))));
                }
                ast::Decl::Class(c) if c.ident.to_id() == *id => return Some((c.ident.span.lo, None)),
                ast::Decl::TsEnum(e) if e.id.to_id() == *id => return Some((e.id.span.lo, None)),
                ast::Decl::Var(v) => {
                    for d in &v.decls {
                        if let ast::Pat::Ident(b) = &d.name
                            && b.id.to_id() == *id
                        {
                            let function = d.init.as_deref().and_then(|e| FunctionRef::of(unwrap(e)));
                            return Some((b.id.span.lo, function));
                        }
                        let mut at = None;
                        pattern_names(&d.name, &mut |i| {
                            if i.to_id() == *id {
                                at = Some(i.span.lo);
                            }
                        });
                        if let Some(pos) = at {
                            return Some((pos, None));
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Module `n`'s `export default`: its name's start when it has one, else the statement's.
    fn default_declaration(&self, n: usize) -> Option<(BytePos, Option<FunctionRef<'_>>)> {
        for item in self.modules[n].body() {
            match item {
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDefaultDecl(d)) => {
                    return Some(match &d.decl {
                        ast::DefaultDecl::Fn(f) => (
                            f.ident.as_ref().map_or(d.span.lo, |i| i.span.lo),
                            Some(FunctionRef::Function(&f.function)),
                        ),
                        ast::DefaultDecl::Class(c) => (c.ident.as_ref().map_or(d.span.lo, |i| i.span.lo), None),
                        ast::DefaultDecl::TsInterfaceDecl(_) => return None,
                    });
                }
                ast::ModuleItem::ModuleDecl(ast::ModuleDecl::ExportDefaultExpr(e)) => {
                    return Some((e.span.lo, FunctionRef::of(unwrap(&e.expr))));
                }
                _ => {}
            }
        }
        None
    }

    /// What a function's body waits for: each `await`, and each call of the SDK's `db`, `api` or
    /// `fetch` (functions inside it included), in order.
    fn waits(&self, n: usize, function: FunctionRef<'_>) -> Vec<Wait> {
        let mut visitor = Waits { project: self, module: n, found: Vec::new() };
        match function {
            FunctionRef::Function(f) => {
                if let Some(body) = &f.body {
                    body.visit_with(&mut visitor);
                }
            }
            FunctionRef::Arrow(a) => a.body.visit_with(&mut visitor),
        }
        let lines = &self.modules[n].lines;
        visitor
            .found
            .into_iter()
            .map(|(pos, what)| {
                let (line, column) = lines.at(pos);
                Wait { line, column, what }
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
enum FunctionRef<'a> {
    Function(&'a ast::Function),
    Arrow(&'a ast::ArrowExpr),
}

impl<'a> FunctionRef<'a> {
    fn of(e: &'a Expr) -> Option<Self> {
        match e {
            Expr::Arrow(a) => Some(Self::Arrow(a)),
            Expr::Fn(f) => Some(Self::Function(&f.function)),
            _ => None,
        }
    }

    /// `async`, or a declared return type of `Promise<…>`/`PromiseLike<…>`.
    fn is_async(self) -> bool {
        let (is_async, returns) = match self {
            Self::Function(f) => (f.is_async, &f.return_type),
            Self::Arrow(a) => (a.is_async, &a.return_type),
        };
        is_async || returns.as_ref().is_some_and(|r| promise_type(&r.type_ann))
    }
}

fn promise_type(t: &ast::TsType) -> bool {
    let ast::TsType::TsTypeRef(r) = t else { return false };
    let last = match &r.type_name {
        ast::TsEntityName::Ident(i) => i.sym.as_str(),
        ast::TsEntityName::TsQualifiedName(q) => q.right.sym.as_str(),
    };
    matches!(last, "Promise" | "PromiseLike")
}

struct Waits<'p, 'f> {
    project: &'p Project<'f>,
    module: usize,
    found: Vec<(BytePos, String)>,
}

impl Waits<'_, '_> {
    fn call(&mut self, callee: &Expr, pos: BytePos) {
        if let Some(name) = self.project.sdk_function(self.module, callee)
            && matches!(name.as_str(), "db" | "api" | "fetch")
        {
            self.found.push((pos, format!("{name}()")));
        }
    }
}

impl Visit for Waits<'_, '_> {
    fn visit_await_expr(&mut self, n: &ast::AwaitExpr) {
        self.found.push((n.span.lo, "await".into()));
        n.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, n: &ast::CallExpr) {
        if let ast::Callee::Expr(callee) = &n.callee {
            self.call(callee, n.span.lo);
        }
        n.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, n: &ast::OptCall) {
        self.call(&n.callee, n.span.lo);
        n.visit_children_with(self);
    }
}

const GLOBAL_NAMES: [&str; 4] = ["globalThis", "window", "self", "global"];
const DOC: &str = "see docs/shared-pool.md";

/// One pass over a file: resource names, import specifiers, `console` calls and the lints.
struct Scan<'p, 'f> {
    project: &'p Project<'f>,
    module: usize,
    /// Inside a function (a `console` call there runs per call, not once at load).
    in_function: bool,
    references: Vec<(BytePos, String, String)>,
    specifiers: Vec<String>,
    console: Vec<(BytePos, String, bool)>,
    /// `DX1`, `DX2`.
    shared_pool: Vec<(BytePos, u32, String)>,
    /// `DX4`.
    resource_lints: Vec<(BytePos, u32, String)>,
    /// `DX5`.
    debugger: Vec<(BytePos, u32, String)>,
}

impl<'p, 'f> Scan<'p, 'f> {
    fn new(project: &'p Project<'f>, module: usize) -> Self {
        Self {
            project,
            module,
            in_function: false,
            references: Vec::new(),
            specifiers: Vec::new(),
            console: Vec::new(),
            shared_pool: Vec::new(),
            resource_lints: Vec::new(),
            debugger: Vec::new(),
        }
    }

    fn call(&mut self, callee: &Expr, args: &[ast::ExprOrSpread], pos: BytePos) {
        // `db("name")` / `api("name")`: the name, or a warning when it isn't written out.
        if let Some(function) = self.project.sdk_function(self.module, callee) {
            let what = match function.as_str() {
                "db" => Some("database"),
                "api" => Some("API"),
                _ => None,
            };
            if let Some(what) = what {
                match args.first().filter(|a| a.spread.is_none()).and_then(|a| literal(&a.expr)) {
                    Some(name) => self.references.push((pos, function, name)),
                    None => self.resource_lints.push((
                        pos,
                        90004,
                        format!(
                            "{function}() with a name that isn't written out: validate can't check that the {what} exists, and `ductus impact` can't see that this code uses it; write {function}(\"name\")"
                        ),
                    )),
                }
            }
        }
        // `Object.assign(globalThis, …)` and the like.
        if let Expr::Member(member) = callee
            && let Expr::Ident(object) = &*member.obj
            && object.sym == "Object"
            && let ast::MemberProp::Ident(f) = &member.prop
            && matches!(f.sym.as_str(), "assign" | "defineProperty" | "defineProperties" | "setPrototypeOf")
            && let Some(first) = args.first()
        {
            let target = unwrap(&first.expr);
            let problem = match target {
                Expr::Ident(i) if GLOBAL_NAMES.contains(&i.sym.as_str()) => {
                    Some((90001, format!("writes to the global scope ({}) via Object.{} ({DOC})", i.sym, f.sym)))
                }
                _ => shared_state_problem(target),
            };
            if let Some((code, message)) = problem {
                self.shared_pool.push((target.span().lo, code, message));
            }
        }
        // The global `console`'s methods (a local or imported `console` isn't it).
        if let Expr::Member(member) = unwrap(callee)
            && let ast::MemberProp::Ident(method) = &member.prop
            && let Expr::Ident(object) = unwrap(&member.obj)
            && object.sym == "console"
            && object.ctxt == self.project.modules[self.module].unresolved
        {
            self.console.push((pos, method.sym.to_string(), !self.in_function));
        }
    }

    fn inside_function(&mut self, visit: impl FnOnce(&mut Self)) {
        let was = std::mem::replace(&mut self.in_function, true);
        visit(self);
        self.in_function = was;
    }
}

/// Problem with an assignment's or `Object.assign`'s target, if any (`DX1`, `DX2`).
fn shared_state_problem(target: &Expr) -> Option<(u32, String)> {
    match unwrap(target) {
        Expr::Member(m) => member_problem(m),
        Expr::Ident(i) if GLOBAL_NAMES.contains(&i.sym.as_str()) => Some(global_write(&i.sym)),
        _ => None,
    }
}

fn member_problem(member: &ast::MemberExpr) -> Option<(u32, String)> {
    // The chain's root (`globalThis` in `globalThis.a.b`), and whether it passes a `.prototype`.
    let mut prototype = matches!(&member.prop, ast::MemberProp::Ident(p) if p.sym == "prototype");
    let mut e = unwrap(&member.obj);
    while let Expr::Member(m) = e {
        prototype |= matches!(&m.prop, ast::MemberProp::Ident(p) if p.sym == "prototype");
        e = unwrap(&m.obj);
    }
    if let Expr::Ident(root) = e
        && GLOBAL_NAMES.contains(&root.sym.as_str())
    {
        return Some(global_write(&root.sym));
    }
    prototype.then(|| {
        (
            90002,
            format!(
                "modifies a built-in prototype, which changes behavior for every interface in the shared pool ({DOC})"
            ),
        )
    })
}

fn global_write(root: &str) -> (u32, String) {
    (
        90001,
        format!(
            "writes to the global scope ({root}), which every interface in the shared pool can see; keep state in module scope instead ({DOC})"
        ),
    )
}

impl Visit for Scan<'_, '_> {
    fn visit_import_decl(&mut self, n: &ast::ImportDecl) {
        self.specifiers.push(text_of(&n.src));
    }

    fn visit_export_all(&mut self, n: &ast::ExportAll) {
        self.specifiers.push(text_of(&n.src));
    }

    fn visit_named_export(&mut self, n: &ast::NamedExport) {
        if let Some(src) = &n.src {
            self.specifiers.push(text_of(src));
        }
    }

    fn visit_call_expr(&mut self, n: &ast::CallExpr) {
        match &n.callee {
            ast::Callee::Expr(callee) => self.call(callee, &n.args, n.span.lo),
            ast::Callee::Import(_) => {
                if let Some(specifier) = n.args.first().and_then(|a| literal(&a.expr)) {
                    self.specifiers.push(specifier);
                }
            }
            ast::Callee::Super(_) => {}
        }
        n.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, n: &ast::OptCall) {
        self.call(&n.callee, &n.args, n.span.lo);
        n.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, n: &ast::AssignExpr) {
        // A bare name being assigned (`x = 1`) is fine; a member of a global, or a prototype, isn't.
        if let ast::AssignTarget::Simple(target) = &n.left {
            let problem = match target {
                ast::SimpleAssignTarget::Member(m) => member_problem(m),
                ast::SimpleAssignTarget::Paren(p) => shared_state_problem(&p.expr),
                ast::SimpleAssignTarget::TsAs(a) => shared_state_problem(&a.expr),
                ast::SimpleAssignTarget::TsSatisfies(s) => shared_state_problem(&s.expr),
                ast::SimpleAssignTarget::TsNonNull(x) => shared_state_problem(&x.expr),
                ast::SimpleAssignTarget::TsTypeAssertion(a) => shared_state_problem(&a.expr),
                _ => None,
            };
            if let Some((code, message)) = problem {
                self.shared_pool.push((target.span().lo, code, message));
            }
        }
        n.visit_children_with(self);
    }

    fn visit_debugger_stmt(&mut self, n: &ast::DebuggerStmt) {
        self.debugger.push((
            n.span.lo,
            90005,
            "a `debugger;` statement: it pauses here in every debugging session (`ductus test --inspect`); remove it when you're done, and set a breakpoint in the editor instead".into(),
        ));
    }

    fn visit_function(&mut self, n: &ast::Function) {
        self.inside_function(|s| n.visit_children_with(s));
    }

    fn visit_arrow_expr(&mut self, n: &ast::ArrowExpr) {
        self.inside_function(|s| n.visit_children_with(s));
    }

    fn visit_constructor(&mut self, n: &ast::Constructor) {
        self.inside_function(|s| n.visit_children_with(s));
    }

    fn visit_getter_prop(&mut self, n: &ast::GetterProp) {
        self.inside_function(|s| n.visit_children_with(s));
    }

    fn visit_setter_prop(&mut self, n: &ast::SetterProp) {
        self.inside_function(|s| n.visit_children_with(s));
    }

    fn visit_static_block(&mut self, n: &ast::StaticBlock) {
        self.inside_function(|s| n.visit_children_with(s));
    }
}

/// Where a relative import (or an absolute path) leads among the project's files, as TypeScript's
/// `Bundler` resolution finds it: the path as written when it names a TypeScript file, `.js`
/// meaning its `.ts`, and without an extension `.ts`, `.tsx`, `.d.ts`, then the folder's `index`.
/// None for a package (`@ductus/sdk`, anything from node_modules) or a file that isn't there.
fn resolve(from: &str, specifier: &str, exists: impl Fn(&str) -> bool) -> Option<String> {
    let relative = specifier == "." || specifier == ".." || specifier.starts_with("./") || specifier.starts_with("../");
    let joined = if relative {
        let dir = &from[..from.rfind('/')?];
        normalize(&format!("{dir}/{specifier}"))
    } else if specifier.starts_with('/') {
        normalize(specifier)
    } else {
        return None;
    };
    let candidates = if [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| joined.ends_with(e)) {
        vec![joined]
    } else if let Some(stem) = joined.strip_suffix(".js") {
        vec![format!("{stem}.ts"), format!("{stem}.tsx"), format!("{stem}.d.ts"), joined.clone()]
    } else if let Some(stem) = joined.strip_suffix(".mjs") {
        vec![format!("{stem}.mts"), format!("{stem}.d.mts"), joined.clone()]
    } else {
        [".ts", ".tsx", ".d.ts", "/index.ts", "/index.tsx", "/index.d.ts"]
            .iter()
            .map(|ending| format!("{joined}{ending}"))
            .collect()
    };
    candidates.into_iter().find(|c| exists(c))
}

/// `a/./b/../c` → `a/c` (a leading `/` or drive kept).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for (i, part) in path.split('/').enumerate() {
        match part {
            "." => {}
            "" if i > 0 => {}
            ".." if parts.len() > 1 => {
                parts.pop();
            }
            ".." => {}
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if path.starts_with('/') && !joined.starts_with('/') { format!("/{joined}") } else { joined }
}

/// Line and column (from 1, the column in UTF-16 units as editors and TypeScript count them) of a
/// position in a file's text.
struct Lines {
    text: std::sync::Arc<str>,
    /// Where each line starts (byte offsets).
    starts: Vec<usize>,
}

impl Lines {
    fn new(text: std::sync::Arc<str>) -> Self {
        let mut starts = vec![0];
        let bytes = text.as_bytes();
        for (i, c) in text.char_indices() {
            let next = match c {
                '\n' => i + 1,
                '\r' if bytes.get(i + 1) != Some(&b'\n') => i + 1,
                '\u{2028}' | '\u{2029}' => i + c.len_utf8(),
                _ => continue,
            };
            starts.push(next);
        }
        Self { text, starts }
    }

    fn at(&self, pos: BytePos) -> (u32, u32) {
        let start = deno_ast::StartSourcePos::START_SOURCE_POS.as_byte_pos().0;
        let offset = (pos.0.saturating_sub(start) as usize).min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let column = self.text.get(self.starts[line]..offset).map_or(0, |t| t.encode_utf16().count());
        (line as u32 + 1, column as u32 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_as_bundler_resolution_does() {
        let files = ["/p/a.ts", "/p/lib/b.ts", "/p/lib/index.ts", "/p/c.d.ts", "D:/w/x.ts"];
        let exists = |k: &str| files.contains(&k);
        assert_eq!(resolve("/p/i/t.ts", "../a.ts", exists).as_deref(), Some("/p/a.ts"));
        assert_eq!(resolve("/p/t.ts", "./lib/b", exists).as_deref(), Some("/p/lib/b.ts"));
        assert_eq!(resolve("/p/t.ts", "./lib/b.js", exists).as_deref(), Some("/p/lib/b.ts"));
        assert_eq!(resolve("/p/t.ts", "./lib", exists).as_deref(), Some("/p/lib/index.ts"));
        assert_eq!(resolve("/p/t.ts", "./c", exists).as_deref(), Some("/p/c.d.ts"));
        assert_eq!(resolve("D:/w/sub/t.ts", "../x.ts", exists).as_deref(), Some("D:/w/x.ts"));
        assert_eq!(resolve("/p/t.ts", "@ductus/sdk", exists), None);
        assert_eq!(resolve("/p/t.ts", "./missing.ts", exists), None);
    }

    #[test]
    fn places_count_lines_and_utf16_columns() {
        let lines = Lines::new("a\r\nbé😀x\ry\n".into());
        let at = |offset: u32| lines.at(BytePos(offset + 1));
        assert_eq!(at(0), (1, 1));
        assert_eq!(at(3), (2, 1));
        // `x` after `b`, `é` (one unit) and `😀` (two): column 5.
        assert_eq!(at(3 + 1 + 2 + 4), (2, 5));
        assert_eq!(at(3 + 1 + 2 + 4 + 2), (3, 1), "a lone \\r ends a line");
    }
}
