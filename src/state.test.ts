import { describe, expect, test } from "bun:test";
import {
  createActivityTracker,
  createEpoch,
  createSerialRunner,
  errorMessage,
  filterSessions,
  isMissingTmuxError,
  reconcileSelection,
  validateDirectory,
  validateSessionName,
} from "./state";
import type { Session, SessionDetails } from "./types";

const session = (id: string, name: string): Session => ({
  id,
  name,
  windows: 1,
  attached: 0,
  created: 1_700_000_000,
});

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("filterSessions", () => {
  const sessions = [session("$1", "website"), session("$2", "API"), session("$3", "scratch")];

  test("matches names case-insensitively", () => {
    expect(filterSessions(sessions, "api").map((s) => s.id)).toEqual(["$2"]);
    expect(filterSessions(sessions, "SITE").map((s) => s.id)).toEqual(["$1"]);
  });

  test("ignores surrounding whitespace and returns everything for an empty query", () => {
    expect(filterSessions(sessions, "  ").length).toBe(3);
    expect(filterSessions(sessions, " scr ").map((s) => s.id)).toEqual(["$3"]);
  });

  test("returns nothing when no name matches", () => {
    expect(filterSessions(sessions, "zzz")).toEqual([]);
  });
});

describe("validateSessionName", () => {
  test("accepts 1 to 64 ASCII letters, digits, underscores and hyphens", () => {
    expect(validateSessionName("a")).toBeNull();
    expect(validateSessionName("my_project-2")).toBeNull();
    expect(validateSessionName("x".repeat(64))).toBeNull();
  });

  test("rejects empty, too long and unsupported characters", () => {
    expect(validateSessionName("")).toMatch(/enter a session name/i);
    expect(validateSessionName("   ")).toMatch(/enter a session name/i);
    expect(validateSessionName("x".repeat(65))).toMatch(/64/);
    for (const bad of ["my project", "a.b", "a:b", "zażółć", "$(rm)", "a'b"]) {
      expect(validateSessionName(bad)).toMatch(/letters, digits/i);
    }
  });
});

describe("validateDirectory", () => {
  test("requires a non-empty directory", () => {
    expect(validateDirectory("")).toMatch(/starting directory/i);
    expect(validateDirectory("  ")).toMatch(/starting directory/i);
    expect(validateDirectory("~/Projects")).toBeNull();
    expect(validateDirectory("/tmp/with space")).toBeNull();
  });
});

describe("reconcileSelection", () => {
  const details: SessionDetails = {
    session: session("$1", "website"),
    windows: [
      {
        id: "@1",
        index: 0,
        name: "dev",
        active: false,
        panes: [
          { id: "%1", index: 0, command: "bun", path: "/p", active: false, width: 80, height: 20 },
          { id: "%2", index: 1, command: "zsh", path: "/p", active: true, width: 80, height: 20 },
        ],
      },
      {
        id: "@2",
        index: 1,
        name: "shell",
        active: true,
        panes: [{ id: "%3", index: 0, command: "zsh", path: "/q", active: true, width: 80, height: 20 }],
      },
    ],
  };

  test("keeps a window and pane that still exist", () => {
    expect(reconcileSelection(details, { windowId: "@1", paneId: "%1" })).toEqual({ windowId: "@1", paneId: "%1" });
  });

  test("falls back to the active window and its active pane when nothing is selected", () => {
    expect(reconcileSelection(details, {})).toEqual({ windowId: "@2", paneId: "%3" });
  });

  test("falls back to the active window when the selected window disappeared", () => {
    expect(reconcileSelection(details, { windowId: "@9", paneId: "%1" })).toEqual({ windowId: "@2", paneId: "%3" });
  });

  test("picks the window's active pane when the selected pane is gone or belongs elsewhere", () => {
    expect(reconcileSelection(details, { windowId: "@1", paneId: "%3" })).toEqual({ windowId: "@1", paneId: "%2" });
    expect(reconcileSelection(details, { windowId: "@1", paneId: "%9" })).toEqual({ windowId: "@1", paneId: "%2" });
  });

  test("returns null pane for a window without panes and null for a session without windows", () => {
    const noPanes: SessionDetails = { ...details, windows: [{ ...details.windows[0]!, panes: [] }] };
    expect(reconcileSelection(noPanes, {})).toEqual({ windowId: "@1", paneId: null });
    expect(reconcileSelection({ ...details, windows: [] }, {})).toBeNull();
  });
});

describe("createSerialRunner", () => {
  test("never runs two tasks at once and coalesces requests into one trailing run", async () => {
    const gates: Array<{ resolve: () => void }> = [];
    let running = 0;
    let maxRunning = 0;
    const runner = createSerialRunner(async () => {
      running++;
      maxRunning = Math.max(maxRunning, running);
      const gate = deferred();
      gates.push(gate);
      await gate.promise;
      running--;
    });

    const first = runner.request();
    const second = runner.request();
    const third = runner.request();
    expect(runner.busy).toBe(true);
    expect(gates.length).toBe(1);

    gates[0]!.resolve();
    await new Promise((r) => setTimeout(r, 0));
    expect(gates.length).toBe(2);
    gates[1]!.resolve();
    await Promise.all([first, second, third]);

    expect(gates.length).toBe(2);
    expect(maxRunning).toBe(1);
    expect(runner.busy).toBe(false);
  });

  test("requestIfIdle skips while a run is in flight", async () => {
    const gate = deferred();
    let calls = 0;
    const runner = createSerialRunner(async () => {
      calls++;
      await gate.promise;
    });
    const first = runner.requestIfIdle();
    expect(first).not.toBeNull();
    expect(runner.requestIfIdle()).toBeNull();
    gate.resolve();
    await first;
    expect(calls).toBe(1);
  });

  test("a failing task does not wedge the runner", async () => {
    let calls = 0;
    const runner = createSerialRunner(async () => {
      calls++;
      if (calls === 1) throw new Error("boom");
    });
    await expect(runner.request()).rejects.toThrow("boom");
    expect(runner.busy).toBe(false);
    await runner.request();
    expect(calls).toBe(2);
  });
});

describe("createEpoch", () => {
  test("only the latest token is current", () => {
    const epoch = createEpoch();
    const a = epoch.next();
    expect(epoch.isCurrent(a)).toBe(true);
    const b = epoch.next();
    expect(epoch.isCurrent(a)).toBe(false);
    expect(epoch.isCurrent(b)).toBe(true);
  });
});

describe("errors", () => {
  test("recognises a missing tmux executable", () => {
    // Exact backend strings (src-tauri tmux.rs / process.rs).
    const missing = [
      "tmux was not found in PATH or in /opt/homebrew/bin, /usr/local/bin, /opt/local/bin, /usr/bin, /bin. Install tmux or set CWELUX_TMUX_PATH.",
      'CWELUX_TMUX_PATH must be an absolute path, got "tmux"',
      "CWELUX_TMUX_PATH does not point to an executable file: /nope/tmux",
      "could not start /opt/homebrew/bin/tmux: No such file or directory (os error 2)",
    ];
    for (const message of missing) expect(isMissingTmuxError(message)).toBe(true);
    const otherFailures = [
      // A stopped server: the socket path contains "tmux" and "No such file".
      "tmux display-message failed: error connecting to /private/tmp/tmux-501/default (No such file or directory)",
      "starting directory does not exist: /Users/me/tmux-work (No such file or directory)",
      "CWELUX_TMUX_SOCKET must be an absolute socket path, got \"sock\"",
      "can't find session: $9",
      "tmux exited with status 1",
    ];
    for (const message of otherFailures) expect(isMissingTmuxError(message)).toBe(false);
  });

  test("errorMessage turns any rejection into readable text", () => {
    expect(errorMessage("plain")).toBe("plain");
    expect(errorMessage(new Error("wrapped"))).toBe("wrapped");
    expect(errorMessage(undefined)).toMatch(/unknown error/i);
    expect(errorMessage({ code: 1 })).toMatch(/unknown error/i);
  });
});

describe("createActivityTracker", () => {
  function track() {
    const changes: boolean[] = [];
    const tracker = createActivityTracker((active) => changes.push(active));
    return { tracker, changes };
  }

  test("starts inactive until the native popover reports it is shown", () => {
    const { tracker, changes } = track();
    expect(tracker.active).toBe(false);
    tracker.setNativeVisible(true);
    expect(tracker.active).toBe(true);
    expect(changes).toEqual([true]);
  });

  test("a hide event stops activity even while the page still looks visible", () => {
    const { tracker, changes } = track();
    tracker.setNativeVisible(true);
    tracker.setNativeVisible(false);
    expect(tracker.active).toBe(false);
    expect(changes).toEqual([true, false]);
  });

  test("losing focus pauses and a new show event resumes", () => {
    const { tracker, changes } = track();
    tracker.setNativeVisible(true);
    tracker.setFocused(false);
    expect(tracker.active).toBe(false);
    tracker.setNativeVisible(true);
    expect(tracker.active).toBe(true);
    expect(changes).toEqual([true, false, true]);
  });

  test("a hidden page is never active", () => {
    const { tracker } = track();
    tracker.setPageVisible(false);
    tracker.setNativeVisible(true);
    expect(tracker.active).toBe(false);
    tracker.setPageVisible(true);
    expect(tracker.active).toBe(true);
  });

  test("repeated identical inputs do not report a change", () => {
    const { tracker, changes } = track();
    tracker.setNativeVisible(true);
    tracker.setNativeVisible(true);
    tracker.setFocused(true);
    expect(changes).toEqual([true]);
  });
});
