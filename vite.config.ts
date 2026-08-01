import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The Tauri dev shell points at this server. `strictPort` is not optional:
// a silent 5173 -> 5174 fallback changes the page origin, which repartitions
// localStorage and makes every piece of saved UI state vanish with no error.
export default defineConfig({
  root: "ui",
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5273,
    strictPort: true,
    watch: { ignored: ["**/target/**", "**/src-tauri/**"] },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "safari15",
    sourcemap: true,
  },
});
