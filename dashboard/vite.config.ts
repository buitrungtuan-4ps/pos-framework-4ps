import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";

// Builds the back-office dashboard into `dist/`, which `pos_cloud` embeds with rust-embed
// (ADR-0060). A relative base keeps the embedded assets resolvable behind whatever hostname the
// cell is reached on. The dev server proxies the admin/API surface to a locally running pos_cloud
// so `pnpm dev` is a live dashboard against a real cloud (default bind 0.0.0.0:8080).
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
      "/admin": "http://127.0.0.1:8080",
      "/v1": "http://127.0.0.1:8080",
      "/health": "http://127.0.0.1:8080",
    },
  },
});
