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

globalThis.__ductus_check = (filesJson, libsJson, sharedJson) => {
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
  const lints = [];
  for (const sourceFile of program.getSourceFiles()) {
    if (sourceFile.isDeclarationFile || sourceFile.fileName.startsWith(LIB_DIR)) continue;
    for (const f of lintSharedPool(sourceFile, shared.has(sourceFile.fileName))) {
      const pos = sourceFile.getLineAndCharacterOfPosition(f.start);
      lints.push({
        file: sourceFile.fileName,
        line: pos.line + 1,
        column: pos.character + 1,
        code: f.code,
        category: "warning",
        message: f.message,
      });
    }
  }
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
  return JSON.stringify(diagnostics.concat(lints));
};
