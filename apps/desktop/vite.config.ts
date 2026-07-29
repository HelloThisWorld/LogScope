import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri development server settings: fixed port, no auto-open, and no
// remote assets of any kind (the application is fully offline).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "chrome120",
    sourcemap: false,
  },
});
