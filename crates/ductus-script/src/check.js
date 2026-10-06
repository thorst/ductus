// Type-check interface code with the embedded TypeScript compiler (global `ts`).
// Everything is in memory: Rust passes the project files and the standard library typings.

globalThis.__ductus_check = (filesJson, libsJson) => {
  const files = JSON.parse(filesJson);
  const libs = JSON.parse(libsJson);
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
  return JSON.stringify(diagnostics);
};
