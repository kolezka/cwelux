import type { SettingsBridge } from "./api";
import { builder } from "./app";
import {
  applyTheme,
  REFRESH_INTERVALS,
  THEMES,
  type Preferences,
  type RefreshInterval,
  type Theme,
} from "./preferences";
import { errorMessage, validateDirectory } from "./state";

export interface SettingsOptions {
  root: HTMLElement;
  bridge: SettingsBridge;
}

export interface SettingsHandle {
  /** Builds the form and loads preferences and the login item state. */
  start(): Promise<void>;
  /** Called on preferences-changed. */
  applyPreferences(preferences: Preferences): void;
}

const THEME_LABELS: Record<Theme, string> = { system: "Match system", light: "Light", dark: "Dark" };

export function createSettingsApp({ root, bridge }: SettingsOptions): SettingsHandle {
  const el = builder(root.ownerDocument);
  const doc = root.ownerDocument;

  let loaded = false;
  let saving = false;
  let loginBusy = false;
  let loginKnown = false;

  const loginBox = el("input", { id: "launch-at-login", type: "checkbox", disabled: true });
  const loginError = el("p", { class: "error", id: "login-error", role: "alert" });
  const directory = el("input", { id: "default-directory", autocomplete: "off", spellcheck: "false", disabled: true });
  const choose = el("button", { class: "quiet", id: "choose-directory", type: "button", text: "Choose…", disabled: true });
  const interval = el("select", { id: "refresh-interval", disabled: true },
    ...REFRESH_INTERVALS.map((s) => el("option", { value: String(s), text: `Every ${s} seconds` })));
  const theme = el("select", { id: "theme", disabled: true },
    ...THEMES.map((t) => el("option", { value: t, text: THEME_LABELS[t] })));
  const save = el("button", { class: "primary", id: "save-settings", type: "submit", text: "Save", disabled: true });
  const formError = el("p", { class: "error", id: "settings-error", role: "alert" });
  const notice = el("p", { class: "notice", id: "settings-notice", role: "status", "aria-live": "polite" });
  const form = el("form", { id: "settings-form", novalidate: true },
    el("label", { class: "field" }, "Default starting directory",
      el("div", { class: "field-row" }, directory, choose)),
    el("p", { class: "hint", text: "Used for new sessions. ~ is your home folder." }),
    el("label", { class: "field" }, "Refresh interval", interval),
    el("label", { class: "field" }, "Theme", theme),
    el("div", { class: "actions" }, save),
    formError, notice);

  const view = el("section", { class: "settings", "data-view": "settings" },
    el("header", { class: "bar" }, el("div", { class: "bar-left" }, el("h2", { text: "Settings" }))),
    el("div", { class: "body" },
      el("label", { class: "toggle" }, loginBox, el("span", { text: "Launch at login" })),
      el("p", { class: "hint", id: "login-hint", text: "Applies immediately. Not part of Save." }),
      loginError,
      form));

  function renderControls() {
    for (const control of [directory, choose, interval, theme]) control.disabled = !loaded || saving;
    save.disabled = !loaded || saving;
    save.textContent = saving ? "Saving…" : "Save";
    loginBox.disabled = !loginKnown || loginBusy;
  }

  function fill(prefs: Preferences) {
    directory.value = prefs.defaultDirectory;
    interval.value = String(prefs.refreshIntervalSeconds);
    theme.value = prefs.theme;
  }

  function showLogin(enabled: boolean | null) {
    loginKnown = enabled !== null;
    // Unknown is shown as off: never claim enabled without the OS saying so.
    loginBox.checked = enabled === true;
    renderControls();
  }

  async function loadPreferences() {
    try {
      const prefs = await bridge.getPreferences();
      fill(prefs);
      applyTheme(doc, prefs.theme);
      loaded = true;
    } catch (error) {
      formError.textContent = `Could not load settings: ${errorMessage(error)}`;
    }
    renderControls();
  }

  async function loadLogin(): Promise<void> {
    try {
      showLogin(await bridge.getLaunchAtLogin());
    } catch (error) {
      showLogin(null);
      loginError.textContent = errorMessage(error);
    }
  }

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!loaded || saving) return;
    notice.textContent = "";
    const dir = directory.value.trim();
    const problem = validateDirectory(dir);
    formError.textContent = problem ?? "";
    if (problem) return;
    const wanted: Preferences = {
      defaultDirectory: dir,
      refreshIntervalSeconds: Number(interval.value) as RefreshInterval,
      theme: theme.value as Theme,
    };
    saving = true;
    renderControls();
    try {
      const saved = await bridge.savePreferences(wanted);
      fill(saved);
      applyTheme(doc, saved.theme);
      notice.textContent = "Saved.";
    } catch (error) {
      formError.textContent = `Could not save settings: ${errorMessage(error)}`;
    } finally {
      saving = false;
      renderControls();
    }
  });

  choose.addEventListener("click", async () => {
    formError.textContent = "";
    try {
      const picked = await bridge.chooseDirectory();
      if (picked !== null) directory.value = picked;
    } catch (error) {
      formError.textContent = `Could not open the folder picker: ${errorMessage(error)}`;
    }
  });

  loginBox.addEventListener("change", async () => {
    if (loginBusy || !loginKnown) return;
    const wanted = loginBox.checked;
    loginBusy = true;
    loginError.textContent = "";
    renderControls();
    try {
      showLogin(await bridge.setLaunchAtLogin(wanted));
    } catch (error) {
      const message = errorMessage(error);
      try {
        showLogin(await bridge.getLaunchAtLogin());
        loginError.textContent = message;
      } catch (readError) {
        showLogin(null);
        loginError.textContent = `${message} The current state is unknown: ${errorMessage(readError)}`;
      }
    } finally {
      loginBusy = false;
      renderControls();
    }
  });

  return {
    async start() {
      root.replaceChildren(view);
      renderControls();
      await Promise.all([loadPreferences(), loadLogin()]);
    },
    applyPreferences(prefs) {
      applyTheme(doc, prefs.theme);
    },
  };
}
