import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createTauriBridge } from "./api";
import { createApp, renderUnavailable } from "./app";
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

  document.addEventListener("visibilitychange", () => activity.setPageVisible(document.visibilityState === "visible"));
  window.addEventListener("focus", () => activity.setFocused(true));
  window.addEventListener("blur", () => activity.setFocused(false));
  void app.start();
}
