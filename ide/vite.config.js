import { defineConfig } from "vite";

// Tauri は固定ポートを前提にする（tauri.conf.json の devUrl と合わせる）。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // WebView2 は Chromium なので、古いブラウザ向けの変換は要らない。
    target: "chrome105",
    outDir: "dist",
    emptyOutDir: true,
    // 配布物にソースマップは載せない。
    sourcemap: false,
  },
});
