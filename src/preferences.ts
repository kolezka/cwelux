import { BridgeDataError } from "./types";

export type Theme = "system" | "light" | "dark";
export type RefreshInterval = 2 | 4 | 10;

/** Mirrors the Rust Preferences struct (serde camelCase). */
export interface Preferences {
  defaultDirectory: string;
  refreshIntervalSeconds: RefreshInterval;
  theme: Theme;
}

export const REFRESH_INTERVALS: readonly RefreshInterval[] = [2, 4, 10];
export const THEMES: readonly Theme[] = ["system", "light", "dark"];

export const DEFAULT_PREFERENCES: Preferences = {
  defaultDirectory: "~",
  refreshIntervalSeconds: 4,
  theme: "system",
};

export function decodePreferences(value: unknown, what = "preferences"): Preferences {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new BridgeDataError(`${what} is not an object`);
  }
  const o = value as Record<string, unknown>;
  const keys = Object.keys(o).sort().join(",");
  if (keys !== "defaultDirectory,refreshIntervalSeconds,theme") {
    throw new BridgeDataError(`${what} has unexpected fields: ${keys}`);
  }
  if (typeof o.defaultDirectory !== "string" || o.defaultDirectory.trim() === "") {
    throw new BridgeDataError(`${what}.defaultDirectory is not a directory`);
  }
  if (!REFRESH_INTERVALS.includes(o.refreshIntervalSeconds as RefreshInterval)) {
    throw new BridgeDataError(`${what}.refreshIntervalSeconds is not 2, 4 or 10`);
  }
  if (!THEMES.includes(o.theme as Theme)) {
    throw new BridgeDataError(`${what}.theme is not system, light or dark`);
  }
  return {
    defaultDirectory: o.defaultDirectory,
    refreshIntervalSeconds: o.refreshIntervalSeconds as RefreshInterval,
    theme: o.theme as Theme,
  };
}

export function decodeBoolean(value: unknown, what: string): boolean {
  if (typeof value !== "boolean") throw new BridgeDataError(`${what} is not true or false`);
  return value;
}

/** A folder picker answers with a path, or null when cancelled. */
export function decodeChosenDirectory(value: unknown): string | null {
  if (value === null) return null;
  if (typeof value !== "string" || value.trim() === "") throw new BridgeDataError("chosen directory is not a path");
  return value;
}

/** "system" follows the OS; light and dark force the CSS tokens. */
export function applyTheme(doc: Document, theme: Theme): void {
  if (theme === "system") delete doc.documentElement.dataset.theme;
  else doc.documentElement.dataset.theme = theme;
}
