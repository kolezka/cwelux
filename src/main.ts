import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createTauriBridge } from "./api";
import { createApp, renderUnavailable } from "./app";
import { decodePreferences } from "./preferences";
import { createActivityTracker } from "./state";
import "./styles.css";

const root = document.getElementById("app");
if (!root) throw new Error("Missing #app root element");

if (!isTauri()) {
  // A plain browser cannot reach tmux. Say so instead of showing an empty list.
  renderUnavailable(root);
} else {
  const app = createApp({
    root,
    bridge: createTauriBridge((command, args) => invoke(command, args)),
    copyText: (text) => navigator.clipboard.writeText(text),
  });
  const activity = createActivityTracker((active) => app.setActive(active));

  // The native side emits true when the popover is shown and false when hidden.
  void listen<unknown>("popover-visibility", (event) => {
    if (typeof event.payload !== "boolean") {
      console.error("Ignoring popover-visibility event with a non-boolean payload:", event.payload);
      return;
    }
    activity.setNativeVisible(event.payload);
  }).catch((error) => console.error("Could not listen for popover-visibility:", error));

  // Sent by the native side only after preferences were written successfully.
  void listen<unknown>("preferences-changed", (event) => {
    try {
      app.applyPreferences(decodePreferences(event.payload, "preferences-changed payload"));
    } catch (error) {
      console.error("Ignoring malformed preferences-changed event:", error);
    }
  }).catch((error) => console.error("Could not listen for preferences-changed:", error));

  // The native side sends this when the blur material failed; CSS then paints a solid tint.
  void listen("glass-unavailable", () => {
    document.documentElement.dataset.glass = "off";
  }).catch((error) => console.error("Could not listen for glass-unavailable:", error));

  document.addEventListener("visibilitychange", () => activity.setPageVisible(document.visibilityState === "visible"));
  window.addEventListener("focus", () => activity.setFocused(true));
  window.addEventListener("blur", () => activity.setFocused(false));
  void app.start();
}
