import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createSettingsBridge } from "./api";
import { renderUnavailable } from "./app";
import { decodePreferences } from "./preferences";
import { createSettingsApp } from "./settings";
import "./styles.css";

const root = document.getElementById("settings");
if (!root) throw new Error("Missing #settings root element");

if (!isTauri()) {
  renderUnavailable(root);
} else {
  const app = createSettingsApp({ root, bridge: createSettingsBridge((command, args) => invoke(command, args)) });
  void listen<unknown>("preferences-changed", (event) => {
    try {
      app.applyPreferences(decodePreferences(event.payload, "preferences-changed payload"));
    } catch (error) {
      console.error("Ignoring malformed preferences-changed event:", error);
    }
  }).catch((error) => console.error("Could not listen for preferences-changed:", error));
  // True when the native blur material failed; CSS then paints a solid tint.
  void invoke<boolean>("glass_unavailable").then((off) => {
    if (off) document.documentElement.dataset.glass = "off";
  }).catch((error) => console.error("Could not read glass state:", error));
  void app.start();
}
