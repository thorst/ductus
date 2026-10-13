// A signed-in user's theme and custom colors live on the server (`/me/settings`), so they follow
// the person to any browser. This keeps the two in step:
//
//  - after sign-in the server's choice is applied; a person who never saved one gets what this
//    browser has (if they had picked anything) sent up once;
//  - every change after that is sent up as it is made.
//
// A session made with a token has no account to save on: it keeps the browser's own choice.

import { api, unwrap, type Schemas } from "./client";
import {
  applyAppearance,
  currentAppearance,
  hasLocalChoice,
  isSavedColors,
  onAppearanceChange,
  THEMES,
  type Appearance,
} from "@/lib/theme";

let syncedFor: string | null = null;

function toServer(a: Appearance): Schemas["Appearance"] {
  // The document says "an object"; what is in it is the UI's own to know (SavedColors).
  return { theme: a.theme, colors: a.colors as Schemas["Appearance"]["colors"] };
}

async function save(a: Appearance) {
  try {
    await api.PUT("/me/settings", { body: toServer(a) });
  } catch {
    // Offline for a moment: the browser's copy still has it, and the next change sends it again.
  }
}

/** The server's saved appearance as the UI uses it (None where it holds something unusable). */
export function fromServer(saved: Schemas["Appearance"]): Appearance {
  const theme = THEMES.find((t) => t === saved.theme) ?? "system";
  const colors = isSavedColors(saved.colors) ? saved.colors : null;
  return { theme, colors };
}

/** Called once the session is known. Does its work once per signed-in person. */
export async function syncAppearance(me: Schemas["Session"]) {
  if (me.kind !== "user") {
    onAppearanceChange(null);
    return;
  }
  if (syncedFor === me.name) return;
  syncedFor = me.name;
  try {
    const saved = unwrap(await api.GET("/me/settings"));
    if (saved.saved) applyAppearance(fromServer(saved));
    else if (hasLocalChoice()) await save(currentAppearance());
  } catch {
    // The engine didn't answer: the browser's copy is used until the next visit.
    syncedFor = null;
  }
  onAppearanceChange((a) => void save(a));
}

/** Signing out: the next person to sign in is treated as new. */
export function forgetSync() {
  syncedFor = null;
  onAppearanceChange(null);
}
