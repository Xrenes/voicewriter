import { defineConfig } from "vite";
import { fileURLToPath } from "url";

// Vite config for the Tauri windows: the settings page (index.html), the
// refine-wheel popup (wheel.html), the recording widget (recorder.html), and
// the save-recording confirm dialog (recorder-confirm.html). Tauri serves
// all four in dev on port 1420.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // tauri src is watched by the tauri cli, not vite
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // Tauri uses Chromium on Windows / WebKit on Linux
    target: ["es2021", "chrome100", "safari13"],
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        wheel: fileURLToPath(new URL("./wheel.html", import.meta.url)),
        recorder: fileURLToPath(new URL("./recorder.html", import.meta.url)),
        recorderConfirm: fileURLToPath(new URL("./recorder-confirm.html", import.meta.url)),
        transcript: fileURLToPath(new URL("./transcript.html", import.meta.url)),
        regionSelect: fileURLToPath(new URL("./region-select.html", import.meta.url)),
        aiChat: fileURLToPath(new URL("./ai-chat.html", import.meta.url)),
        webToolbar: fileURLToPath(new URL("./web-toolbar.html", import.meta.url)),
      },
    },
  },
});
