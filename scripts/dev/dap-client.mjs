// Drives VS Code's JavaScript debugger (js-debug's DAP server, scripts/dev/js-debug.sh) as VS Code
// does for the launch configuration `ductus types` writes: launch the program with
// `attachSimplePort`, answer js-debug's request for a child session, set one breakpoint, and when
// it's hit print the top frame, the locals and an expression's value, then go on to the end.
//   node dap-client.mjs DAP_SERVER FILE LINE EXPRESSION PROGRAM [ARGS...]
// With DAP_CONFIG (a configuration's name), the configuration is read from ./.vscode/launch.json
// as VS Code reads it instead of ARGS: `${file}`, `${fileDirname}` and `${workspaceFolder}`
// filled in for FILE open in the editor, `ductus` found as PROGRAM (VS Code finds it on the PATH).
// For a program that doesn't end by itself (a running engine): DAP_TRIGGER, a shell command run
// once the breakpoint is set (a message to the interface); DAP_WHILE_PAUSED, one run while stopped
// at it (its output printed); DAP_STOP, a shell command waited for after continuing (until the
// message is through), then the session stopped as VS Code's stop button does.
// Prints what happened, one line each; exits 1 if a step doesn't come within 30 s.
import { execSync, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import net from "node:net";
import path from "node:path";

const [server, file, line, expression, program, ...args] = process.argv.slice(2);
const { DAP_CONFIG, DAP_TRIGGER, DAP_WHILE_PAUSED, DAP_STOP } = process.env;
// The commands are bash's; on Windows (checked by hand there) cmd's.
const shell = process.platform === "win32" ? true : "/bin/bash";
const PORT = 4711 + (process.pid % 1000);
const say = (...a) => console.log(...a);
const fail = (why) => {
  say(`FAIL: ${why}`);
  process.exit(1);
};

const dap = spawn(process.execPath, [server, String(PORT), "127.0.0.1"], { stdio: ["ignore", "pipe", "inherit"] });
await new Promise((ok) => dap.stdout.on("data", (d) => String(d).includes("Debug server listening") && ok()));

/** One DAP connection: requests answered by promises, events waited for, reverse requests handled. */
function connect(name) {
  const socket = net.connect(PORT, "127.0.0.1");
  let seq = 0;
  let buffer = Buffer.alloc(0);
  const answers = new Map();
  const waiting = [];
  const c = {
    events: [],
    onRequest: {},
    write(message) {
      const body = JSON.stringify({ seq: ++seq, ...message });
      socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
      return seq;
    },
    send(command, args = {}) {
      const s = c.write({ type: "request", command, arguments: args });
      return new Promise((ok, no) => answers.set(s, (m) => (m.success ? ok(m.body ?? {}) : no(new Error(`${command}: ${m.message}`)))));
    },
    respond(request, body = {}) {
      c.write({ type: "response", request_seq: request.seq, command: request.command, success: true, body });
    },
    event(event) {
      const seen = c.events.find((e) => e.event === event && !e.taken);
      if (seen) return Promise.resolve(Object.assign(seen, { taken: true }));
      return new Promise((ok) => {
        const timer = setTimeout(() => fail(`${name}: no ${event} in 30 s`), 30000);
        waiting.push({ event, ok: (e) => (clearTimeout(timer), ok(Object.assign(e, { taken: true }))) });
      });
    },
  };
  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const head = buffer.indexOf("\r\n\r\n");
      if (head < 0) return;
      const length = Number(/Content-Length: (\d+)/.exec(buffer.subarray(0, head).toString())[1]);
      if (buffer.length < head + 4 + length) return;
      const m = JSON.parse(buffer.subarray(head + 4, head + 4 + length).toString());
      buffer = buffer.subarray(head + 4 + length);
      if (m.type === "response") answers.get(m.request_seq)?.(m);
      else if (m.type === "request") (c.onRequest[m.command] ?? ((r) => c.respond(r)))(m);
      else if (m.type === "event") {
        if (m.event === "output" && m.body.category !== "telemetry") {
          for (const l of m.body.output.split("\n").filter(Boolean)) say(`output: ${l}`);
        }
        c.events.push(m);
        const i = waiting.findIndex((w) => w.event === m.event);
        if (i >= 0) waiting.splice(i, 1)[0].ok(m);
      }
    }
  });
  return new Promise((ok) => socket.on("connect", () => ok(c)));
}

const initialize = (c) =>
  c.send("initialize", { clientID: "ductus-demo", adapterID: "pwa-node", linesStartAt1: true, columnsStartAt1: true, pathFormat: "path", supportsStartDebuggingRequest: true });
const breakpoint = (c) => c.send("setBreakpoints", { source: { path: file }, breakpoints: [{ line: Number(line) }] });

// js-debug runs the program in its parent session and asks for a child session (startDebugging)
// to debug what it attached to: breakpoints and pauses are the child's.
let childReady;
const child = new Promise((ok) => (childReady = ok));
const parent = await connect("parent");
parent.onRequest.startDebugging = async (request) => {
  parent.respond(request);
  const c = await connect("child");
  await initialize(c);
  c.send("launch", request.arguments.configuration);
  await c.event("initialized");
  const set = await breakpoint(c);
  await c.send("configurationDone");
  // In code that's already loaded (a running engine) the breakpoint is placed once js-debug has
  // read the script's source map: wait for that before sending anything to it.
  if (DAP_TRIGGER && !set.breakpoints?.[0]?.verified) {
    while (!(await c.event("breakpoint")).body.breakpoint.verified);
  }
  childReady(c);
};

/** The configuration named DAP_CONFIG in .vscode/launch.json, as VS Code hands it to js-debug. */
function fromLaunchJson() {
  const launch = JSON.parse(readFileSync(".vscode/launch.json", "utf8"));
  const found = launch.configurations.find((c) => c.name === DAP_CONFIG);
  if (!found) fail(`no configuration "${DAP_CONFIG}" in .vscode/launch.json`);
  const vars = { file, fileDirname: path.dirname(file), workspaceFolder: process.cwd() };
  const fill = (s) => s.replace(/\$\{(\w+)\}/g, (_, v) => vars[v] ?? fail(`unknown \${${v}}`));
  say(`configuration: ${found.name}: ${found.runtimeExecutable} ${found.runtimeArgs.join(" ")}`);
  // VS Code merges the platform's own block (`windows`, `linux`, `osx`) into the configuration.
  const platform = { win32: "windows", linux: "linux", darwin: "osx" }[process.platform];
  const { windows, linux, osx, ...common } = found;
  return {
    ...common,
    ...{ windows, linux, osx }[platform],
    type: "pwa-node", // what VS Code runs "node" configurations with
    runtimeExecutable: found.runtimeExecutable === "ductus" ? program : found.runtimeExecutable,
    runtimeArgs: found.runtimeArgs.map(fill),
    cwd: process.cwd(),
  };
}

await initialize(parent);
const launched = parent.send(
  "launch",
  DAP_CONFIG
    ? fromLaunchJson()
    : {
        type: "pwa-node",
        request: "launch",
        name: "Ductus: debug these tests",
        runtimeExecutable: program,
        runtimeArgs: args,
        attachSimplePort: 9229,
        cwd: process.cwd(),
        console: "internalConsole",
        outputCapture: "std",
      },
);
await parent.event("initialized");
await breakpoint(parent);
await parent.send("configurationDone");
await launched;

const c = await child;
if (DAP_TRIGGER) {
  const trigger = spawn(DAP_TRIGGER, { shell, stdio: ["ignore", "pipe", "pipe"] });
  trigger.stdout.on("data", (d) => String(d).split("\n").filter(Boolean).forEach((l) => say(`trigger: ${l}`)));
  trigger.stderr.on("data", (d) => String(d).split("\n").filter(Boolean).forEach((l) => say(`trigger: ${l}`)));
}
const stopped = await c.event("stopped");
say(`stopped: ${stopped.body.reason}`);
const { stackFrames } = await c.send("stackTrace", { threadId: stopped.body.threadId });
const top = stackFrames[0];
say(`at: ${top.name} ${top.source?.path}:${top.line}`);
const { scopes } = await c.send("scopes", { frameId: top.id });
const local = scopes.find((s) => s.name.startsWith("Local"));
const { variables } = await c.send("variables", { variablesReference: local.variablesReference });
say(`locals: ${variables.map((v) => `${v.name}=${v.value}`).join(", ")}`);
const evaluated = await c.send("evaluate", { expression, frameId: top.id, context: "watch" });
say(`${expression} = ${evaluated.result}`);
if (DAP_WHILE_PAUSED) {
  const out = execSync(DAP_WHILE_PAUSED, { shell, encoding: "utf8" });
  out.split("\n").filter(Boolean).forEach((l) => say(`while paused: ${l}`));
}
// Once is enough: the breakpoint goes before the run does.
await c.send("setBreakpoints", { source: { path: file }, breakpoints: [] });
await c.send("continue", { threadId: stopped.body.threadId });
if (DAP_STOP) {
  execSync(DAP_STOP, { shell });
  say("stopping the session");
  await parent.send("terminate");
}
// A program that ends by itself: the parent session says so once its output is all through.
// Stopped from here: the debugged session says so, the parent not always.
await (DAP_STOP ? Promise.race([parent.event("terminated"), c.event("terminated")]) : parent.event("terminated"));
say("terminated");
dap.kill();
process.exit(0);
