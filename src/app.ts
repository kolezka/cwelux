import type { TmuxBridge } from "./api";
import { applyTheme, DEFAULT_PREFERENCES, type Preferences } from "./preferences";
import {
  createEpoch,
  createSerialRunner,
  errorMessage,
  filterSessions,
  isMissingTmuxError,
  reconcileSelection,
  validateDirectory,
  validateSessionName,
} from "./state";
import type { Pane, Session, SessionDetails, TmuxWindow } from "./types";

export interface Timers {
  setInterval(fn: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
}

export interface AppOptions {
  root: HTMLElement;
  bridge: TmuxBridge;
  timers?: Timers;
  pollIntervalMs?: number;
  copyText?: (text: string) => Promise<void>;
}

export interface AppHandle {
  /** Builds the UI and loads sessions once. */
  start(): Promise<void>;
  /** Refreshes the list and, on the details view, the selected session. */
  refresh(): Promise<void>;
  /** Polling runs only while the popover is visible and focused. */
  setActive(active: boolean): void;
  /** Applies saved preferences: theme, polling interval, default directory. */
  applyPreferences(preferences: Preferences): void;
}

type View = "list" | "detail" | "create";
type ListState = "loading" | "ready" | "empty" | "no-match" | "error" | "missing-tmux";
type Mutation = "create" | "rename" | "kill" | "open" | "attach";

type Child = Node | string;

export function builder(doc: Document) {
  return function el<K extends keyof HTMLElementTagNameMap>(
    tag: K,
    props: Record<string, string | boolean> = {},
    ...children: Child[]
  ): HTMLElementTagNameMap[K] {
    const node = doc.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key === "text") node.textContent = String(value);
      else if (key === "hidden") node.hidden = Boolean(value);
      else if (typeof value === "boolean") {
        if (value) node.setAttribute(key, "");
      } else node.setAttribute(key, value);
    }
    node.append(...children);
    return node;
  };
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

function formatCreated(seconds: number, now = new Date()): string {
  const date = new Date(seconds * 1000);
  const time = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (date.toDateString() === now.toDateString()) return `Today, ${time}`;
  return `${date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" })}, ${time}`;
}

export function renderUnavailable(root: HTMLElement): void {
  const el = builder(root.ownerDocument);
  root.replaceChildren(
    el(
      "section",
      { class: "unavailable", "data-view": "unavailable" },
      el("span", { class: "tray-mark", text: ">_", "aria-hidden": "true" }),
      el("h2", { text: "Desktop app required" }),
      el("p", {
        text: "Cwelux reads tmux sessions through its desktop app. This page is open in a plain browser, so it cannot reach tmux and shows no sessions.",
      }),
      el("p", { class: "mono", text: "bun run tauri dev" }),
    ),
  );
}

export function createApp(options: AppOptions): AppHandle {
  const { root, bridge } = options;
  const timers: Timers = options.timers ?? {
    setInterval: (fn, ms) => globalThis.setInterval(fn, ms),
    clearInterval: (handle) => globalThis.clearInterval(handle as ReturnType<typeof setInterval>),
  };
  let pollIntervalMs = options.pollIntervalMs ?? DEFAULT_PREFERENCES.refreshIntervalSeconds * 1000;
  let defaultDirectory = DEFAULT_PREFERENCES.defaultDirectory;
  // Bumped by every applied change, so a slow initial load cannot undo a newer one.
  let preferencesVersion = 0;
  const copyText = options.copyText ?? ((text: string) => navigator.clipboard.writeText(text));
  const el = builder(root.ownerDocument);

  // ---- State ----
  let view: View = "list";
  let sessions: Session[] | null = null;
  let listError: string | null = null;
  let lastUpdated: Date | null = null;
  let selectedId: string | null = null;
  let details: SessionDetails | null = null;
  let detailError: string | null = null;
  let selection: { windowId: string | null; paneId: string | null } = { windowId: null, paneId: null };
  const pending = new Set<Mutation>();
  const epoch = createEpoch();
  const detailRequests = createEpoch();
  const previewRequests = createEpoch();
  let token = epoch.next();
  let pollHandle: unknown = null;

  // ---- Skeleton ----
  const refreshButton = el("button", { class: "icon", id: "refresh", type: "button", "aria-label": "Refresh", title: "Refresh", text: "↻" });
  const sessionCount = el("span", { class: "count", id: "session-count" });
  const search = el("input", {
    id: "search", class: "search", type: "search", placeholder: "Find a session…",
    "aria-label": "Find a session", autocomplete: "off", spellcheck: "false",
  });
  const updated = el("span", { id: "updated" });
  const listStateTitle = el("strong");
  const listStateDetail = el("span", { class: "state-detail" });
  const listState = el("div", { id: "list-state", class: "state", role: "status", "aria-live": "polite" }, listStateTitle, listStateDetail);
  const retry = el("button", { id: "retry", class: "quiet", type: "button", text: "Try again", hidden: true });
  const sessionList = el("div", { id: "session-list" });
  const newSession = el("button", { id: "new-session", class: "primary wide", type: "button", text: "+ New session" });

  const openSettings = el("button", { class: "quiet", id: "open-settings", type: "button", text: "Settings" });
  const footerError = el("p", { class: "error footer-error", id: "footer-error", role: "alert" });
  const footer = el("footer", { class: "footer" },
    el("div", { class: "footer-row" },
      el("span", { text: "Sessions keep running when you close this menu." }),
      openSettings),
    footerError);

  const listView = el(
    "section",
    { "data-view": "list" },
    el("header", { class: "bar" },
      el("div", { class: "bar-left" }, el("h2", { text: "Sessions" }), sessionCount),
      el("div", { class: "bar-right" }, el("span", { class: "status", text: "Local tmux" }), refreshButton)),
    el("div", { class: "body" },
      search,
      el("div", { class: "section-label" }, el("span", { text: "ON THIS MAC" }), updated),
      listState, retry, sessionList, newSession),
  );

  const back = el("button", { class: "icon", id: "back", type: "button", "aria-label": "Back to sessions", text: "‹" });
  const detailName = el("h2", { class: "mono", id: "detail-name" });
  const detailStatus = el("span", { class: "status", id: "detail-status" });
  const detailRefresh = el("button", { class: "icon", id: "detail-refresh", type: "button", "aria-label": "Refresh", title: "Refresh", text: "↻" });
  const created = el("dd", { id: "created" });
  const clients = el("dd", { id: "clients" });
  const directory = el("dd", { id: "directory" });
  const windowCount = el("span", { id: "window-count" });
  const windowTabs = el("div", { class: "tabs", id: "window-tabs", role: "tablist", "aria-label": "Session windows" });
  const panes = el("div", { class: "group", id: "panes", "aria-label": "Panes" });
  const output = el("pre", { class: "output", id: "output", "aria-label": "Pane output preview" });
  const outputState = el("p", { class: "notice", id: "output-state", role: "status" });
  const openTerminal = el("button", { class: "primary", id: "open-terminal", type: "button", text: "Open in terminal ↗" });
  const copyAttach = el("button", { class: "quiet", id: "copy-attach", type: "button", text: "Copy attach command" });
  const renameToggle = el("button", { class: "quiet", id: "rename-toggle", type: "button", text: "Rename" });
  const endToggle = el("button", { class: "quiet danger", id: "end-toggle", type: "button", text: "End session" });
  const attachCommand = el("code", { id: "attach-command", class: "command" });
  const attachBox = el("div", { id: "attach-box", class: "attach", hidden: true },
    el("span", { text: "Run in any terminal:" }), attachCommand);
  const renameInput = el("input", { id: "rename-input", maxlength: "64", autocomplete: "off", spellcheck: "false" });
  const renameError = el("p", { class: "error", id: "rename-error", role: "alert" });
  const renameSubmit = el("button", { class: "primary", id: "rename-submit", type: "submit", text: "Save name" });
  const renameCancel = el("button", { class: "quiet", id: "rename-cancel", type: "button", text: "Cancel" });
  const renameForm = el("form", { id: "rename-form", hidden: true, novalidate: true },
    el("label", { class: "field" }, "Session name", renameInput),
    renameError,
    el("div", { class: "actions" }, renameSubmit, renameCancel));
  const endName = el("strong", { class: "mono" });
  const confirmEnd = el("button", { class: "quiet danger", id: "confirm-end", type: "button", text: "End session" });
  const cancelEnd = el("button", { class: "quiet", id: "cancel-end", type: "button", text: "Cancel" });
  const endConfirm = el("div", { class: "confirmation", id: "end-confirm", hidden: true, role: "alertdialog", "aria-label": "Confirm ending session" },
    el("p", {}, "End ", endName, "? This stops all processes running in its panes."),
    el("div", { class: "actions tight" }, confirmEnd, cancelEnd));
  const detailError_ = el("p", { class: "error", id: "detail-error", role: "alert" });
  const detailNotice = el("p", { class: "notice", id: "detail-notice", role: "status", "aria-live": "polite" });

  const detailView = el(
    "section",
    { "data-view": "detail", hidden: true },
    el("header", { class: "bar" },
      el("div", { class: "bar-left" }, back, detailName),
      el("div", { class: "bar-right" }, detailStatus, detailRefresh)),
    el("div", { class: "body" },
      el("dl", { class: "metadata" },
        el("div", {}, el("dt", { text: "CREATED" }), created),
        el("div", {}, el("dt", { text: "CLIENTS" }), clients),
        el("div", { class: "path" }, el("dt", { text: "WORKING DIRECTORY" }), directory)),
      el("div", { class: "section-label" }, el("span", { text: "WINDOWS" }), windowCount),
      windowTabs, panes,
      el("div", { class: "section-label" }, el("span", { text: "OUTPUT PREVIEW" }), el("span", { text: "Read-only · last 200 lines" })),
      output, outputState,
      el("div", { class: "actions" }, openTerminal, copyAttach, renameToggle, endToggle),
      attachBox, renameForm, endConfirm, detailError_, detailNotice),
  );

  const backCreate = el("button", { class: "icon", id: "back-create", type: "button", "aria-label": "Back to sessions", text: "‹" });
  const newName = el("input", { id: "new-name", placeholder: "my-project", maxlength: "64", autocomplete: "off", spellcheck: "false" });
  const newDirectory = el("input", { id: "new-directory", autocomplete: "off", spellcheck: "false" });
  newDirectory.value = defaultDirectory;
  const createError = el("p", { class: "error", id: "create-error", role: "alert" });
  const createSubmit = el("button", { class: "primary wide", id: "create-submit", type: "submit", text: "Create session" });
  const createForm = el("form", { class: "body", id: "create-form", novalidate: true },
    el("label", { class: "field" }, "Name", newName),
    el("p", { class: "hint", text: "Letters, digits, underscores and hyphens. Up to 64 characters." }),
    el("label", { class: "field" }, "Starting directory", newDirectory),
    el("p", { class: "notice", text: "Starts in the background. You can open it in your terminal later." }),
    createSubmit, createError);
  const createView = el("section", { "data-view": "create", hidden: true },
    el("header", { class: "bar" }, el("div", { class: "bar-left" }, backCreate, el("h2", { text: "New session" }))),
    createForm);

  // ---- Rendering ----
  function showView(next: View) {
    view = next;
    for (const section of [listView, detailView, createView]) section.hidden = section.dataset.view !== next;
  }

  function currentListState(): ListState {
    if (listError !== null) return isMissingTmuxError(listError) ? "missing-tmux" : "error";
    if (sessions === null) return "loading";
    if (sessions.length === 0) return "empty";
    return filterSessions(sessions, search.value).length ? "ready" : "no-match";
  }

  function renderList() {
    const state = currentListState();
    listState.dataset.state = state;
    listState.hidden = state === "ready";
    retry.hidden = state !== "error" && state !== "missing-tmux";
    sessionCount.textContent = sessions ? String(sessions.length) : "";
    updated.textContent = lastUpdated
      ? `Updated ${lastUpdated.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" })}`
      : "";
    const messages: Record<ListState, [string, string]> = {
      loading: ["Loading sessions…", ""],
      ready: ["", ""],
      empty: ["No tmux sessions running.", "Start one below."],
      "no-match": [`No sessions match “${search.value.trim()}”.`, ""],
      error: ["Could not list tmux sessions.", listError ?? ""],
      "missing-tmux": [
        "tmux not found.",
        `Install it with Homebrew (brew install tmux), or set CWELUX_TMUX_PATH to the tmux executable. Details: ${listError ?? ""}`,
      ],
    };
    const [title, detail] = messages[state];
    listStateTitle.textContent = title;
    listStateDetail.textContent = detail;
    listState.classList.toggle("is-error", state === "error" || state === "missing-tmux");

    const visible = sessions ? filterSessions(sessions, search.value) : [];
    const rows = visible.map((s) =>
      el("button", { class: "session", type: "button", "data-session-id": s.id },
        el("span", { class: "session-icon", text: ">_", "aria-hidden": "true" }),
        el("span", { class: "session-copy" },
          el("span", { class: "session-name", text: s.name }),
          el("span", {
            class: "session-meta",
            text: `${plural(s.windows, "window")} · ${s.attached ? `${plural(s.attached, "client")} attached` : "Detached"}`,
          })),
        ...(s.attached > 0 ? [el("span", { class: "dot", "aria-hidden": "true" })] : []),
        el("span", { class: "arrow", text: "›", "aria-hidden": "true" })));
    sessionList.replaceChildren(...(rows.length ? [el("div", { class: "group" }, ...rows)] : []));
  }

  function selectedSession(): Session | null {
    if (details && details.session.id === selectedId) return details.session;
    return sessions?.find((s) => s.id === selectedId) ?? null;
  }

  function selectedWindow(): TmuxWindow | null {
    return details?.windows.find((w) => w.id === selection.windowId) ?? null;
  }

  function selectedPane(): Pane | null {
    return selectedWindow()?.panes.find((p) => p.id === selection.paneId) ?? null;
  }

  function renderDetail() {
    const s = selectedSession();
    detailName.textContent = s?.name ?? "";
    endName.textContent = s?.name ?? "";
    detailStatus.textContent = s ? (s.attached ? "Attached" : "Detached") : "";
    created.textContent = s ? formatCreated(s.created) : "-";
    clients.textContent = s ? (s.attached ? `${s.attached} attached` : "None") : "-";
    directory.textContent = selectedPane()?.path ?? "-";

    const windows = details?.windows ?? [];
    windowCount.textContent = details ? plural(windows.length, "window") : "";
    windowTabs.replaceChildren(
      ...windows.map((w) =>
        el("button", {
          class: "tab", type: "button", role: "tab", "data-window-id": w.id,
          "aria-selected": String(w.id === selection.windowId), text: `${w.index}: ${w.name}`,
        })),
    );
    panes.replaceChildren(
      ...(selectedWindow()?.panes ?? []).map((p) =>
        el("button", { class: "pane", type: "button", "data-pane-id": p.id, "aria-pressed": String(p.id === selection.paneId) },
          el("span", { class: "pane-copy" },
            el("strong", { class: "mono", text: `Pane ${p.index} · ${p.command}` }),
            el("span", { class: "pane-path", text: p.path })),
          el("span", { class: "pane-size", text: `${p.width} × ${p.height}` }))),
    );

    detailError_.textContent = detailError ?? "";
    detailError_.hidden = !detailError;
    renderButtons();
  }

  function renderButtons() {
    const busy = pending.has("kill") || pending.has("rename");
    const usable = details !== null && !busy;
    openTerminal.disabled = !usable || pending.has("open");
    copyAttach.disabled = !usable || pending.has("attach");
    renameToggle.disabled = !usable;
    endToggle.disabled = !usable;
    renameSubmit.disabled = pending.has("rename");
    confirmEnd.disabled = pending.has("kill");
    createSubmit.disabled = pending.has("create");
    createSubmit.textContent = pending.has("create") ? "Creating…" : "Create session";
    renameSubmit.textContent = pending.has("rename") ? "Saving…" : "Save name";
    confirmEnd.textContent = pending.has("kill") ? "Ending…" : "End session";
  }

  function clearPreview(message: string) {
    output.textContent = "";
    outputState.textContent = message;
  }

  function setNotice(text: string) {
    detailNotice.textContent = text;
  }

  // ---- Loading ----
  async function loadPreview(): Promise<void> {
    const mine = token;
    const request = previewRequests.next();
    const sessionId = selectedId;
    const paneId = selection.paneId;
    if (!sessionId || !paneId) {
      clearPreview(details ? "This window has no panes." : "");
      return;
    }
    try {
      const text = await bridge.capturePane(sessionId, paneId);
      if (!epoch.isCurrent(mine) || !previewRequests.isCurrent(request)) return;
      output.textContent = text;
      outputState.textContent = text ? "" : "The pane shows no output.";
    } catch (error) {
      if (!epoch.isCurrent(mine) || !previewRequests.isCurrent(request)) return;
      clearPreview(`Could not read this pane: ${errorMessage(error)}`);
    }
  }

  async function loadDetail(): Promise<void> {
    const mine = token;
    // Selection alone cannot order overlapping refreshes of the same session.
    const request = detailRequests.next();
    previewRequests.next();
    const sessionId = selectedId;
    if (!sessionId) return;
    try {
      const next = await bridge.sessionDetails(sessionId);
      if (!epoch.isCurrent(mine) || !detailRequests.isCurrent(request)) return;
      details = next;
      detailError = null;
      const previousPane = selection.paneId;
      const reconciled = reconcileSelection(next, selection);
      selection = reconciled ?? { windowId: null, paneId: null };
      if (selection.paneId !== previousPane) clearPreview("Loading output…");
      renderDetail();
    } catch (error) {
      if (!epoch.isCurrent(mine) || !detailRequests.isCurrent(request)) return;
      details = null;
      detailError = errorMessage(error);
      clearPreview("");
      renderDetail();
      return;
    }
    await loadPreview();
  }

  async function refreshTask(): Promise<void> {
    try {
      sessions = await bridge.listSessions();
      listError = null;
      lastUpdated = new Date();
    } catch (error) {
      sessions = null;
      listError = errorMessage(error);
    }
    renderList();

    if (view !== "detail" || !selectedId) return;
    if (sessions && !sessions.some((s) => s.id === selectedId)) {
      token = epoch.next();
      details = null;
      detailError = "This session is no longer running.";
      clearPreview("");
      renderDetail();
      return;
    }
    await loadDetail();
  }

  const runner = createSerialRunner(refreshTask);

  function changeSelection(sessionId: string | null) {
    token = epoch.next();
    selectedId = sessionId;
    details = null;
    detailError = null;
    selection = { windowId: null, paneId: null };
    renameForm.hidden = true;
    endConfirm.hidden = true;
    attachBox.hidden = true;
    attachCommand.textContent = "";
    setNotice("");
    clearPreview(sessionId ? "Loading output…" : "");
  }

  function openDetails(sessionId: string) {
    changeSelection(sessionId);
    showView("detail");
    renderDetail();
    void loadDetail();
  }

  function selectWithin(windowId: string, paneId: string | null) {
    if (!details) return;
    token = epoch.next();
    selection = reconcileSelection(details, { windowId, paneId }) ?? { windowId: null, paneId: null };
    clearPreview("Loading output…");
    renderDetail();
    void loadPreview();
  }

  async function mutate<T>(kind: Mutation, action: () => Promise<T>): Promise<{ ok: true; value: T } | { ok: false; error: string }> {
    pending.add(kind);
    renderButtons();
    try {
      return { ok: true, value: await action() };
    } catch (error) {
      return { ok: false, error: errorMessage(error) };
    } finally {
      pending.delete(kind);
      renderButtons();
    }
  }

  async function showAttachCommand(sessionId: string): Promise<string | null> {
    const result = await mutate("attach", () => bridge.attachCommand(sessionId));
    if (selectedId !== sessionId) return null;
    if (!result.ok) {
      detailError = `Could not build the attach command: ${result.error}`;
      renderDetail();
      return null;
    }
    attachCommand.textContent = result.value;
    attachBox.hidden = false;
    return result.value;
  }

  // ---- Events ----
  search.addEventListener("input", renderList);
  refreshButton.addEventListener("click", () => void runner.request());
  detailRefresh.addEventListener("click", () => void runner.request());
  retry.addEventListener("click", () => {
    listError = null;
    sessions = null;
    renderList();
    void runner.request();
  });
  sessionList.addEventListener("click", (event) => {
    const target = (event.target as Element).closest<HTMLElement>("[data-session-id]");
    if (target?.dataset.sessionId) openDetails(target.dataset.sessionId);
  });
  newSession.addEventListener("click", () => {
    createError.textContent = "";
    // A fresh form starts in the saved directory. Changes while it is open leave it alone.
    newDirectory.value = defaultDirectory;
    showView("create");
    newName.focus();
  });
  const backToList = () => {
    changeSelection(null);
    showView("list");
    renderList();
  };
  back.addEventListener("click", backToList);
  backCreate.addEventListener("click", backToList);

  windowTabs.addEventListener("click", (event) => {
    const target = (event.target as Element).closest<HTMLElement>("[data-window-id]");
    const windowId = target?.dataset.windowId;
    if (windowId && windowId !== selection.windowId) selectWithin(windowId, null);
  });
  panes.addEventListener("click", (event) => {
    const target = (event.target as Element).closest<HTMLElement>("[data-pane-id]");
    const paneId = target?.dataset.paneId;
    if (paneId && selection.windowId && paneId !== selection.paneId) selectWithin(selection.windowId, paneId);
  });

  createForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (pending.has("create")) return;
    const name = newName.value.trim();
    const dir = newDirectory.value.trim();
    const problem = validateSessionName(name) ?? validateDirectory(dir);
    createError.textContent = problem ?? "";
    if (problem) return;
    const result = await mutate("create", () => bridge.createSession(name, dir));
    if (!result.ok) {
      createError.textContent = `Could not create the session: ${result.error}`;
      return;
    }
    newName.value = "";
    createError.textContent = "";
    sessions = sessions ? [...sessions.filter((s) => s.id !== result.value.id), result.value] : [result.value];
    if (view === "create") {
      openDetails(result.value.id);
      setNotice(`Session ${result.value.name} started.`);
    }
    void runner.request();
  });

  openTerminal.addEventListener("click", async () => {
    const sessionId = selectedId;
    if (!sessionId || pending.has("open")) return;
    setNotice("");
    const result = await mutate("open", () => bridge.openSession(sessionId));
    if (selectedId !== sessionId) return;
    if (result.ok) {
      // Success only means Launch Services accepted the request.
      setNotice("Sent to your default terminal. If nothing appears, copy the attach command.");
      return;
    }
    detailError = `Could not open a terminal: ${result.error}`;
    renderDetail();
    await showAttachCommand(sessionId);
  });

  copyAttach.addEventListener("click", async () => {
    const sessionId = selectedId;
    if (!sessionId || pending.has("attach")) return;
    setNotice("");
    const command = await showAttachCommand(sessionId);
    if (!command) return;
    try {
      await copyText(command);
      setNotice("Attach command copied.");
    } catch {
      setNotice("Copying failed. Select the command above and copy it.");
    }
  });

  renameToggle.addEventListener("click", () => {
    endConfirm.hidden = true;
    renameError.textContent = "";
    renameInput.value = selectedSession()?.name ?? "";
    renameForm.hidden = false;
    renameInput.focus();
  });
  renameCancel.addEventListener("click", () => {
    renameForm.hidden = true;
  });
  renameForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const sessionId = selectedId;
    if (!sessionId || pending.has("rename")) return;
    const name = renameInput.value.trim();
    const problem = validateSessionName(name);
    renameError.textContent = problem ?? "";
    if (problem) return;
    const result = await mutate("rename", () => bridge.renameSession(sessionId, name));
    if (!result.ok) {
      if (selectedId === sessionId) renameError.textContent = `Could not rename: ${result.error}`;
      return;
    }
    // The shared list must change even if the user has navigated elsewhere.
    sessions = sessions?.map((session) => session.id === sessionId ? { ...session, name } : session) ?? null;
    renderList();
    if (selectedId === sessionId) {
      renameForm.hidden = true;
      setNotice(`Renamed to ${name}.`);
    }
    await runner.request();
  });

  endToggle.addEventListener("click", () => {
    renameForm.hidden = true;
    endConfirm.hidden = false;
  });
  cancelEnd.addEventListener("click", () => {
    endConfirm.hidden = true;
  });
  confirmEnd.addEventListener("click", async () => {
    const sessionId = selectedId;
    if (!sessionId || pending.has("kill")) return;
    const result = await mutate("kill", () => bridge.killSession(sessionId));
    if (!result.ok) {
      if (selectedId === sessionId) {
        detailError = `Could not end the session: ${result.error}`;
        renderDetail();
      }
      return;
    }
    sessions = sessions?.filter((s) => s.id !== sessionId) ?? null;
    if (selectedId === sessionId) backToList();
    else renderList();
    await runner.request();
  });

  openSettings.addEventListener("click", () => {
    footerError.textContent = "";
    bridge.openSettings().catch((error) => {
      footerError.textContent = `Could not open settings: ${errorMessage(error)}`;
    });
  });

  function startPolling() {
    pollHandle = timers.setInterval(() => void runner.requestIfIdle(), pollIntervalMs);
  }

  function applyPreferences(next: Preferences) {
    preferencesVersion++;
    footerError.textContent = "";
    applyTheme(root.ownerDocument, next.theme);
    defaultDirectory = next.defaultDirectory;
    if (view !== "create") newDirectory.value = defaultDirectory;
    const nextMs = next.refreshIntervalSeconds * 1000;
    if (nextMs !== pollIntervalMs) {
      pollIntervalMs = nextMs;
      if (pollHandle !== null) {
        timers.clearInterval(pollHandle);
        startPolling();
      }
    }
  }

  async function loadPreferences() {
    const version = preferencesVersion;
    try {
      const loaded = await bridge.getPreferences();
      if (version === preferencesVersion) applyPreferences(loaded);
    } catch (error) {
      if (version === preferencesVersion) footerError.textContent = `Could not load settings: ${errorMessage(error)}`;
    }
  }

  root.ownerDocument.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    bridge.hidePopover().catch((error) => console.error("hide_popover failed:", errorMessage(error)));
  });

  // ---- Public handle ----
  return {
    async start() {
      root.replaceChildren(listView, detailView, createView, footer);
      showView("list");
      renderList();
      await Promise.all([loadPreferences(), runner.request()]);
    },
    refresh: () => runner.request(),
    setActive(active) {
      if (active && pollHandle === null) {
        startPolling();
        void runner.requestIfIdle();
      } else if (!active && pollHandle !== null) {
        timers.clearInterval(pollHandle);
        pollHandle = null;
      }
    },
    applyPreferences,
  };
}
