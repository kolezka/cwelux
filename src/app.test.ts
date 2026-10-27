import { beforeEach, describe, expect, test } from "bun:test";
import { Window } from "happy-dom";
import { createTauriBridge, type TmuxBridge } from "./api";
import { createApp, renderUnavailable, type AppHandle } from "./app";
import { DEFAULT_PREFERENCES, type Preferences } from "./preferences";
import type { Session, SessionDetails } from "./types";

type Method = keyof TmuxBridge;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const session = (id: string, name: string, extra: Partial<Session> = {}): Session => ({
  id,
  name,
  windows: 2,
  attached: 0,
  created: 1_700_000_000,
  ...extra,
});

function detailsFor(s: Session): SessionDetails {
  const n = s.id.replace("$", "");
  return {
    session: s,
    windows: [
      {
        id: `@${n}0`,
        index: 0,
        name: "dev",
        active: true,
        panes: [
          { id: `%${n}0`, index: 0, command: "bun", path: `/work/${s.name}`, active: true, width: 120, height: 20 },
          { id: `%${n}1`, index: 1, command: "zsh", path: `/work/${s.name}`, active: false, width: 120, height: 12 },
        ],
      },
      {
        id: `@${n}1`,
        index: 1,
        name: "shell",
        active: false,
        panes: [{ id: `%${n}2`, index: 0, command: "zsh", path: "/tmp", active: true, width: 120, height: 32 }],
      },
    ],
  };
}

/** Records calls; each call is answered by an override handler or a default. */
class FakeBridge implements TmuxBridge {
  calls: Array<{ method: Method; args: unknown[] }> = [];
  sessions: Session[] = [session("$1", "website", { attached: 1 }), session("$2", "api")];
  handlers: Partial<Record<Method, (...args: any[]) => Promise<any>>> = {};
  preferences: Preferences = { ...DEFAULT_PREFERENCES };

  private call<T>(method: Method, args: unknown[], fallback: () => T): Promise<T> {
    this.calls.push({ method, args });
    const handler = this.handlers[method];
    return handler ? handler(...args) : Promise.resolve().then(fallback);
  }
  count(method: Method) {
    return this.calls.filter((c) => c.method === method).length;
  }
  listSessions() {
    return this.call("listSessions", [], () => structuredClone(this.sessions));
  }
  sessionDetails(sessionId: string) {
    return this.call("sessionDetails", [sessionId], () => {
      const found = this.sessions.find((s) => s.id === sessionId);
      if (!found) throw `can't find session: ${sessionId}`;
      return detailsFor(found);
    });
  }
  capturePane(sessionId: string, paneId: string) {
    return this.call("capturePane", [sessionId, paneId], () => `output of ${paneId}`);
  }
  createSession(name: string, directory: string) {
    return this.call("createSession", [name, directory], () => {
      const created = session(`$${this.sessions.length + 10}`, name);
      this.sessions.push(created);
      return created;
    });
  }
  renameSession(sessionId: string, name: string) {
    return this.call("renameSession", [sessionId, name], () => {
      const found = this.sessions.find((s) => s.id === sessionId);
      if (found) found.name = name;
    });
  }
  killSession(sessionId: string) {
    return this.call("killSession", [sessionId], () => {
      this.sessions = this.sessions.filter((s) => s.id !== sessionId);
    });
  }
  openSession(sessionId: string) {
    return this.call("openSession", [sessionId], () => undefined);
  }
  attachCommand(sessionId: string) {
    return this.call("attachCommand", [sessionId], () => `tmux attach-session -t '${sessionId}'`);
  }
  hidePopover() {
    return this.call("hidePopover", [], () => undefined);
  }
  getPreferences() {
    return this.call("getPreferences", [], () => ({ ...this.preferences }));
  }
  openSettings() {
    return this.call("openSettings", [], () => undefined);
  }
}

class FakeTimers {
  active = new Map<number, () => void>();
  delays = new Map<number, number>();
  private nextId = 1;
  setInterval = (fn: () => void, ms: number) => {
    const id = this.nextId++;
    this.active.set(id, fn);
    this.delays.set(id, ms);
    return id;
  };
  clearInterval = (id: unknown) => {
    this.active.delete(id as number);
  };
  activeDelays() {
    return [...this.active.keys()].map((id) => this.delays.get(id));
  }
  tick() {
    for (const fn of [...this.active.values()]) fn();
  }
}

let window: Window;
let document: Document;
let root: HTMLElement;
let bridge: FakeBridge;
let timers: FakeTimers;
let copied: string[];
let app: AppHandle;

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};
const $ = <T extends HTMLElement = HTMLElement>(selector: string) => {
  const el = root.querySelector(selector);
  if (!el) throw new Error(`missing ${selector}`);
  return el as unknown as T;
};
const visibleView = () => root.querySelector<HTMLElement>("[data-view]:not([hidden])")?.dataset.view;
const sessionNames = () => [...root.querySelectorAll("#session-list .session-name")].map((el) => el.textContent);
const type = (selector: string, value: string) => {
  const input = $<HTMLInputElement>(selector);
  input.value = value;
  input.dispatchEvent(new window.Event("input", { bubbles: true }) as unknown as Event);
};
const submit = (selector: string) => {
  $<HTMLFormElement>(selector).dispatchEvent(
    new window.Event("submit", { bubbles: true, cancelable: true }) as unknown as Event,
  );
};

async function mount(b: TmuxBridge = bridge) {
  app = createApp({
    root,
    bridge: b,
    timers,
    pollIntervalMs: 1000,
    copyText: async (text) => {
      copied.push(text);
    },
  });
  await app.start();
  await flush();
}

async function openSession(id: string) {
  $(`#session-list [data-session-id="${id}"]`).click();
  await flush();
}

beforeEach(() => {
  window = new Window({ url: "http://localhost/" });
  document = window.document as unknown as Document;
  document.body.innerHTML = '<main id="app"></main>';
  root = document.getElementById("app")!;
  bridge = new FakeBridge();
  timers = new FakeTimers();
  copied = [];
});

describe("session list", () => {
  test("shows sessions with window and client counts", async () => {
    await mount();
    expect(visibleView()).toBe("list");
    expect(sessionNames()).toEqual(["website", "api"]);
    expect($("#session-count").textContent).toBe("2");
    const meta = [...root.querySelectorAll("#session-list .session-meta")].map((el) => el.textContent);
    expect(meta).toEqual(["2 windows · 1 client attached", "2 windows · Detached"]);
  });

  test("session rows live inside one grouped card", async () => {
    await mount();
    const group = root.querySelector("#session-list .group");
    expect(group).not.toBeNull();
    expect(group!.querySelectorAll(".session").length).toBe(bridge.sessions.length);
  });

  test("an attached session shows a status dot, a detached one does not", async () => {
    bridge.sessions = [session("$1", "a", { attached: 1 }), session("$2", "b", { attached: 0 })];
    await mount();
    const dots = [...root.querySelectorAll("#session-list .session")].map((row) => row.querySelector(".dot") !== null);
    expect(dots).toEqual([true, false]);
  });

  test("an empty list renders no grouped card", async () => {
    bridge.sessions = [];
    await mount();
    expect(root.querySelector("#session-list .group")).toBeNull();
  });

  test("renders tmux text as text, never as markup", async () => {
    bridge.sessions = [session("$1", '<img src=x onerror="alert(1)">')];
    await mount();
    expect(sessionNames()).toEqual(['<img src=x onerror="alert(1)">']);
    expect(root.querySelector("img")).toBeNull();
  });

  test("search filters by name and explains when nothing matches", async () => {
    await mount();
    type("#search", "AP");
    expect(sessionNames()).toEqual(["api"]);
    type("#search", "nothing");
    expect(sessionNames()).toEqual([]);
    expect($("#list-state").textContent).toMatch(/no sessions match/i);
  });

  test("shows a loading state until the first response", async () => {
    const pending = deferred<Session[]>();
    bridge.handlers.listSessions = () => pending.promise;
    app = createApp({ root, bridge, timers, copyText: async () => {} });
    void app.start();
    await flush();
    expect($("#list-state").dataset.state).toBe("loading");
    expect($("#list-state").textContent).toMatch(/loading/i);
    pending.resolve([]);
    await flush();
    expect($("#list-state").dataset.state).toBe("empty");
  });

  test("an empty server is an explicit empty state", async () => {
    bridge.sessions = [];
    await mount();
    expect($("#list-state").dataset.state).toBe("empty");
    expect($("#list-state").textContent).toMatch(/no tmux sessions/i);
  });

  test("a stopped-server socket error is a command failure, not missing tmux", async () => {
    bridge.handlers.listSessions = async () => {
      throw "tmux list-sessions failed: error connecting to /private/tmp/tmux-501/default (No such file or directory)";
    };
    await mount();
    expect($("#list-state").dataset.state).toBe("error");
  });

  test("a command failure shows the error and retry recovers", async () => {
    bridge.handlers.listSessions = async () => {
      throw "tmux exited with status 1: server exploded";
    };
    await mount();
    expect($("#list-state").dataset.state).toBe("error");
    expect($("#list-state").textContent).toContain("server exploded");
    expect(sessionNames()).toEqual([]);
    delete bridge.handlers.listSessions;
    $("#retry").click();
    await flush();
    expect(sessionNames()).toEqual(["website", "api"]);
    expect(root.querySelector<HTMLElement>("#retry")!.hidden).toBe(true);
  });

  test("a missing tmux executable has its own state", async () => {
    bridge.handlers.listSessions = async () => {
      throw "tmux was not found in PATH or in /opt/homebrew/bin, /usr/local/bin, /opt/local/bin, /usr/bin, /bin. Install tmux or set CWELUX_TMUX_PATH.";
    };
    await mount();
    expect($("#list-state").dataset.state).toBe("missing-tmux");
    expect($("#list-state").textContent).toMatch(/tmux is not installed|tmux not found/i);
    expect($("#list-state").textContent).toContain("CWELUX_TMUX_PATH");
  });

  test("malformed backend data surfaces as an error, not an empty list", async () => {
    const malformed = createTauriBridge(async () => [{ id: "$1", name: "x" }]);
    await mount(malformed);
    expect($("#list-state").dataset.state).toBe("error");
    expect($("#list-state").textContent).toMatch(/unexpected data/i);
  });

  test("a failed refresh after a good one keeps no stale list on screen", async () => {
    await mount();
    bridge.handlers.listSessions = async () => {
      throw "boom";
    };
    $("#refresh").click();
    await flush();
    expect($("#list-state").dataset.state).toBe("error");
    expect(sessionNames()).toEqual([]);
  });
});

describe("session details", () => {
  test("shows windows, panes and the active pane preview", async () => {
    await mount();
    await openSession("$1");
    expect(visibleView()).toBe("detail");
    expect($("#detail-name").textContent).toBe("website");
    expect($("#detail-status").textContent).toBe("Attached");
    expect($("#clients").textContent).toBe("1 attached");
    expect($("#directory").textContent).toBe("/work/website");
    const tabs = [...root.querySelectorAll<HTMLElement>("#window-tabs .tab")];
    expect(tabs.map((t) => t.textContent)).toEqual(["0: dev", "1: shell"]);
    expect(tabs.map((t) => t.getAttribute("aria-selected"))).toEqual(["true", "false"]);
    const panes = [...root.querySelectorAll<HTMLElement>("#panes .pane")];
    expect(panes.length).toBe(2);
    expect(panes[0]!.textContent).toContain("bun");
    expect(panes[0]!.textContent).toContain("120 × 20");
    expect($("#output").textContent).toBe("output of %10");
    expect(bridge.calls.find((c) => c.method === "capturePane")?.args).toEqual(["$1", "%10"]);
  });

  test("switching window and pane changes the preview to the right pane", async () => {
    await mount();
    await openSession("$1");
    $('#window-tabs [data-window-id="@11"]').click();
    await flush();
    expect($('#window-tabs [data-window-id="@11"]').getAttribute("aria-selected")).toBe("true");
    expect($("#output").textContent).toBe("output of %12");

    $('#window-tabs [data-window-id="@10"]').click();
    await flush();
    $('#panes [data-pane-id="%11"]').click();
    await flush();
    expect($('#panes [data-pane-id="%11"]').getAttribute("aria-pressed")).toBe("true");
    expect($("#output").textContent).toBe("output of %11");
    expect(bridge.calls.filter((c) => c.method === "capturePane").at(-1)?.args).toEqual(["$1", "%11"]);
  });

  test("a late preview for a previously selected pane is discarded and the old text is cleared", async () => {
    await mount();
    await openSession("$1");
    const slow = deferred<string>();
    bridge.handlers.capturePane = (_s: string, paneId: string) =>
      paneId === "%11" ? slow.promise : Promise.resolve(`output of ${paneId}`);
    $('#panes [data-pane-id="%11"]').click();
    await flush();
    expect($("#output").textContent).toBe("");
    $('#panes [data-pane-id="%10"]').click();
    await flush();
    expect($("#output").textContent).toBe("output of %10");
    slow.resolve("STALE OUTPUT");
    await flush();
    expect($("#output").textContent).toBe("output of %10");
  });

  test("a preview error replaces old output with the error", async () => {
    await mount();
    await openSession("$1");
    bridge.handlers.capturePane = async () => {
      throw "pane %11 is not in session $1";
    };
    $('#panes [data-pane-id="%11"]').click();
    await flush();
    expect($("#output").textContent).toBe("");
    expect($("#output-state").textContent).toContain("pane %11 is not in session $1");
  });

  test("a late details response for a previous session is ignored", async () => {
    await mount();
    const slow = deferred<SessionDetails>();
    bridge.handlers.sessionDetails = (id: string) =>
      id === "$1" ? slow.promise : Promise.resolve(detailsFor(bridge.sessions[1]!));
    await openSession("$1");
    $("#back").click();
    await flush();
    await openSession("$2");
    slow.resolve(detailsFor(bridge.sessions[0]!));
    await flush();
    expect($("#detail-name").textContent).toBe("api");
    expect($("#directory").textContent).toBe("/work/api");
  });

  test("a session that disappears shows a visible error and no stale panes", async () => {
    await mount();
    await openSession("$1");
    bridge.sessions = bridge.sessions.filter((s) => s.id !== "$1");
    await app.refresh();
    await flush();
    expect($("#detail-error").textContent).toMatch(/no longer running|can't find session/i);
    expect(root.querySelectorAll("#panes .pane").length).toBe(0);
    expect($("#output").textContent).toBe("");
    expect($<HTMLButtonElement>("#open-terminal").disabled).toBe(true);
  });

  test("tmux text in panes and output is rendered as text", async () => {
    const evil = "<b>bold</b>";
    bridge.handlers.capturePane = async () => evil;
    bridge.sessions = [session("$1", "x")];
    const d = detailsFor(bridge.sessions[0]!);
    d.windows[0]!.panes[0]!.command = "<script>x()</script>";
    bridge.handlers.sessionDetails = async () => d;
    await mount();
    await openSession("$1");
    expect($("#output").textContent).toBe(evil);
    expect(root.querySelector("#output b")).toBeNull();
    expect(root.querySelector("#panes script")).toBeNull();
    expect($("#panes").textContent).toContain("<script>x()</script>");
  });
});

describe("create session", () => {
  test("validates locally without calling tmux", async () => {
    await mount();
    $("#new-session").click();
    expect(visibleView()).toBe("create");
    type("#new-name", "bad name");
    type("#new-directory", "~/p");
    submit("#create-form");
    await flush();
    expect($("#create-error").textContent).toMatch(/letters, digits/i);
    type("#new-name", "ok");
    type("#new-directory", " ");
    submit("#create-form");
    await flush();
    expect($("#create-error").textContent).toMatch(/starting directory/i);
    expect(bridge.count("createSession")).toBe(0);
  });

  test("creates once while pending and opens the new session", async () => {
    await mount();
    $("#new-session").click();
    const pending = deferred<Session>();
    bridge.handlers.createSession = () => pending.promise;
    type("#new-name", "proj_1");
    type("#new-directory", "~/Projects/my app");
    submit("#create-form");
    submit("#create-form");
    await flush();
    expect(bridge.count("createSession")).toBe(1);
    expect(bridge.calls.find((c) => c.method === "createSession")?.args).toEqual(["proj_1", "~/Projects/my app"]);
    expect($<HTMLButtonElement>("#create-submit").disabled).toBe(true);
    const created = session("$9", "proj_1");
    bridge.sessions.push(created);
    pending.resolve(created);
    await flush();
    expect(visibleView()).toBe("detail");
    expect($("#detail-name").textContent).toBe("proj_1");
    expect($<HTMLButtonElement>("#create-submit").disabled).toBe(false);
    expect($<HTMLInputElement>("#new-name").value).toBe("");
  });

  test("a backend error keeps the form values", async () => {
    await mount();
    $("#new-session").click();
    bridge.handlers.createSession = async () => {
      throw "directory does not exist: /nope";
    };
    type("#new-name", "proj");
    type("#new-directory", "/nope");
    submit("#create-form");
    await flush();
    expect($("#create-error").textContent).toContain("directory does not exist");
    expect($<HTMLInputElement>("#new-name").value).toBe("proj");
    expect($<HTMLInputElement>("#new-directory").value).toBe("/nope");
  });
});

describe("rename and end session", () => {
  test("rename validates, runs once and shows the new name", async () => {
    await mount();
    await openSession("$1");
    $("#rename-toggle").click();
    expect($<HTMLInputElement>("#rename-input").value).toBe("website");
    type("#rename-input", "bad:name");
    submit("#rename-form");
    await flush();
    expect($("#rename-error").textContent).toMatch(/letters, digits/i);
    expect(bridge.count("renameSession")).toBe(0);

    const pending = deferred<void>();
    bridge.handlers.renameSession = (id: string, name: string) => {
      bridge.sessions.find((s) => s.id === id)!.name = name;
      return pending.promise;
    };
    type("#rename-input", "site-2");
    submit("#rename-form");
    submit("#rename-form");
    await flush();
    expect(bridge.count("renameSession")).toBe(1);
    expect(bridge.calls.find((c) => c.method === "renameSession")?.args).toEqual(["$1", "site-2"]);
    pending.resolve();
    await flush();
    expect($("#detail-name").textContent).toBe("site-2");
    expect($<HTMLElement>("#rename-form").hidden).toBe(true);
  });

  test("refresh polling does not reset a rename in progress", async () => {
    await mount();
    await openSession("$1");
    app.setActive(true);
    await flush();
    $("#rename-toggle").click();
    type("#rename-input", "half-typed");
    timers.tick();
    await flush();
    expect($<HTMLInputElement>("#rename-input").value).toBe("half-typed");
    expect($<HTMLElement>("#rename-form").hidden).toBe(false);
  });

  test("ending a session needs confirmation and runs once", async () => {
    await mount();
    await openSession("$2");
    $("#end-toggle").click();
    expect(bridge.count("killSession")).toBe(0);
    expect($("#end-confirm").textContent).toMatch(/stops all processes/i);
    $("#cancel-end").click();
    expect($<HTMLElement>("#end-confirm").hidden).toBe(true);

    $("#end-toggle").click();
    const pending = deferred<void>();
    bridge.handlers.killSession = (id: string) => {
      bridge.sessions = bridge.sessions.filter((s) => s.id !== id);
      return pending.promise;
    };
    $("#confirm-end").click();
    $("#confirm-end").click();
    await flush();
    expect(bridge.count("killSession")).toBe(1);
    expect($<HTMLButtonElement>("#confirm-end").disabled).toBe(true);
    pending.resolve();
    await flush();
    expect(visibleView()).toBe("list");
    expect(sessionNames()).toEqual(["website"]);
  });

  test("a failed end shows the error and stays on the session", async () => {
    await mount();
    await openSession("$2");
    bridge.handlers.killSession = async () => {
      throw "permission denied";
    };
    $("#end-toggle").click();
    $("#confirm-end").click();
    await flush();
    expect(visibleView()).toBe("detail");
    expect($("#detail-error").textContent).toContain("permission denied");
    expect($<HTMLButtonElement>("#confirm-end").disabled).toBe(false);
  });
});

describe("terminal", () => {
  test("open in terminal calls the backend once and reports success", async () => {
    await mount();
    await openSession("$1");
    $("#open-terminal").click();
    $("#open-terminal").click();
    await flush();
    expect(bridge.count("openSession")).toBe(1);
    expect($("#detail-notice").textContent).toBe("Sent to your default terminal. If nothing appears, copy the attach command.");
  });

  test("an open failure explains and offers the attach command", async () => {
    await mount();
    await openSession("$1");
    bridge.handlers.openSession = async () => {
      throw "no handler for .command files";
    };
    $("#open-terminal").click();
    await flush();
    expect($("#detail-error").textContent).toContain("no handler for .command files");
    expect($("#attach-command").textContent).toBe("tmux attach-session -t '$1'");
  });

  test("copy attach command shows it and copies it", async () => {
    await mount();
    await openSession("$1");
    $("#copy-attach").click();
    await flush();
    expect($("#attach-command").textContent).toBe("tmux attach-session -t '$1'");
    expect(copied).toEqual(["tmux attach-session -t '$1'"]);
    expect($("#detail-notice").textContent).toMatch(/copied/i);
  });
});

describe("same-selection request ordering", () => {
  for (const outcome of ["success", "failure"] as const) {
    test(`an older details ${outcome} cannot replace a newer same-session result`, async () => {
      await mount();
      const slow = deferred<SessionDetails>();
      let calls = 0;
      const newer = detailsFor(session("$1", "newer-details"));
      bridge.handlers.sessionDetails = () => ++calls === 1 ? slow.promise : Promise.resolve(newer);
      await openSession("$1");
      await app.refresh();
      expect($("#detail-name").textContent).toBe("newer-details");
      if (outcome === "success") slow.resolve(detailsFor(session("$1", "older-details")));
      else slow.reject("older request failed");
      await flush();
      expect($("#detail-name").textContent).toBe("newer-details");
      expect($("#detail-error").textContent).toBe("");
    });

    test(`an older preview ${outcome} cannot replace newer output for the same pane`, async () => {
      await mount();
      const slow = deferred<string>();
      let calls = 0;
      bridge.handlers.capturePane = () => ++calls === 1 ? slow.promise : Promise.resolve("newer-pane-output");
      await openSession("$1");
      await app.refresh();
      expect($("#output").textContent).toBe("newer-pane-output");
      if (outcome === "success") slow.resolve("older-pane-output");
      else slow.reject("older capture failed");
      await flush();
      expect($("#output").textContent).toBe("newer-pane-output");
      expect($("#output-state").textContent).toBe("");
    });
  }
});

describe("mutation completion after navigation", () => {
  for (const operation of ["rename", "kill"] as const) {
    test(`${operation} updates the list even after selecting another session`, async () => {
      await mount();
      await openSession("$1");
      const slow = deferred<void>();
      if (operation === "rename") {
        bridge.handlers.renameSession = async (id: string, name: string) => {
          await slow.promise;
          bridge.sessions.find((s) => s.id === id)!.name = name;
        };
        $("#rename-toggle").click();
        type("#rename-input", "renamed-after-navigation");
        submit("#rename-form");
      } else {
        bridge.handlers.killSession = async (id: string) => {
          await slow.promise;
          bridge.sessions = bridge.sessions.filter((s) => s.id !== id);
        };
        $("#end-toggle").click();
        $("#confirm-end").click();
      }
      await flush();
      $("#back").click();
      await openSession("$2");
      slow.resolve();
      await flush();
      expect($("#detail-name").textContent).toBe("api");
      $("#back").click();
      expect(sessionNames()).toEqual(operation === "rename" ? ["renamed-after-navigation", "api"] : ["api"]);
    });
  }
});

describe("polling", () => {
  test("polls only while active and never overlaps refreshes", async () => {
    await mount();
    const initial = bridge.count("listSessions");
    timers.tick();
    await flush();
    expect(bridge.count("listSessions")).toBe(initial);

    app.setActive(true);
    await flush();
    expect(timers.active.size).toBe(1);
    const afterActivate = bridge.count("listSessions");
    expect(afterActivate).toBe(initial + 1);

    const slow = deferred<Session[]>();
    bridge.handlers.listSessions = () => slow.promise;
    timers.tick();
    timers.tick();
    timers.tick();
    await flush();
    expect(bridge.count("listSessions")).toBe(afterActivate + 1);
    slow.resolve(bridge.sessions);
    await flush();
    // Ticks during a run are skipped, not queued for later.
    expect(bridge.count("listSessions")).toBe(afterActivate + 1);

    app.setActive(false);
    expect(timers.active.size).toBe(0);
  });
});

describe("keyboard", () => {
  test("Escape asks the native side to hide the popover", async () => {
    await mount();
    document.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }) as unknown as Event);
    await flush();
    expect(bridge.count("hidePopover")).toBe(1);
  });

  test("other keys do not hide the popover", async () => {
    await mount();
    document.dispatchEvent(new window.KeyboardEvent("keydown", { key: "a", bubbles: true }) as unknown as Event);
    await flush();
    expect(bridge.count("hidePopover")).toBe(0);
  });
});

describe("placeholders", () => {
  test("missing metadata uses a plain hyphen, not a long dash", async () => {
    await mount();
    const slow = deferred<SessionDetails>();
    bridge.handlers.sessionDetails = () => slow.promise;
    await openSession("$1");
    expect($("#directory").textContent).toBe("-");
  });
});

describe("without Tauri", () => {
  test("explains that the desktop app is required", () => {
    renderUnavailable(root);
    expect(root.textContent).toMatch(/desktop app/i);
    expect(root.querySelector("#session-list")).toBeNull();
  });
});

describe("settings entry point", () => {
  test("the footer Settings action works from the list, details and create views", async () => {
    await mount();
    const footer = $("#open-settings");
    expect(footer.closest("[hidden]")).toBeNull();
    footer.click();
    await openSession("$1");
    expect(visibleView()).toBe("detail");
    expect($("#open-settings").closest("[hidden]")).toBeNull();
    $("#open-settings").click();
    $("#back").click();
    $("#new-session").click();
    expect(visibleView()).toBe("create");
    expect($("#open-settings").closest("[hidden]")).toBeNull();
    $("#open-settings").click();
    await flush();
    expect(bridge.count("openSettings")).toBe(3);
  });

  test("a failure to open settings is shown, not swallowed", async () => {
    bridge.handlers.openSettings = async () => {
      throw "Could not open settings: no display";
    };
    await mount();
    $("#open-settings").click();
    await flush();
    expect($("#footer-error").textContent).toContain("no display");
  });
});

describe("preferences in the popover", () => {
  const prefs = (extra: Partial<Preferences>): Preferences => ({ ...DEFAULT_PREFERENCES, ...extra });

  test("polls at the saved interval and switches without overlapping timers", async () => {
    bridge.preferences = prefs({ refreshIntervalSeconds: 10 });
    await mount();
    app.setActive(true);
    await flush();
    expect(timers.activeDelays()).toEqual([10_000]);
    app.applyPreferences(prefs({ refreshIntervalSeconds: 2 }));
    expect(timers.activeDelays()).toEqual([2_000]);
    app.setActive(false);
    app.applyPreferences(prefs({ refreshIntervalSeconds: 4 }));
    expect(timers.activeDelays()).toEqual([]);
    app.setActive(true);
    await flush();
    expect(timers.activeDelays()).toEqual([4_000]);
  });

  test("applies the theme live and returns to system", async () => {
    await mount();
    app.applyPreferences(prefs({ theme: "dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    app.applyPreferences(prefs({ theme: "light" }));
    expect(document.documentElement.dataset.theme).toBe("light");
    app.applyPreferences(prefs({ theme: "system" }));
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });

  test("new sessions start in the saved default directory", async () => {
    bridge.preferences = prefs({ defaultDirectory: "~/code" });
    await mount();
    $("#new-session").click();
    expect($<HTMLInputElement>("#new-directory").value).toBe("~/code");
    type("#new-name", "proj");
    submit("#create-form");
    await flush();
    expect(bridge.calls.find((c) => c.method === "createSession")?.args).toEqual(["proj", "~/code"]);
  });

  test("a preferences change does not replace a create form in progress", async () => {
    await mount();
    $("#new-session").click();
    type("#new-directory", "/tmp/mine");
    app.applyPreferences(prefs({ defaultDirectory: "~/other" }));
    expect($<HTMLInputElement>("#new-directory").value).toBe("/tmp/mine");
    $("#back-create").click();
    $("#new-session").click();
    expect($<HTMLInputElement>("#new-directory").value).toBe("~/other");
  });

  test("a preferences load failure is visible and falls back to defaults", async () => {
    bridge.handlers.getPreferences = async () => {
      throw "Settings file /x/preferences.json is malformed (EOF). Fix or remove it.";
    };
    await mount();
    expect($("#footer-error").textContent).toContain("Could not load settings");
    expect($("#footer-error").textContent).toContain("malformed");
    $("#new-session").click();
    expect($<HTMLInputElement>("#new-directory").value).toBe("~");
    // The interval stays at what the app started with (1000 ms in this harness).
    app.setActive(true);
    expect(timers.activeDelays()).toEqual([1_000]);
  });

  test("a change event that arrives first wins over a late initial load", async () => {
    const slow = deferred<Preferences>();
    bridge.handlers.getPreferences = () => slow.promise;
    app = createApp({ root, bridge, timers, copyText: async () => {} });
    void app.start();
    await flush();
    app.applyPreferences(prefs({ theme: "dark", defaultDirectory: "~/new" }));
    slow.resolve(prefs({ theme: "light", defaultDirectory: "~/old" }));
    await flush();
    expect(document.documentElement.dataset.theme).toBe("dark");
    $("#new-session").click();
    expect($<HTMLInputElement>("#new-directory").value).toBe("~/new");
  });

  test("a successful change clears an earlier load error", async () => {
    bridge.handlers.getPreferences = async () => {
      throw "boom";
    };
    await mount();
    expect($("#footer-error").textContent).toContain("boom");
    app.applyPreferences(prefs({}));
    expect($("#footer-error").textContent).toBe("");
  });
});
