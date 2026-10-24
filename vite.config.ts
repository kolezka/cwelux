import { defineConfig } from "vite";

// Tauri expects a fixed dev port and its own terminal output.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    outDir: "dist",
    target: "safari15",
    emptyOutDir: true,
    // The popover and the settings window are separate pages.
    rolldownOptions: {
      input: { main: "index.html", settings: "settings.html" },
    },
  },
});
