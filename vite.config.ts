import { defineConfig } from "vite";

// Tauri 要求固定端口，vite 会把 1420 视为 dev server 端口
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "esnext",
    outDir: "dist",
    emptyOutDir: true,
  },
});
