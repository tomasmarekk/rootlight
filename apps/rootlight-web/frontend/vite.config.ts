// Configures deterministic production assets and the explicit local development proxy.

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const developmentApiPort = process.env.ROOTLIGHT_WEB_DEV_API_PORT ?? "43127";
// gl-bench publishes a valid ESM module but prioritizes its UMD browser build.
const glBenchEsmEntry = fileURLToPath(
  new URL("./node_modules/gl-bench/dist/gl-bench.module.js", import.meta.url),
);

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: [{ find: /^gl-bench$/u, replacement: glBenchEsmEntry }],
  },
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": {
        changeOrigin: false,
        target: `http://127.0.0.1:${developmentApiPort}`,
      },
    },
  },
  preview: {
    host: "127.0.0.1",
    port: 4173,
    strictPort: true,
  },
  build: {
    assetsInlineLimit: 0,
    cssCodeSplit: true,
    sourcemap: false,
    target: "es2023",
    rollupOptions: {
      output: {
        assetFileNames: "assets/[name]-[hash][extname]",
        chunkFileNames: "assets/[name]-[hash].js",
        entryFileNames: "assets/[name]-[hash].js",
        hashCharacters: "hex",
      },
    },
  },
  test: {
    environment: "jsdom",
    exclude: ["tests/e2e/**"],
    globals: true,
    include: ["tests/**/*.test.{ts,tsx}"],
    setupFiles: ["./tests/setup.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "json-summary"],
      thresholds: {
        branches: 85,
        functions: 85,
        lines: 85,
        statements: 85,
      },
    },
  },
});
