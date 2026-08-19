import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";

// Builds the operator UI into `dist/`, which `pos_edge` embeds with rust-embed (ADR-0018). A
// relative base keeps the embedded assets resolvable however the store is reached on the LAN.
// The dev server proxies the API and the WebSocket to a locally running edge so `pnpm dev` is a
// live interface against a real store.
export default defineConfig({
  plugins: [solid(), tailwindcss()],
  base: "/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:8787",
      "/ws": { target: "ws://127.0.0.1:8787", ws: true },
      "/healthz": "http://127.0.0.1:8787",
    },
  },
});
