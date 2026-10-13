import { describe, expect, it } from "vitest";
import {
  PRESETS,
  PALETTE_KEYS,
  check,
  contrast,
  derive,
  failing,
  fix,
  fixAll,
  mix,
  normalizeHex,
  type Palette,
} from "./colors";

/** A small seeded generator, so a failure can be reproduced. */
function random(seed: number) {
  return () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 2 ** 32;
  };
}

function randomPalette(next: () => number): Palette {
  const color = () =>
    "#" +
    Math.floor(next() * 0x1000000)
      .toString(16)
      .padStart(6, "0");
  return { header: color(), accent: color(), bg: color(), panel: color(), text: color() };
}

describe("contrast", () => {
  it("matches the WCAG reference values", () => {
    expect(contrast("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrast("#ffffff", "#ffffff")).toBeCloseTo(1, 5);
    expect(contrast("#777777", "#ffffff")).toBeCloseTo(4.48, 2);
    expect(contrast("#767676", "#ffffff")).toBeCloseTo(4.54, 2);
  });

  it("doesn't depend on the order", () => {
    expect(contrast("#2f5bd3", "#f3f5f8")).toBe(contrast("#f3f5f8", "#2f5bd3"));
  });
});

describe("hex input", () => {
  it("takes #rgb, #rrggbb and no #", () => {
    expect(normalizeHex("#ABC")).toBe("#aabbcc");
    expect(normalizeHex("17233B")).toBe("#17233b");
    expect(normalizeHex(" #17233b ")).toBe("#17233b");
  });

  it("refuses anything else", () => {
    for (const s of ["", "#12", "#12345", "#gggggg", "red", "#1234567"]) expect(normalizeHex(s), s).toBeNull();
  });
});

describe("mix", () => {
  it("is the first color at weight 1, the second at 0", () => {
    expect(mix("#2f5bd3", "#ffffff", 1)).toBe("#2f5bd3");
    expect(mix("#2f5bd3", "#ffffff", 0)).toBe("#ffffff");
  });

  it("lands between", () => {
    const mid = mix("#000000", "#ffffff", 0.5);
    expect(contrast(mid, "#000000")).toBeGreaterThan(1);
    expect(contrast(mid, "#ffffff")).toBeGreaterThan(1);
  });
});

describe("the presets", () => {
  it.each(Object.entries(PRESETS))("%s passes the check", (_, palette) => {
    expect(failing(palette).map((c) => c.label)).toEqual([]);
  });

  it("Light and Dark are the built-in themes' own colors", () => {
    expect(PRESETS.Light).toMatchObject({ accent: "#2f5bd3", bg: "#f3f5f8", panel: "#ffffff" });
    expect(PRESETS.Dark).toMatchObject({ accent: "#7ca3ff", bg: "#0e131a", panel: "#151c26" });
  });
});

describe("derive", () => {
  it("writes every variable the stylesheet takes from a theme, as plain values", () => {
    const { vars } = derive(PRESETS.Light!);
    expect(Object.keys(vars).sort()).toEqual(
      [
        "--accent",
        "--accent-fg",
        "--accent-soft",
        "--bg",
        "--border",
        "--bad",
        "--header",
        "--header-fg",
        "--hover",
        "--muted",
        "--ok",
        "--panel",
        "--paused",
        "--text",
        "--warn",
      ].sort(),
    );
    for (const value of Object.values(vars)) expect(value).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("picks the color scheme from the backgrounds", () => {
    expect(derive(PRESETS.Light!).scheme).toBe("light");
    expect(derive(PRESETS.Dark!).scheme).toBe("dark");
  });

  it("makes the status colors readable on both backgrounds, whatever they are", () => {
    const next = random(7);
    for (let i = 0; i < 300; i++) {
      const p = randomPalette(next);
      // Status colors can only be as readable as the backgrounds allow: judge palettes whose
      // text is readable (the others are the check's business).
      if (failing(p).some((c) => c.group === "text")) continue;
      for (const c of derive(p).status) {
        expect(contrast(c, p.bg), JSON.stringify(p)).toBeGreaterThanOrEqual(4.5);
        expect(contrast(c, p.panel), JSON.stringify(p)).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("always makes the header and button text readable", () => {
    const next = random(11);
    for (let i = 0; i < 500; i++) {
      const p = randomPalette(next);
      const derived = check(p).filter((c) => c.group === "derived" && c.id !== "status");
      expect(
        derived.filter((c) => !c.pass),
        JSON.stringify(p),
      ).toEqual([]);
    }
  });
});

describe("check", () => {
  const pale: Palette = { header: "#ffffff", accent: "#ffeb3b", bg: "#ffffff", panel: "#ffffff", text: "#cccccc" };

  it("passes a good scheme", () => {
    expect(check(PRESETS.Light!).every((c) => c.pass)).toBe(true);
  });

  it("names what is hard to read, with its ratio", () => {
    const found = failing(pale);
    expect(found.map((c) => c.id)).toEqual(expect.arrayContaining(["text-bg", "text-panel", "link-bg", "link-panel"]));
    const text = found.find((c) => c.id === "text-bg")!;
    expect(text.ratio).toBeCloseTo(1.6, 1);
    expect(text.min).toBe(4.5);
  });
});

describe("fix", () => {
  const pale: Palette = { header: "#ffffff", accent: "#ffeb3b", bg: "#ffffff", panel: "#ffffff", text: "#cccccc" };

  it("moves only the text for a text problem, keeping its hue", () => {
    const before = { ...PRESETS.Light!, text: "#9aa3ad" };
    expect(failing(before).every((c) => c.group === "text")).toBe(true);
    const { palette, changes } = fix(before, "text");
    expect(changes.map((c) => c.key)).toEqual(["text"]);
    expect(failing(palette)).toEqual([]);
    // Far enough for the secondary text too (two thirds of the text color, mixed into the panel),
    // but a dark slate, not black.
    expect(palette.text).not.toBe("#000000");
    expect(contrast(palette.text, before.bg)).toBeLessThan(contrast("#000000", before.bg));
  });

  it("leaves a passing scheme alone", () => {
    const { palette, changes } = fixAll(PRESETS.Ocean!);
    expect(changes).toEqual([]);
    expect(palette).toEqual(PRESETS.Ocean);
  });

  it("repairs pale text and a yellow accent on white, changing only those two", () => {
    const { palette, changes } = fixAll(pale);
    expect(failing(palette)).toEqual([]);
    expect(changes.map((c) => c.key).sort()).toEqual(["accent", "text"]);
  });

  it("goes lighter on a dark scheme", () => {
    const dim: Palette = { ...PRESETS.Dark!, text: "#4a5260", accent: "#2a3a66" };
    const { palette } = fixAll(dim);
    expect(failing(palette)).toEqual([]);
    expect(contrast(palette.text, "#000000")).toBeGreaterThan(contrast(dim.text, "#000000"));
  });

  it("pulls the panel toward the page when no text could be read on both", () => {
    const split: Palette = { ...PRESETS.Light!, bg: "#000000", panel: "#ffffff" };
    const { palette, changes } = fixAll(split);
    expect(failing(palette)).toEqual([]);
    expect(changes.map((c) => c.key)).toContain("panel");
    expect(palette.bg).toBe("#000000");
  });

  it("repairs mid-grey backgrounds, the hardest case, by moving them as a last resort", () => {
    const grey: Palette = { header: "#333333", accent: "#888888", bg: "#808080", panel: "#808080", text: "#808080" };
    const { palette } = fixAll(grey);
    expect(failing(palette)).toEqual([]);
  });

  it("repairs any scheme: 300 random ones, each ending clean", () => {
    const next = random(2026);
    let changed = 0;
    for (let i = 0; i < 300; i++) {
      const p = randomPalette(next);
      const { palette, changes } = fixAll(p);
      expect(
        failing(palette).map((c) => c.label),
        JSON.stringify(p),
      ).toEqual([]);
      // The header is the user's, always (its text is derived).
      expect(palette.header).toBe(p.header);
      if (changes.length) changed++;
    }
    expect(changed).toBeGreaterThan(0);
  }, 60_000);

  it("reports before and after for each change", () => {
    const { changes } = fixAll(pale);
    for (const c of changes) {
      expect(PALETTE_KEYS.map((k) => k.key)).toContain(c.key);
      expect(c.from).not.toBe(c.to);
      expect(c.to).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});
