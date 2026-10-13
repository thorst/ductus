import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fromServer } from "@/api/appearance";
import {
  applyAppearance,
  currentAppearance,
  forgetAppearanceHere,
  forgetCustomColors,
  hasLocalChoice,
  onAppearanceChange,
  saveCustomColors,
  setTheme,
  type SavedColors,
} from "./theme";

const custom: SavedColors = {
  colors: { header: "#112233", accent: "#2255cc", bg: "#ffffff", panel: "#f4f4f4", text: "#111111" },
  vars: { "--accent": "#2255cc", "--bg": "#ffffff" },
  scheme: "light",
};

describe("the appearance kept in this browser", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.removeAttribute("style");
  });
  afterEach(() => onAppearanceChange(null));

  it("tells the listener about a person's changes, once each", () => {
    const heard = vi.fn();
    onAppearanceChange(heard);
    setTheme("dark");
    expect(heard).toHaveBeenLastCalledWith({ theme: "dark", colors: null });
    saveCustomColors(custom);
    expect(heard).toHaveBeenCalledTimes(2);
    expect(heard).toHaveBeenLastCalledWith({ theme: "custom", colors: custom });
    expect(document.documentElement.style.getPropertyValue("--accent")).toBe("#2255cc");
    forgetCustomColors();
    expect(heard).toHaveBeenCalledTimes(3);
    expect(heard).toHaveBeenLastCalledWith({ theme: "system", colors: null });
    expect(document.documentElement.style.getPropertyValue("--accent")).toBe("");
  });

  it("applies what the server has saved without sending it back", () => {
    const heard = vi.fn();
    onAppearanceChange(heard);
    applyAppearance({ theme: "custom", colors: custom });
    expect(heard).not.toHaveBeenCalled();
    expect(document.documentElement.dataset.theme).toBe("custom");
    expect(currentAppearance()).toEqual({ theme: "custom", colors: custom });
    // Custom with no colors is the system theme.
    applyAppearance({ theme: "custom", colors: null });
    expect(currentAppearance().theme).toBe("system");
  });

  it("goes back to the default for the next person", () => {
    saveCustomColors(custom);
    expect(hasLocalChoice()).toBe(true);
    forgetAppearanceHere();
    expect(hasLocalChoice()).toBe(false);
    expect(document.documentElement.dataset.theme).toBeUndefined();
    expect(localStorage.getItem("ductus.colors")).toBeNull();
  });

  it("reads the server's answer defensively", () => {
    expect(fromServer({ theme: "dark", colors: null, saved: true })).toEqual({ theme: "dark", colors: null });
    expect(fromServer({ theme: "neon", colors: null, saved: true }).theme).toBe("system");
    expect(fromServer({ theme: "custom", colors: { nothing: "usable" } as never, saved: true }).colors).toBeNull();
    expect(fromServer({ theme: "custom", colors: custom as never, saved: true }).colors).toEqual(custom);
  });
});
