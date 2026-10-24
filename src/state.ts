import type { Session, SessionDetails } from "./types";

export function filterSessions(sessions: Session[], query: string): Session[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return sessions;
  return sessions.filter((s) => s.name.toLowerCase().includes(needle));
}

const SESSION_NAME = /^[A-Za-z0-9_-]+$/;

/** Returns an error message, or null when the name is acceptable for create and rename. */
export function validateSessionName(raw: string): string | null {
  const name = raw.trim();
  if (!name) return "Enter a session name.";
  if (name.length > 64) return "Use at most 64 characters.";
  if (!SESSION_NAME.test(name)) return "Use only ASCII letters, digits, underscores and hyphens.";
  return null;
}

export function validateDirectory(raw: string): string | null {
  return raw.trim() ? null : "Enter a starting directory.";
}

export interface Selection {
  windowId?: string | null;
  paneId?: string | null;
}

/** Keeps the previous window and pane when they still exist, otherwise falls back to the active ones. */
export function reconcileSelection(
  details: SessionDetails,
  previous: Selection,
): { windowId: string; paneId: string | null } | null {
  const windows = details.windows;
  const window =
    windows.find((w) => w.id === previous.windowId) ?? windows.find((w) => w.active) ?? windows[0];
  if (!window) return null;
  const pane =
    window.panes.find((p) => p.id === previous.paneId) ?? window.panes.find((p) => p.active) ?? window.panes[0];
  return { windowId: window.id, paneId: pane?.id ?? null };
}

/**
 * Runs one task at a time. `request` coalesces calls made during a run into a single
 * trailing run, so a mutation never relies on data fetched before it finished.
 * `requestIfIdle` is for polling: it skips instead of queueing.
 */
export function createSerialRunner(task: () => Promise<void>) {
  let current: Promise<void> | null = null;
  let trailing: Promise<void> | null = null;

  const start = (): Promise<void> => {
    const run = task().finally(() => {
      if (current === run) current = null;
    });
    current = run;
    return run;
  };

  return {
    get busy() {
      return current !== null;
    },
    request(): Promise<void> {
      if (!current) return start();
      if (!trailing) {
        trailing = current
          .catch(() => {})
          .then(() => {
            trailing = null;
            return start();
          });
      }
      return trailing;
    },
    requestIfIdle(): Promise<void> | null {
      return current ? null : start();
    },
  };
}

/** A counter for discarding responses that belong to an earlier selection. */
export function createEpoch() {
  let value = 0;
  return {
    next: () => ++value,
    isCurrent: (token: number) => token === value,
  };
}

// Matches the backend's exact prefixes. A loose "tmux ... no such file" match would also
// catch a stopped server, whose socket path contains "tmux".
const MISSING_TMUX = /^(tmux was not found|CWELUX_TMUX_PATH |could not start \S*tmux)/;

export function isMissingTmuxError(message: string): boolean {
  return MISSING_TMUX.test(message);
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string" && error) return error;
  if (error instanceof Error && error.message) return error.message;
  return "Unknown error.";
}

/**
 * Decides when polling may run. The native side reports popover show/hide; a show
 * implies focus, and a blur or hidden page pauses until the next show or focus.
 */
export function createActivityTracker(onChange: (active: boolean) => void) {
  let nativeVisible = false;
  let focused = true;
  let pageVisible = true;
  let active = false;

  const update = () => {
    const next = nativeVisible && focused && pageVisible;
    if (next === active) return;
    active = next;
    onChange(active);
  };

  return {
    get active() {
      return active;
    },
    setNativeVisible(visible: boolean) {
      nativeVisible = visible;
      if (visible) focused = true;
      update();
    },
    setFocused(value: boolean) {
      focused = value;
      update();
    },
    setPageVisible(value: boolean) {
      pageVisible = value;
      update();
    },
  };
}
