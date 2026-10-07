// Type-check interface code with the embedded TypeScript compiler (global `ts`).
// Everything is in memory: Rust passes the project files and the standard library typings.

// Shared-pool safety lints (see docs/shared-pool.md). Interfaces in the shared pool run in one
// V8 isolate: module scope is private, but these reach state other interfaces can see.
const GLOBAL_NAMES = new Set(["globalThis", "window", "self", "global"]);
const DOC = "see docs/shared-pool.md";

/** See through `(x)`, `x as T`, `<T>x`, `x!` and `x satisfies T`. */
function unwrap(node) {
  while (
    ts.isParenthesizedExpression(node) || ts.isAsExpression(node) || ts.isTypeAssertionExpression(node) ||
    ts.isNonNullExpression(node) || ts.isSatisfiesExpression(node)
  ) {
    node = node.expression;
  }
  return node;
}

function rootIdentifier(node) {
  node = unwrap(node);
  while (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) node = unwrap(node.expression);
  return ts.isIdentifier(node) ? node.text : undefined;
}

function touchesPrototype(node) {
  node = unwrap(node);
  while (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) {
    if (ts.isPropertyAccessExpression(node) && node.name.text === "prototype") return true;
    node = unwrap(node.expression);
  }
  return false;
}

/** Problem with an assignment target or an Object.assign/defineProperty target, if any. */
function sharedStateProblem(target) {
  target = unwrap(target);
  if (!ts.isPropertyAccessExpression(target) && !ts.isElementAccessExpression(target) && !ts.isIdentifier(target)) {
    return undefined;
  }
  const root = rootIdentifier(target);
  if (root && GLOBAL_NAMES.has(root)) {
    return [90001, `writes to the global scope (${root}), which every interface in the shared pool can see; keep state in module scope instead (${DOC})`];
  }
  if (touchesPrototype(target)) {
    return [90002, `modifies a built-in prototype, which changes behavior for every interface in the shared pool (${DOC})`];
  }
  return undefined;
}

function lintSharedPool(sourceFile, isSharedModule) {
  const found = [];
  const report = (node, [code, message]) => found.push({ file: sourceFile, start: node.getStart(sourceFile), code, message });

  const visit = (node) => {
    if (
      ts.isBinaryExpression(node) &&
      node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
      node.operatorToken.kind <= ts.SyntaxKind.LastAssignment &&
      node.left !== undefined
    ) {
      const left = node.left;
      // A bare global name being assigned (e.g. `globalThis = x`) or a member of one.
      const problem = ts.isIdentifier(left) ? undefined : sharedStateProblem(left);
      if (problem) report(left, problem);
    }
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
      const callee = node.expression;
      const fn = callee.name.text;
      if (
        ts.isIdentifier(callee.expression) && callee.expression.text === "Object" &&
        ["assign", "defineProperty", "defineProperties", "setPrototypeOf"].includes(fn) &&
        node.arguments.length > 0
      ) {
        const target = unwrap(node.arguments[0]);
        const problem = ts.isIdentifier(target) && GLOBAL_NAMES.has(target.text)
          ? [90001, `writes to the global scope (${target.text}) via Object.${fn} (${DOC})`]
          : sharedStateProblem(target);
        if (problem) report(target, problem);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);

  if (isSharedModule) {
    for (const statement of sourceFile.statements) {
      if (ts.isVariableStatement(statement) && !(statement.declarationList.flags & ts.NodeFlags.Const)) {
        report(statement, [
          90003,
          `top-level \`let\`/\`var\` in a shared module: every interface importing this file shares one copy of it; use const and pure functions (${DOC})`,
        ]);
      }
    }
  }
  return found;
}

// SDK functions whose first argument names a resource in resources.yaml.
const RESOURCE_FUNCTIONS = { db: "database", api: "API" };

/** `db` or `api` if this call is to the SDK's function (through any import alias), else undefined. */
function sdkResourceFunction(checker, call) {
  let symbol = checker.getSymbolAtLocation(unwrap(call.expression));
  if (symbol && symbol.flags & ts.SymbolFlags.Alias) symbol = checker.getAliasedSymbol(symbol);
  const decl = symbol?.declarations?.[0];
  if (!decl || !(symbol.name in RESOURCE_FUNCTIONS)) return undefined;
  let parent = decl.parent;
  while (parent && !ts.isModuleDeclaration(parent)) parent = parent.parent;
  return parent && ts.isStringLiteral(parent.name) && parent.name.text === "@ductus/sdk" ? symbol.name : undefined;
}

/**
 * Resources named in code (`db("patient_db")`, `api("empi")`), so validate can check they exist
 * and `ductus impact` knows who uses them. A name that isn't written out can't be known before
 * the code runs: that gets a warning instead.
 */
function findResources(checker, sourceFile) {
  const references = [];
  const lints = [];
  const visit = (node) => {
    if (ts.isCallExpression(node)) {
      const fn = sdkResourceFunction(checker, node);
      if (fn) {
        const arg = node.arguments[0] && unwrap(node.arguments[0]);
        const start = node.getStart(sourceFile);
        if (arg && (ts.isStringLiteral(arg) || ts.isNoSubstitutionTemplateLiteral(arg))) {
          references.push({ start, kind: fn, name: arg.text });
        } else {
          lints.push({
            start,
            code: 90004,
            message: `${fn}() with a name that isn't written out: validate can't check that the ${RESOURCE_FUNCTIONS[fn]} exists, and \`ductus impact\` can't see that this code uses it; write ${fn}("name")`,
          });
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  return { references, lints };
}

/** The project files a file imports (static and dynamic imports, re-exports). */
function localImports(sourceFile, options, host) {
  const specifiers = [];
  const visit = (node) => {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      specifiers.push(node.moduleSpecifier.text);
    } else if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword && node.arguments[0] && ts.isStringLiteral(node.arguments[0])) {
      specifiers.push(node.arguments[0].text);
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  const out = new Set();
  for (const s of specifiers) {
    const resolved = ts.resolveModuleName(s, sourceFile.fileName, options, host).resolvedModule;
    if (resolved && !resolved.isExternalLibraryImport && host.fileExists(resolved.resolvedFileName)) out.add(resolved.resolvedFileName);
  }
  return [...out];
}

globalThis.__ductus_check = (filesJson, libsJson, sharedJson, wantDiagnostics) => {
  const files = JSON.parse(filesJson);
  const libs = JSON.parse(libsJson);
  const shared = new Set(JSON.parse(sharedJson ?? "[]"));
  const LIB_DIR = "/__ductus_lib__/";

  const read = (name) => {
    if (name.startsWith(LIB_DIR)) return libs[name.slice(LIB_DIR.length)];
    return files[name];
  };

  const options = {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    lib: ["lib.es2023.d.ts"],
    strict: true,
    noEmit: true,
    allowImportingTsExtensions: true,
    skipLibCheck: true,
    types: [],
  };

  const host = {
    getSourceFile: (name, languageVersion) => {
      const text = read(name);
      return text === undefined ? undefined : ts.createSourceFile(name, text, languageVersion, true);
    },
    getDefaultLibFileName: () => LIB_DIR + "lib.es2023.d.ts",
    getDefaultLibLocation: () => LIB_DIR,
    writeFile: () => {},
    getCurrentDirectory: () => "/",
    getCanonicalFileName: (f) => f,
    useCaseSensitiveFileNames: () => true,
    getNewLine: () => "\n",
    fileExists: (f) => read(f) !== undefined,
    readFile: (f) => read(f),
    directoryExists: () => true,
    getDirectories: () => [],
  };

  const program = ts.createProgram(Object.keys(files), options, host);
  const checker = program.getTypeChecker();
  const lints = [];
  const references = [];
  const imports = {};
  for (const sourceFile of program.getSourceFiles()) {
    if (sourceFile.isDeclarationFile || sourceFile.fileName.startsWith(LIB_DIR)) continue;
    const at = (start) => {
      const pos = sourceFile.getLineAndCharacterOfPosition(start);
      return { file: sourceFile.fileName, line: pos.line + 1, column: pos.character + 1 };
    };
    const found = findResources(checker, sourceFile);
    for (const r of found.references) references.push({ ...at(r.start), kind: r.kind, name: r.name });
    imports[sourceFile.fileName] = localImports(sourceFile, options, host);
    if (!wantDiagnostics) continue;
    for (const f of lintSharedPool(sourceFile, shared.has(sourceFile.fileName)).concat(found.lints)) {
      lints.push({ ...at(f.start), code: f.code, category: "warning", message: f.message });
    }
  }
  if (!wantDiagnostics) return JSON.stringify({ diagnostics: [], references, imports });
  const diagnostics = ts.getPreEmitDiagnostics(program).map((d) => {
    let line = 0;
    let column = 0;
    if (d.file && d.start !== undefined) {
      const pos = d.file.getLineAndCharacterOfPosition(d.start);
      line = pos.line + 1;
      column = pos.character + 1;
    }
    return {
      file: d.file ? d.file.fileName : null,
      line,
      column,
      code: d.code,
      category: ts.DiagnosticCategory[d.category].toLowerCase(),
      message: ts.flattenDiagnosticMessageText(d.messageText, "\n"),
    };
  });
  return JSON.stringify({ diagnostics: diagnostics.concat(lints), references, imports });
};
