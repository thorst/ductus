import * as vscode from "vscode";
import {
  type EngineInfo,
  type NamedEngine,
  type Need,
  type Session,
  lacks,
  parseEngineInfo,
  parseEngines,
  parseSession,
} from "./lines";
import { engines as enginesSetting } from "./settings";

/** A call to an engine answers in seconds; a list of releases is small. */
const CALL_WITHIN_MS = 30_000;

/** An engine and the token to call it with (your `ductus dev`'s from `.ductus/dev-token`). */
export interface Reach {
  engine: NamedEngine;
  token: string;
}

/** A token checked on its engine, ready to use. */
export interface Signed extends Reach {
  session: Session;
}

/** How the extension asks for a token (a prompt; the extension's tests answer it themselves). */
export type Ask = (engine: NamedEngine, need: Need, why?: string) => Promise<string | undefined>;

/**
 * The engines named in `ductus.engines`, and a token for each kept in VS Code's SecretStorage (the
 * operating system's keychain), by the engine's URL: asked for the first time it's needed,
 * checked with `GET /session` (and that it may do what it's for) before it's kept, and asked again
 * when the engine no longer knows it. **Ductus: Forget an engine's token** removes one.
 */
export class Engines implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private said = "";
  /** Replaced by the extension's tests to answer the prompt. */
  ask: Ask = prompt;

  constructor(
    private readonly secrets: vscode.SecretStorage,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(vscode.commands.registerCommand("ductus.forgetToken", () => this.forgetCommand()));
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  /** The engines named in the setting (what's wrong with any other said once in the output). */
  list(): NamedEngine[] {
    const { engines, problems } = parseEngines(enginesSetting());
    const said = problems.join("\n");
    if (said && said !== this.said) this.log.warn(said);
    this.said = said;
    return engines;
  }

  /**
   * A token for `engine` that may do `need`: the one kept, else asked for. Each is checked with
   * `GET /session` first. Undefined when none was given (cancelled), or the engine can't be reached
   * (said in a notification).
   */
  async signIn(engine: NamedEngine, need: Need): Promise<Signed | undefined> {
    let token = await this.secrets.get(key(engine));
    let why: string | undefined;
    for (;;) {
      if (!token) {
        token = (await this.ask(engine, need, why))?.trim();
        if (!token) return undefined;
      }
      const answer = await call(engine, "/session", token);
      if ("unreachable" in answer) {
        void vscode.window.showErrorMessage(`Ductus: ${answer.unreachable}`);
        return undefined;
      }
      const session = answer.status === 200 ? parseSession(answer.body) : undefined;
      if (!session) {
        // Not known there (revoked, expired, or for another engine): forget it and ask again.
        await this.secrets.delete(key(engine));
        why =
          answer.status === 401
            ? `${engine.name} doesn't know that token (revoked, or made on another engine)`
            : `${engine.name} answered ${answer.status} to it`;
        this.log.warn(`${engine.name} (${engine.url}): ${why}`);
        token = undefined;
        continue;
      }
      const missing = lacks(session, need);
      if (missing) {
        await this.secrets.delete(key(engine));
        why = missing;
        this.log.warn(`${engine.name} (${engine.url}): ${missing}`);
        token = undefined;
        continue;
      }
      await this.secrets.store(key(engine), token);
      return { engine, token, session };
    }
  }

  /** What `GET /engine` says of an engine (its environment, release mode), with a signed token. */
  async info(signed: Signed): Promise<EngineInfo | { why: string }> {
    const answer = await call(signed.engine, "/engine", signed.token);
    if ("unreachable" in answer) return { why: answer.unreachable };
    const info = answer.status === 200 ? parseEngineInfo(answer.body) : undefined;
    return info ?? { why: `${signed.engine.name} answered ${answer.status} to GET /engine` };
  }

  /** `GET path` on an engine with its token: its body, or why there's none. */
  async get(signed: Reach, path: string): Promise<{ body: string } | { why: string }> {
    const answer = await call(signed.engine, path, signed.token);
    if ("unreachable" in answer) return { why: answer.unreachable };
    if (answer.status !== 200)
      return { why: `${signed.engine.name} answered ${answer.status}: ${errorText(answer.body)}` };
    return { body: answer.body };
  }

  /** Forget the token kept for an engine (the next use asks again). */
  async forget(engine: NamedEngine): Promise<boolean> {
    const had = (await this.secrets.get(key(engine))) !== undefined;
    await this.secrets.delete(key(engine));
    return had;
  }

  private async forgetCommand() {
    const engines = this.list();
    if (engines.length === 0) {
      void vscode.window.showInformationMessage("Ductus: no engines are named in `ductus.engines`.");
      return;
    }
    const chosen = await vscode.window.showQuickPick(
      engines.map((e) => ({ label: e.name, description: e.url, engine: e })),
      { placeHolder: "The engine whose token to forget (the next use asks for one)" },
    );
    if (!chosen) return;
    const had = await this.forget(chosen.engine);
    void vscode.window.showInformationMessage(
      had ? `Ductus: forgot the token for ${chosen.label}.` : `Ductus: no token was kept for ${chosen.label}.`,
    );
  }
}

/** Where an engine's token is kept: by its URL, so renaming it in the setting keeps the token. */
function key(engine: NamedEngine): string {
  return `ductus.token:${engine.url}`;
}

/** What each need asks for, and where to make one. */
const PROMPTS: Record<Need, string> = {
  deploy:
    "One with the `deploy` permission: on the web UI's Account page (your own), or `ductus token create NAME --role operator --permission deploy`.",
  download:
    "One that can fetch releases (`download`, or an operator's `deploy`): on the web UI's Account page, or `ductus token create NAME --role viewer --permission download`.",
  payloads:
    "One with the `payloads` permission (debugging reads the message): on the web UI's Account page, or `ductus token create NAME --role viewer --permission payloads`.",
  read: "Any token of yours (the History view only reads): on the web UI's Account page, or `ductus token create NAME --role viewer`.",
};

/** The prompt: a password box saying what the token is for and how to make one. */
const prompt: Ask = (engine, need, why) =>
  Promise.resolve(
    vscode.window.showInputBox({
      title: `A token for ${engine.name} (${engine.url})`,
      prompt: (why ? `${why}. ` : "") + PROMPTS[need] + " Kept in your system's keychain.",
      password: true,
      ignoreFocusOut: true,
      placeHolder: "dux_…",
    }),
  );

/** `GET path` (under `/api/v1`) on an engine. */
async function call(
  engine: NamedEngine,
  path: string,
  token: string,
): Promise<{ status: number; body: string } | { unreachable: string }> {
  try {
    const response = await fetch(`${engine.url}/api/v1${path}`, {
      headers: { authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(CALL_WITHIN_MS),
    });
    return { status: response.status, body: await response.text() };
  } catch (e) {
    return { unreachable: `${engine.name} doesn't answer at ${engine.url} (${cause(e)})` };
  }
}

/** What a failed fetch says (a refused connection, an untrusted certificate), not "fetch failed". */
export function cause(e: unknown): string {
  const inner = e instanceof Error && e.cause instanceof Error ? e.cause.message : undefined;
  return inner ?? (e instanceof Error ? e.message : String(e));
}

/** An error answer's `error`, else its text. */
export function errorText(body: string): string {
  try {
    const value = JSON.parse(body);
    if (value && typeof value.error === "string") return value.error;
  } catch {
    // not JSON
  }
  return body.trim() || "no details";
}
