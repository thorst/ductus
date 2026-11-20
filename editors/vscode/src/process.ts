import { spawn } from "node:child_process";
import { LineBuffer } from "./lines";

/** How a command ended. */
export interface Ended {
  /** The exit code; null when it was killed (`signal`) or never started (`error`). */
  code: number | null;
  signal: NodeJS.Signals | null;
  /** Everything it wrote to stderr. */
  stderr: string;
  /** It couldn't be started (not found, not executable). */
  error?: Error;
  ms: number;
}

export interface Listeners {
  /** Each line of stdout as it's written. */
  line?(text: string): void;
  /** Each chunk of stderr as it's written. */
  stderr?(text: string): void;
}

/** Something that can ask for a stop (VS Code's CancellationToken is one). */
export interface Stoppable {
  isCancellationRequested: boolean;
  onCancellationRequested(listener: () => void): { dispose(): void };
}

/**
 * Run `command` in `cwd`, handing stdout over line by line. Never rejects: a command that can't be
 * started ends with `error`. A stop request kills it. `env`: added to the extension's own
 * environment (secrets go here, never in `args`, which other users of the machine can list).
 */
export function run(
  command: string,
  args: string[],
  cwd: string,
  listeners: Listeners = {},
  stop?: Stoppable,
  env?: Record<string, string>,
): Promise<Ended> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      env: env ? { ...process.env, ...env } : undefined,
    });
    const lines = new LineBuffer();
    let stderr = "";
    let error: Error | undefined;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => lines.push(chunk).forEach((l) => listeners.line?.(l)));
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
      listeners.stderr?.(chunk);
    });
    child.on("error", (e) => (error = e));
    const stopping = stop?.onCancellationRequested(() => child.kill());
    if (stop?.isCancellationRequested) child.kill();
    child.on("close", (code, signal) => {
      lines.end().forEach((l) => listeners.line?.(l));
      stopping?.dispose();
      resolve({ code, signal, stderr, error, ms: Date.now() - started });
    });
  });
}

/** Run a command and keep its whole stdout (for one JSON document). */
export async function output(
  command: string,
  args: string[],
  cwd: string,
  env?: Record<string, string>,
): Promise<Ended & { stdout: string }> {
  const out: string[] = [];
  const ended = await run(command, args, cwd, { line: (l) => out.push(l) }, undefined, env);
  return { ...ended, stdout: out.join("\n") };
}
