// Bridge from @ductus/sdk to the engine's native operations.
const core = Deno.core;
const ops = core.ops;

globalThis.__ductus_ops = Object.freeze({
  dbQuery: (resource, sql, params) => ops.op_ductus_db_query(resource, sql, params),
  dbExecute: (resource, sql, params) => ops.op_ductus_db_execute(resource, sql, params),
  fetch: (request) => ops.op_ductus_fetch(request),
  variable: (name) => ops.op_ductus_variable(name),
  secret: (name) => ops.op_ductus_secret(name),
});

// `console` for interface code. deno_core's own writes every line to the process's stdout as it
// comes: unlabelled lines in the engine's journal, nothing at all under the Windows service, a
// failed message when stdout is a closed pipe, and lines among `--json` and `--emit` output. Ours
// hands each line to the engine (`Console` in ops.rs): its log, with the level and the place it
// was written from, or a test's output under `ductus test`. Levels: 0 debug, 1 info, 2 warn.
const DEBUG = 0, INFO = 1, WARN = 2;

/** A value as a log line shows it: text as is, objects as one line of JSON. */
function show(value) {
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.stack ?? String(value);
  if (typeof value === "function") return `[Function ${value.name || "(anonymous)"}]`;
  if (typeof value === "bigint") return `${value}n`;
  if (value === null || typeof value !== "object") return String(value);
  if (value instanceof Map) return `Map ${show(Object.fromEntries(value))}`;
  if (value instanceof Set) return `Set ${show([...value])}`;
  // Objects that say what they are (an HL7 message, a Date) say it; plain ones and arrays are JSON.
  if (!Array.isArray(value) && typeof value.toString === "function" && value.toString !== Object.prototype.toString) {
    return String(value);
  }
  try {
    return JSON.stringify(value, (_, v) => (typeof v === "bigint" ? `${v}n` : v)) ?? String(value);
  } catch {
    return String(value); // circular
  }
}

function write(level, args) {
  // 0: nobody reads this level (the log's filter), so nothing is formatted; 2: the log wants the
  // place too (a stack written costs ~30 us, so only then). This file must stay ASCII (ascii_str_include).
  const wants = ops.op_ductus_console_wants(level);
  if (wants === 0) return;
  let stack = "";
  if (wants === 2) {
    const limit = Error.stackTraceLimit;
    Error.stackTraceLimit = 4; // write, the console method, the caller (an SDK frame at most between)
    stack = new Error().stack ?? "";
    Error.stackTraceLimit = limit;
  }
  ops.op_ductus_console(level, args.map(show).join(" "), stack);
}

const ductusConsole = {
  log: (...args) => write(INFO, args),
  info: (...args) => write(INFO, args),
  debug: (...args) => write(DEBUG, args),
  trace: (...args) => write(DEBUG, args),
  warn: (...args) => write(WARN, args),
  error: (...args) => write(WARN, args),
  dir: (value) => write(INFO, [value]),
  table: (value) => write(INFO, [value]),
  assert: (condition, ...args) => {
    if (!condition) write(WARN, ["Assertion failed", ...args]);
  },
};
// V8's console methods are wrapped around ours, so a debugger attached to the isolate sees every
// line too; the ones we don't define (group, time, count...) reach only a debugger.
core.wrapConsole(ductusConsole, core.v8Console);
globalThis.console = ductusConsole;
globalThis.__ductus_console_take = () => ops.op_ductus_console_take();
