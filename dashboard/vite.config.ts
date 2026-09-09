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
    // Anchored regular expressions, not bare prefixes. Vite matches a plain key as a *prefix*, and
    // the console has a screen at `/admins` — so `"/admin"` forwarded that SPA route to pos_cloud,
    // which answered with the built `index.html` out of `dist/`, whose hashed asset paths do not
    // exist on the dev server. The Admins screen was a white page with two 404s for everyone working
    // in `pnpm dev` (Wave 3 · D4). Every real call is `/admin/<something>`, so requiring the slash
    // costs nothing and stops the next screen path from colliding with the next proxy prefix;
    // `tests/dev-proxy.test.ts` holds that line for all of them, not just this one.
    proxy: {
      "^/admin/": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "^/v1/": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "^/health$": { target: "http://127.0.0.1:8080", changeOrigin: false },
    },
  },
});
