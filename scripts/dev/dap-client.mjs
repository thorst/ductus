// Drives VS Code's JavaScript debugger (js-debug's DAP server, scripts/dev/js-debug.sh) as VS Code
// does for the launch configuration `ductus types` writes: launch the program with
// `attachSimplePort`, answer js-debug's request for a child session, set one breakpoint, and when
// it's hit print the top frame, the locals and an expression's value, then go on to the end.
//   node dap-client.mjs DAP_SERVER FILE LINE EXPRESSION PROGRAM [ARGS...]
// Prints what happened, one line each; exits 1 if a step doesn't come within 30 s.
import { spawn } from "node:child_process";
import net from "node:net";

const [server, file, line, expression, program, ...args] = process.argv.slice(2);
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
  await breakpoint(c);
  await c.send("configurationDone");
  childReady(c);
};
await initialize(parent);
const launched = parent.send("launch", {
  type: "pwa-node",
  request: "launch",
  name: "Ductus: debug these tests",
  runtimeExecutable: program,
  runtimeArgs: args,
  attachSimplePort: 9229,
  cwd: process.cwd(),
  console: "internalConsole",
  outputCapture: "std",
});
await parent.event("initialized");
await breakpoint(parent);
await parent.send("configurationDone");
await launched;

const c = await child;
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
// Once is enough: the breakpoint goes before the run does.
await c.send("setBreakpoints", { source: { path: file }, breakpoints: [] });
await c.send("continue", { threadId: stopped.body.threadId });
await parent.event("terminated");
say("terminated");
dap.kill();
process.exit(0);
