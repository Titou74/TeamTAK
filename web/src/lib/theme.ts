import { useSyncExternalStore } from "react";

/** Colour themes an account may choose. `auto` follows the device's light or dark setting. */
export const THEMES = ["auto", "light", "dark"] as const;
export type Theme = (typeof THEMES)[number];

// Read before the first paint by public/theme-init.js: keep the key and values the same.
const STORAGE_KEY = "teamtak.theme";
const deviceDark = window.matchMedia("(prefers-color-scheme: dark)");
const listeners = new Set<() => void>();

export function isTheme(value: unknown): value is Theme {
  return typeof value === "string" && (THEMES as readonly string[]).includes(value);
}

function stored(): Theme {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    return isTheme(value) ? value : "auto";
  } catch {
    return "auto";
  }
}

let current: Theme = stored();

function render() {
  const dark = current === "dark" || (current === "auto" && deviceDark.matches);
  document.documentElement.classList.toggle("dark", dark);
  for (const listener of listeners) listener();
}

deviceDark.addEventListener("change", () => {
  if (current === "auto") render();
});

/**
 * Applies a theme and remembers it on this device, so the next visit starts in it before the
 * account is loaded. The account's choice is applied once signed in, and `auto` once signed out.
 */
export function applyTheme(theme: Theme) {
  current = theme;
  try {
    if (theme === "auto") window.localStorage.removeItem(STORAGE_KEY);
    else window.localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Private browsing: the next visit starts from the device's setting.
  }
  render();
}

export function currentTheme(): Theme {
  return current;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The chosen theme; re-renders when it changes. */
export function useTheme(): Theme {
  return useSyncExternalStore(subscribe, currentTheme);
}

/** Whether the page is dark right now, whichever way that was decided. */
export function useIsDark(): boolean {
  return useSyncExternalStore(subscribe, () => document.documentElement.classList.contains("dark"));
}
