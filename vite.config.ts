import { defineConfig } from "vite";

// Vite config for the Tauri setup/status window.
// The frontend is a single small page; Tauri serves it in dev on port 1420.
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
  },
});
