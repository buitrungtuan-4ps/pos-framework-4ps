// Throwaway: time the wasm32 CEL evaluator inside headless Chromium, at 1x and 4x CPU throttle.
// usage: node bench-browser.cjs <file.wasm> <path to a node_modules that has playwright>
const fs = require("fs");
const path = require("path");
const { chromium } = require(path.join(process.argv[3], "playwright"));

(async () => {
  const bytes = fs.readFileSync(process.argv[2]).toString("base64");
  const browser = await chromium.launch({ executablePath: process.env.POS_UI_CHROMIUM });
  for (const rate of [1, 4]) {
    const page = await browser.newPage();
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Emulation.setCPUThrottlingRate", { rate });
    const out = await page.evaluate(async (b64) => {
      const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      let t = performance.now();
      const { instance } = await WebAssembly.instantiate(bin, {});
      const instantiate = performance.now() - t;
      const x = instance.exports;
      t = performance.now();
      x.setup();
      const setup = performance.now() - t;
      const time = (f, n) => { f(1); const s = performance.now(); f(n); return ((performance.now() - s) * 1000) / n; };
      const rows = [];
      for (let i = 0; i < 5; i++) {
        rows.push({
          i,
          compile_us: time((n) => x.compile_n(i, n), 2000).toFixed(2),
          eval_us: time((n) => x.eval_n(i, n), 50000).toFixed(3),
          fresh_us: time((n) => x.eval_fresh_n(i, n), 20000).toFixed(3),
          first_eval_us: (() => { const s = performance.now(); x.eval_n(i, 1); return ((performance.now() - s) * 1000).toFixed(1); })(),
        });
      }
      return { instantiate_ms: instantiate.toFixed(1), setup_ms: setup.toFixed(2), rows };
    }, bytes);
    console.log(`== ${rate}x`, JSON.stringify(out, null, 1));
    await page.close();
  }
  await browser.close();
})();
