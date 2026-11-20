/**
 * The Ductus extension for VS Code, as VS Code knows it: `<publisher>.<name>` from
 * editors/vscode/package.json (vscode.test.ts checks they agree; the publisher changes once it's
 * published to the Marketplace).
 */
export const EXTENSION = "ductus.ductus";

/**
 * The link that opens a stored message in VS Code under the debugger: the extension runs
 * `ductus try <interface> --message <id> --from <engine> --inspect` in the developer's copy of
 * the project (nothing is sent; the read is in this engine's audit log). `engine` is this engine's
 * URL as the browser reached it (its origin: the API is under it).
 */
export function debugInVsCode(engine: string, iface: string, id: number): string {
  const query = new URLSearchParams({ engine: engine.replace(/\/+$/, ""), interface: iface, message: String(id) });
  return `vscode://${EXTENSION}/debug-message?${query}`;
}
