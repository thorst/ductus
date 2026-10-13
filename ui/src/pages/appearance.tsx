import { CircleAlert, CircleCheck, Undo2, WandSparkles } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { useMemo, useState, type CSSProperties } from "react";
import { session } from "@/api/queries";
import { PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import {
  PALETTE_KEYS,
  PRESETS,
  check,
  derive,
  fix,
  fixAll,
  normalizeHex,
  type Check,
  type Palette,
  type PaletteKey,
} from "@/lib/colors";
import { forgetCustomColors, getSavedColors, getTheme, saveCustomColors } from "@/lib/theme";

// This page and the color math it needs load on demand (their own chunk, scripts/budget.mjs).

const LABEL = Object.fromEntries(PALETTE_KEYS.map(({ key, label }) => [key, label])) as Record<PaletteKey, string>;

/**
 * Five colors, a preview, and a check of every pair of colors the interface reads text or
 * controls with. A pair below WCAG AA (4.5:1) is flagged, with a "Fix it" that moves the text or
 * accent just far enough; it never stops anyone using what they chose, since it's their screen.
 */
export function AppearancePage() {
  const { data: me } = useQuery(session);
  const saved = getSavedColors();
  const [palette, setPalette] = useState<Palette>(saved?.colors ?? PRESETS.Light!);
  // What is in each hex box while typing (a half-typed color isn't a color yet).
  const [typed, setTyped] = useState<Partial<Record<PaletteKey, string>>>({});
  const [notice, setNotice] = useState<{ text: string; undo?: Palette } | null>(null);
  const [inUse, setInUse] = useState(() => getTheme() === "custom");

  const checks = useMemo(() => check(palette), [palette]);
  const derived = useMemo(() => derive(palette), [palette]);
  const hard = checks.filter((c) => !c.pass);

  const edit = (next: Palette) => {
    setPalette(next);
    setTyped({});
    setNotice(null);
  };
  const applyFix = (result: ReturnType<typeof fixAll>, what: string) => {
    if (result.changes.length === 0) return;
    const moved = result.changes.map((c) => `${LABEL[c.key]} ${c.from} → ${c.to}`).join(", ");
    setPalette(result.palette);
    setTyped({});
    setNotice({ text: `${what}: ${moved}.`, undo: palette });
  };
  const use = () => {
    saveCustomColors({ colors: palette, vars: derived.vars, scheme: derived.scheme });
    setInUse(true);
    setNotice({ text: "These colors are in use." });
  };
  const reset = () => {
    forgetCustomColors();
    setInUse(false);
    edit(PRESETS.Light!);
    setNotice({ text: "Your colors are forgotten; the theme follows the system again." });
  };

  return (
    <>
      <PageHeader title="Appearance">
        Choose five colors; the rest is worked out from them. The check below tells you where text may be hard to read.
        {me?.kind === "user"
          ? "They are saved to your account, so they follow you to any browser."
          : "They are kept in this browser only: a token has no account to save them in."}
      </PageHeader>
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)]">
        <div className="min-w-0 space-y-6">
          <Card aria-label="Colors">
            <CardHeader>
              <CardTitle>Your colors</CardTitle>
              <label className="flex items-center gap-2 text-sm">
                Start from
                <select
                  className="h-9 rounded-md border bg-panel px-2 text-sm"
                  value=""
                  onChange={(e) => {
                    const preset = PRESETS[e.target.value];
                    if (preset) edit(preset);
                  }}
                >
                  <option value="">a preset…</option>
                  {Object.keys(PRESETS).map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
            </CardHeader>
            <CardBody className="space-y-3">
              {PALETTE_KEYS.map(({ key, label, used }) => {
                const shown = typed[key] ?? palette[key];
                const valid = normalizeHex(shown) !== null;
                return (
                  <div key={key} className="flex flex-wrap items-center gap-3">
                    <div className="w-44">
                      <div className="text-sm font-medium">{label}</div>
                      <div className="text-xs text-muted">{used}</div>
                    </div>
                    <input
                      type="color"
                      aria-label={`${label} color`}
                      value={palette[key]}
                      onChange={(e) => edit({ ...palette, [key]: e.target.value })}
                      className="h-9 w-12 cursor-pointer rounded-md border bg-panel p-0.5"
                    />
                    <Input
                      aria-label={`${label} as hex`}
                      aria-invalid={!valid}
                      className="w-28 font-mono"
                      spellCheck={false}
                      value={shown}
                      onChange={(e) => {
                        const hex = normalizeHex(e.target.value);
                        setTyped({ ...typed, [key]: e.target.value });
                        setNotice(null);
                        if (hex) setPalette({ ...palette, [key]: hex });
                      }}
                    />
                    {!valid && <span className="text-xs text-bad">Use a color like #2f5bd3.</span>}
                  </div>
                );
              })}
            </CardBody>
          </Card>

          <Card aria-label="Contrast check">
            <CardHeader>
              <CardTitle>Is it readable?</CardTitle>
              {hard.length > 0 && (
                <Button variant="outline" size="sm" onClick={() => applyFix(fixAll(palette), "Fixed")}>
                  <WandSparkles aria-hidden /> Fix all
                </Button>
              )}
            </CardHeader>
            <CardBody className="space-y-3">
              <p role="status" className="flex items-start gap-2 text-sm">
                {hard.length === 0 ? (
                  <>
                    <CircleCheck aria-hidden className="mt-0.5 size-4 shrink-0 text-ok" />
                    Every pair reaches WCAG AA (4.5:1 for text).
                  </>
                ) : (
                  <>
                    <CircleAlert aria-hidden className="mt-0.5 size-4 shrink-0 text-warn" />
                    <span>
                      {hard.length} of {checks.length} pairs may be hard to read. You can still use these colors; Fix it
                      moves the text or accent colors just far enough.
                    </span>
                  </>
                )}
              </p>
              {notice && (
                <p className="flex flex-wrap items-center gap-2 rounded-md border bg-hover p-2 text-sm" role="status">
                  <span>{notice.text}</span>
                  {notice.undo && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        edit(notice.undo!);
                      }}
                    >
                      <Undo2 aria-hidden /> Undo
                    </Button>
                  )}
                </p>
              )}
              <Table aria-label="Contrast of each pair of colors">
                <thead>
                  <tr>
                    <Th>Pair</Th>
                    <Th>Sample</Th>
                    <Th>Ratio</Th>
                    <Th>Result</Th>
                    <Th>
                      <span className="sr-only">Fix</span>
                    </Th>
                  </tr>
                </thead>
                <tbody>
                  {checks.map((c) => (
                    <CheckRow key={c.id} c={c} onFix={(group) => applyFix(fix(palette, group), `Fixed ${c.label}`)} />
                  ))}
                </tbody>
              </Table>
            </CardBody>
          </Card>

          <div className="flex flex-wrap items-center gap-3">
            <Button onClick={use}>Use these colors</Button>
            <Button variant="outline" onClick={reset}>
              Reset
            </Button>
            {inUse && <span className="text-sm text-muted">Custom is the theme in use.</span>}
          </div>
        </div>

        <Preview palette={palette} vars={derived.vars} />
      </div>
    </>
  );
}

function CheckRow({ c, onFix }: { c: Check; onFix: (group: "text" | "accent") => void }) {
  const group = c.group;
  return (
    <tr>
      <Td>{c.label}</Td>
      <Td>
        <span
          aria-hidden
          data-user-colors
          className="inline-block rounded border px-2 font-semibold"
          style={{ background: c.bg, color: c.fg }}
        >
          Aa
        </span>
      </Td>
      <Td className="tabular-nums">
        {c.ratio.toFixed(1)}:1<span className="sr-only"> (needs {c.min}:1)</span>
        {c.pass && c.ratio >= 7 && <span className="ml-1 text-xs text-muted">AAA</span>}
      </Td>
      <Td>
        {c.pass ? (
          <span className="flex items-center gap-1 text-ok">
            <CircleCheck aria-hidden className="size-4" /> Readable
          </span>
        ) : (
          <span className="flex items-center gap-1 text-warn">
            <CircleAlert aria-hidden className="size-4" /> Hard to read
          </span>
        )}
      </Td>
      <Td>
        {!c.pass && group !== "derived" && (
          <Button variant="outline" size="sm" aria-label={`Fix it: ${c.label}`} onClick={() => onFix(group)}>
            Fix it
          </Button>
        )}
      </Td>
    </tr>
  );
}

/** What the colors look like: the real classes, with the derived values set on this box only. */
function Preview({ palette, vars }: { palette: Palette; vars: Record<string, string> }) {
  return (
    <Card aria-label="Preview" className="self-start">
      <CardHeader>
        <CardTitle>Preview</CardTitle>
      </CardHeader>
      <CardBody>
        {/* Look only: nothing in it can be focused or read out. */}
        <div
          aria-hidden
          data-user-colors
          className="overflow-hidden rounded-lg border bg-bg text-text"
          style={vars as CSSProperties}
        >
          <div className="flex h-10 items-center gap-2 bg-header px-3 text-sm font-semibold text-header-fg">
            Ductus <span className="font-normal opacity-80">{palette.header}</span>
          </div>
          <div className="space-y-3 p-3">
            <p className="text-sm text-muted">Page background</p>
            <div className="space-y-2 rounded-lg border bg-panel p-3 text-sm">
              <div className="font-semibold">A panel</div>
              <div>
                Text you read all day, with a link to <span className="text-accent underline">somewhere</span>.
              </div>
              <div className="text-muted">Secondary text, a little quieter.</div>
              <div className="flex flex-wrap gap-2 pt-1">
                <span className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-fg">Button</span>
                <span className="rounded-md bg-accent-soft px-3 py-1.5 text-sm font-medium text-accent">Open page</span>
              </div>
              <div className="flex flex-wrap gap-3 pt-1 text-xs font-medium">
                <span className="text-ok">● OK</span>
                <span className="text-warn">▲ Behind</span>
                <span className="text-bad">■ Errors</span>
                <span className="text-paused">❚❚ Paused</span>
              </div>
            </div>
          </div>
        </div>
      </CardBody>
    </Card>
  );
}
