import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: { strictPort: true, port: 1420 },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: { target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13" },
});
