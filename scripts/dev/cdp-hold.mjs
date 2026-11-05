// A debugger that attaches to the first target on PORT, says "attached", and once the code pauses
// (a breakpoint or `debugger;`) says "paused" and keeps it paused until this process is stopped:
// what a developer looking at a breakpoint does to the engine, for scripts/bench-inspect.sh.
//   node cdp-hold.mjs PORT
// Uses Node's own WebSocket (Node 22 and later).
const port = process.argv[2] ?? "9229";
const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const socket = new WebSocket(targets[0].webSocketDebuggerUrl);
let id = 0;
const send = (method) => socket.send(JSON.stringify({ id: ++id, method, params: {} }));
socket.onopen = () => {
  send("Runtime.enable");
  send("Debugger.enable");
};
socket.onmessage = ({ data }) => {
  const m = JSON.parse(data);
  if (m.id === 2) console.log("attached");
  if (m.method === "Debugger.paused") console.log("paused");
};
socket.onclose = () => process.exit(0);
