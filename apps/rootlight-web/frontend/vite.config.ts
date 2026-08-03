// Configures deterministic production assets and the explicit local development proxy.

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

const developmentApiPort = process.env.ROOTLIGHT_WEB_DEV_API_PORT ?? "43127";
// gl-bench publishes a valid ESM module but prioritizes its UMD browser build.
const glBenchEsmEntry = fileURLToPath(
  new URL("./node_modules/gl-bench/dist/gl-bench.module.js", import.meta.url),
);

const cosmosModuleSuffix = "/node_modules/@cosmos.gl/graph/dist/index.js";

/**
 * Replaces Cosmos 3.4 DOM style writes with external-class equivalents.
 *
 * The exact replacements deliberately fail the build when the pinned dependency changes, because
 * silently shipping a new inline-style path would violate the native host's strict CSP.
 */
function cosmosStrictCspPlugin(): Plugin {
  const replacements: readonly (readonly [string, string])[] = [
    [
      '    document.documentElement.style.setProperty("--cosmosgl-attribution-color", t > 0.65 ? "black" : "white"), document.documentElement.style.setProperty("--cosmosgl-error-message-color", t > 0.65 ? "black" : "white"), this.div && (this.div.style.backgroundColor = `rgba(${e[0] * 255}, ${e[1] * 255}, ${e[2] * 255}, ${e[3]})`), this.isDarkenGreyout = t < 0.65;',
      "    this.isDarkenGreyout = t < 0.65;",
    ],
    [
      '      if (r.parentNode !== this.store.div && (r.parentNode && r.parentNode.removeChild(r), this.store.div.appendChild(r)), this.addAttribution(), r.style.width = "100%", r.style.height = "100%", this.canvas = r, this.updateCanvasTouchAction(), await Promise.race([',
      '      if (r.parentNode !== this.store.div && (r.parentNode && r.parentNode.removeChild(r), this.store.div.appendChild(r)), this.addAttribution(), r.classList.add("rootlight-cosmos-canvas"), this.canvas = r, this.updateCanvasTouchAction(), await Promise.race([',
    ],
    [
      '      this.stopFrames(), _(this.canvas).style("cursor", null), this.device && (this.device.beginRenderPass({',
      '      this.stopFrames(), this.canvas.classList.remove("rootlight-cosmos-canvas--point", "rootlight-cosmos-canvas--link", "rootlight-cosmos-canvas--grab", "rootlight-cosmos-canvas--grabbing"), this.device && (this.device.beginRenderPass({',
    ],
    [
      '    this.canvas.style.touchAction = this.config.enableDrag || this.config.enableZoom ? "none" : "";',
      '    this.canvas.classList.toggle("rootlight-cosmos-canvas--interactive", this.config.enableDrag || this.config.enableZoom);',
    ],
    [
      '    this.dragInstance.isActive ? _(this.canvas).style("cursor", "grabbing") : this.store.hoveredPoint ? !this.config.enableDrag || this.store.isSpaceKeyPressed ? _(this.canvas).style("cursor", e) : _(this.canvas).style("cursor", "grab") : this.store.isLinkHoveringEnabled && this.store.hoveredLinkIndex !== void 0 ? _(this.canvas).style("cursor", t) : _(this.canvas).style("cursor", null);',
      '    this.canvas.classList.remove("rootlight-cosmos-canvas--point", "rootlight-cosmos-canvas--link", "rootlight-cosmos-canvas--grab", "rootlight-cosmos-canvas--grabbing"), this.dragInstance.isActive ? this.canvas.classList.add("rootlight-cosmos-canvas--grabbing") : this.store.hoveredPoint ? !this.config.enableDrag || this.store.isSpaceKeyPressed ? this.canvas.classList.add("rootlight-cosmos-canvas--point") : this.canvas.classList.add("rootlight-cosmos-canvas--grab") : this.store.isLinkHoveringEnabled && this.store.hoveredLinkIndex !== void 0 && this.canvas.classList.add("rootlight-cosmos-canvas--link");',
    ],
  ];

  return {
    name: "rootlight-cosmos-strict-csp",
    enforce: "pre",
    transform(code, id) {
      const normalizedId = id.split("?")[0]?.replaceAll("\\", "/");
      if (normalizedId?.endsWith(cosmosModuleSuffix) !== true) {
        return null;
      }
      let transformed = code;
      for (const [source, replacement] of replacements) {
        const first = transformed.indexOf(source);
        if (first < 0 || transformed.includes(source, first + source.length)) {
          throw new Error(
            "The pinned Cosmos DOM integration changed; review the strict-CSP compatibility transform.",
          );
        }
        transformed = transformed.replace(source, replacement);
      }
      return { code: transformed, map: null };
    },
  };
}

export default defineConfig({
  plugins: [cosmosStrictCspPlugin(), react(), tailwindcss()],
  optimizeDeps: {
    exclude: ["@cosmos.gl/graph"],
  },
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
