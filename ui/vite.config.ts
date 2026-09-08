import { fileURLToPath, URL } from "node:url";

import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

// Tauri 期望固定的开发端口（与 tauri.conf.json 的 devUrl 一致）
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  // Tauri 使用固定端口，若被占用则直接失败而非自动切换
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 5174,
        }
      : undefined,
    watch: {
      // 不监听 Rust 侧目录，避免无关重建
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_", "TAURI_"],
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
