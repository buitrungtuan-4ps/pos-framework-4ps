// Drives `bench/volume.html` in Chromium and prints how long the console's table takes to paint.
//
// Usage: start the dev server (`pnpm dev`), then `node bench/measure.mjs [baseUrl]`.
//
// This exists because "virtualise the tables" sat in the roadmap as planned work on the strength of
// a docstring's assertion — "right-sized for tens to hundreds of rows" — that nobody had measured.
// Virtualisation is a large change to the one component every list screen renders. The numbers this
// prints are what decide whether to build it, and they belong in the repository rather than in
// somebody's terminal history.
//
// Not wired into `pnpm build`: it needs a browser and a running dev server, and a gate that needs
// both is a gate that gets skipped. It is a tool to be run when the question comes up again.

import playwright from "playwright";

const { chromium } = playwright;

const base = process.argv[2] ?? "http://127.0.0.1:5173";

/** Unpaged, so every row renders — the case that decides whether virtualisation is needed. */
const UNPAGED = [100, 500, 1_000, 2_000, 5_000, 10_000];

/** The bounded case, for contrast: what a `pageSize` already buys at the same volume. */
const PAGED = [{ rows: 10_000, page: 25 }];

async function measure(page, rows, pageSize) {
  const query = `rows=${rows}${pageSize > 0 ? `&page=${pageSize}` : ""}`;
  await page.goto(`${base}/bench/volume.html?${query}`, { waitUntil: "load" });
  await page.waitForSelector("#result", { timeout: 60_000 });
  return JSON.parse(await page.textContent("#result"));
}

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  // One warm-up: the first navigation pays for Vite's on-demand transform of the component tree,
  // which is a dev-server cost and not the console's.
  await measure(page, 100, 0);

  const results = [];
  for (const rows of UNPAGED) {
    results.push(await measure(page, rows, 0));
  }
  for (const { rows, page: size } of PAGED) {
    results.push(await measure(page, rows, size));
  }

  console.log("rows\tpageSize\trendered\tms");
  for (const row of results) {
    console.log(`${row.rows}\t${row.pageSize}\t\t${row.renderedRows}\t\t${row.ms}`);
  }
} finally {
  await browser.close();
}
