import { beforeEach, describe, expect, test } from "bun:test";
import { Window } from "happy-dom";
import type { SettingsBridge } from "./api";
import { DEFAULT_PREFERENCES, type Preferences } from "./preferences";
import { createSettingsApp } from "./settings";

type Method = keyof SettingsBridge;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

class FakeSettingsBridge implements SettingsBridge {
  calls: Array<{ method: Method; args: unknown[] }> = [];
  stored: Preferences = { defaultDirectory: "~/code", refreshIntervalSeconds: 10, theme: "dark" };
  loginEnabled = false;
  handlers: Partial<Record<Method, (...args: any[]) => Promise<any>>> = {};

  private call<T>(method: Method, args: unknown[], fallback: () => T): Promise<T> {
    this.calls.push({ method, args });
    const handler = this.handlers[method];
    return handler ? handler(...args) : Promise.resolve().then(fallback);
  }
  count(method: Method) {
    return this.calls.filter((c) => c.method === method).length;
  }
  getPreferences() {
    return this.call("getPreferences", [], () => ({ ...this.stored }));
  }
  savePreferences(preferences: Preferences) {
    return this.call("savePreferences", [preferences], () => {
      this.stored = { ...preferences };
      return { ...preferences };
    });
  }
  chooseDirectory() {
    return this.call("chooseDirectory", [], () => "/Users/demo/picked" as string | null);
  }
  getLaunchAtLogin() {
    return this.call("getLaunchAtLogin", [], () => this.loginEnabled);
  }
  setLaunchAtLogin(enabled: boolean) {
    return this.call("setLaunchAtLogin", [enabled], () => {
      this.loginEnabled = enabled;
      return enabled;
    });
  }
}

let window: Window;
let document: Document;
let root: HTMLElement;
let bridge: FakeSettingsBridge;

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};
const $ = <T extends HTMLElement = HTMLElement>(selector: string) => {
  const el = root.querySelector(selector);
  if (!el) throw new Error(`missing ${selector}`);
  return el as unknown as T;
};
const setValue = (selector: string, value: string) => {
  const input = $<HTMLInputElement | HTMLSelectElement>(selector);
  input.value = value;
  input.dispatchEvent(new window.Event("input", { bubbles: true }) as unknown as Event);
  input.dispatchEvent(new window.Event("change", { bubbles: true }) as unknown as Event);
};
const toggleLogin = () => {
  const box = $<HTMLInputElement>("#launch-at-login");
  box.checked = !box.checked;
  box.dispatchEvent(new window.Event("change", { bubbles: true }) as unknown as Event);
};
const submit = () =>
  $<HTMLFormElement>("#settings-form").dispatchEvent(
    new window.Event("submit", { bubbles: true, cancelable: true }) as unknown as Event,
  );
const theme = () => document.documentElement.dataset.theme;

async function mount() {
  const app = createSettingsApp({ root, bridge });
  await app.start();
  await flush();
  return app;
}

beforeEach(() => {
  window = new Window({ url: "http://localhost/settings.html" });
  document = window.document as unknown as Document;
  document.body.innerHTML = '<main id="settings"></main>';
  root = document.getElementById("settings")!;
  bridge = new FakeSettingsBridge();
});

describe("loading", () => {
  test("shows the saved preferences and the real login item state", async () => {
    bridge.loginEnabled = true;
    await mount();
    expect($<HTMLInputElement>("#default-directory").value).toBe("~/code");
    expect($<HTMLSelectElement>("#refresh-interval").value).toBe("10");
    expect($<HTMLSelectElement>("#theme").value).toBe("dark");
    expect($<HTMLInputElement>("#launch-at-login").checked).toBe(true);
    expect(theme()).toBe("dark");
    expect($("#login-hint").textContent).toMatch(/applies immediately/i);
  });

  test("offers exactly the approved intervals and themes", async () => {
    await mount();
    const values = (sel: string) => [...root.querySelectorAll<HTMLOptionElement>(`${sel} option`)].map((o) => o.value);
    expect(values("#refresh-interval")).toEqual(["2", "4", "10"]);
    expect(values("#theme")).toEqual(["system", "light", "dark"]);
  });

  test("opening settings never changes the login item", async () => {
    await mount();
    expect(bridge.count("setLaunchAtLogin")).toBe(0);
    submit();
    await flush();
    expect(bridge.count("savePreferences")).toBe(1);
    expect(bridge.count("setLaunchAtLogin")).toBe(0);
  });

  test("a malformed settings file is shown and Save is disabled, so nothing overwrites it", async () => {
    bridge.handlers.getPreferences = async () => {
      throw "Settings file /x/preferences.json is malformed (EOF). Fix or remove it.";
    };
    await mount();
    expect($("#settings-error").textContent).toContain("Could not load settings");
    expect($("#settings-error").textContent).toContain("malformed");
    expect($<HTMLButtonElement>("#save-settings").disabled).toBe(true);
    submit();
    await flush();
    expect(bridge.count("savePreferences")).toBe(0);
  });

  test("an unreadable login item state is unchecked, disabled and explained", async () => {
    bridge.loginEnabled = true;
    bridge.handlers.getLaunchAtLogin = async () => {
      throw "Could not read the launch at login state: denied";
    };
    await mount();
    const box = $<HTMLInputElement>("#launch-at-login");
    expect(box.checked).toBe(false);
    expect(box.disabled).toBe(true);
    expect($("#login-error").textContent).toContain("denied");
    expect($<HTMLInputElement>("#default-directory").value).toBe("~/code");
  });
});

describe("saving", () => {
  test("saves exactly the three preferences and applies the theme", async () => {
    await mount();
    setValue("#default-directory", "  /tmp/work ");
    setValue("#refresh-interval", "2");
    setValue("#theme", "light");
    expect(theme()).toBe("dark");
    submit();
    await flush();
    expect(bridge.calls.find((c) => c.method === "savePreferences")?.args).toEqual([
      { defaultDirectory: "/tmp/work", refreshIntervalSeconds: 2, theme: "light" },
    ]);
    expect(theme()).toBe("light");
    expect($("#settings-notice").textContent).toBe("Saved.");
    expect($("#settings-error").textContent).toBe("");
  });

  test("a save error keeps the form and the previous theme", async () => {
    await mount();
    bridge.handlers.savePreferences = async () => {
      throw "Not a directory: /etc/hosts";
    };
    setValue("#default-directory", "/etc/hosts");
    setValue("#theme", "light");
    submit();
    await flush();
    expect($("#settings-error").textContent).toContain("Could not save settings: Not a directory: /etc/hosts");
    expect($<HTMLInputElement>("#default-directory").value).toBe("/etc/hosts");
    expect($<HTMLSelectElement>("#theme").value).toBe("light");
    expect(theme()).toBe("dark");
    expect($("#settings-notice").textContent).toBe("");
    expect(bridge.stored).toEqual({ defaultDirectory: "~/code", refreshIntervalSeconds: 10, theme: "dark" });
  });

  test("an empty directory is rejected before calling the backend", async () => {
    await mount();
    setValue("#default-directory", "   ");
    submit();
    await flush();
    expect($("#settings-error").textContent).toMatch(/starting directory/i);
    expect(bridge.count("savePreferences")).toBe(0);
  });

  test("saves once while a save is pending", async () => {
    await mount();
    const slow = deferred<Preferences>();
    bridge.handlers.savePreferences = () => slow.promise;
    submit();
    submit();
    await flush();
    expect(bridge.count("savePreferences")).toBe(1);
    expect($<HTMLButtonElement>("#save-settings").disabled).toBe(true);
    slow.resolve({ ...DEFAULT_PREFERENCES });
    await flush();
    expect($<HTMLButtonElement>("#save-settings").disabled).toBe(false);
  });
});

describe("folder picker", () => {
  test("a chosen folder fills the directory field, cancel leaves it", async () => {
    await mount();
    $("#choose-directory").click();
    await flush();
    expect($<HTMLInputElement>("#default-directory").value).toBe("/Users/demo/picked");
    bridge.handlers.chooseDirectory = async () => null;
    $("#choose-directory").click();
    await flush();
    expect($<HTMLInputElement>("#default-directory").value).toBe("/Users/demo/picked");
    expect(bridge.count("savePreferences")).toBe(0);
  });

  test("a picker failure is shown", async () => {
    await mount();
    bridge.handlers.chooseDirectory = async () => {
      throw "dialog unavailable";
    };
    $("#choose-directory").click();
    await flush();
    expect($("#settings-error").textContent).toContain("dialog unavailable");
  });
});

describe("launch at login", () => {
  test("applies immediately and shows the state the OS reports", async () => {
    await mount();
    toggleLogin();
    await flush();
    expect(bridge.calls.find((c) => c.method === "setLaunchAtLogin")?.args).toEqual([true]);
    expect($<HTMLInputElement>("#launch-at-login").checked).toBe(true);
    expect(bridge.count("savePreferences")).toBe(0);
  });

  test("an error rolls the checkbox back to the re-read state", async () => {
    await mount();
    bridge.handlers.setLaunchAtLogin = async () => {
      throw "Could not turn on launch at login: write denied";
    };
    toggleLogin();
    await flush();
    const box = $<HTMLInputElement>("#launch-at-login");
    expect(box.checked).toBe(false);
    expect(box.disabled).toBe(false);
    expect($("#login-error").textContent).toContain("write denied");
    expect(bridge.count("getLaunchAtLogin")).toBe(2);
  });

  test("when the state cannot be re-read after an error it never shows enabled", async () => {
    await mount();
    bridge.handlers.setLaunchAtLogin = async () => {
      throw "Launch at login is still off after trying to turn on it.";
    };
    bridge.handlers.getLaunchAtLogin = async () => {
      throw "read denied";
    };
    toggleLogin();
    await flush();
    const box = $<HTMLInputElement>("#launch-at-login");
    expect(box.checked).toBe(false);
    expect(box.disabled).toBe(true);
    expect($("#login-error").textContent).toContain("still off");
    expect($("#login-error").textContent).toContain("read denied");
  });

  test("the checkbox is locked while a change is pending", async () => {
    await mount();
    const slow = deferred<boolean>();
    bridge.handlers.setLaunchAtLogin = () => slow.promise;
    toggleLogin();
    await flush();
    expect($<HTMLInputElement>("#launch-at-login").disabled).toBe(true);
    slow.resolve(true);
    await flush();
    expect($<HTMLInputElement>("#launch-at-login").disabled).toBe(false);
    expect(bridge.count("setLaunchAtLogin")).toBe(1);
  });
});

describe("live changes", () => {
  test("a preferences-changed event applies the theme", async () => {
    const app = await mount();
    app.applyPreferences({ ...DEFAULT_PREFERENCES, theme: "light" });
    expect(theme()).toBe("light");
    app.applyPreferences({ ...DEFAULT_PREFERENCES });
    expect(theme()).toBeUndefined();
  });
});
