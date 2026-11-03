// Where each act's time went, from a trace `pnpm perf` wrote with `POS_PERF_TRACE=<file>`.
//
//   node perf/trace-breakdown.mjs <trace.json>
//
// The harness marks every sample as a user-timing measure named `ttp <act>`, from the tap's
// `pointerdown` to the animation frame that answers it. For each act this prints the mean over its
// samples of what the page's main thread did inside that window, by the self time of each trace
// event, in four groups:
//
// - **script**: event dispatch, timers, microtasks, animation-frame callbacks, compiling, GC;
// - **style/layout**: style recalculation, layout, hit testing, layer-tree updates;
// - **paint**: pre-paint, paint, layerize and the commit to the compositor;
// - **other**: the rest of the main thread's own time — task scheduling, IPC and tracing itself;
// - **idle**: the window minus everything the main thread did, which is mostly waiting for the
//   next frame and, for an act that waits for the edge, waiting on `/api`.
//
// `frame` is the part of the idle time spent between the last task before the answering frame and
// that frame's start: what a tap waits for the display's clock once its work is done.
// `api` is how much of the window had at least one `/api/*` request in flight, which overlaps the
// idle time rather than adding to it. A record of the method in `docs/metrics/`, not a gate.

import { readFileSync } from "node:fs";

const SCRIPT = new Set([
  "EventDispatch", "FunctionCall", "EvaluateScript", "TimerFire", "FireAnimationFrame", "RunMicrotasks",
  "V8.Execute", "v8.compile", "v8.compileModule", "V8.CompileCode", "MinorGC", "MajorGC", "V8.GC_SCAVENGER",
  "BlinkGC.AtomicPhase", "XHRReadyStateChange", "XHRLoad", "v8.callFunction", "v8.evaluateModule",
  "ParseHTML", "FireIdleCallback", "HandlePostMessage",
]);
const LAYOUT = new Set([
  "UpdateLayoutTree", "RecalculateStyles", "Layout", "HitTest", "UpdateLayerTree", "UpdateLayer",
  "ScheduleStyleRecalculation", "InvalidateLayout", "IntersectionObserverController::computeIntersections",
]);
const PAINT = new Set(["PrePaint", "Paint", "PaintImage", "Layerize", "Commit", "CompositeLayers", "UpdateLayerTree::Paint"]);

function group(name) {
  if (SCRIPT.has(name) || /^v8\./i.test(name) || name.startsWith("BlinkScheduler_PerformMicrotask")) return "script";
  if (LAYOUT.has(name)) return "layout";
  if (PAINT.has(name)) return "paint";
  return "other";
}

const path = process.argv[2];
if (path === undefined) {
  console.error("usage: node perf/trace-breakdown.mjs <trace.json>");
  process.exit(2);
}
const raw = JSON.parse(readFileSync(path, "utf8"));
const events = Array.isArray(raw) ? raw : raw.traceEvents;

// The samples: user-timing measures named `ttp <act>`, as async begin/end pairs.
const open = new Map();
const windows = [];
for (const e of events) {
  if (typeof e.name !== "string" || !e.name.startsWith("ttp ") || !String(e.cat).includes("user_timing")) continue;
  const key = `${e.pid}:${e.id ?? e.id2?.local}:${e.name}`;
  if (e.ph === "b") open.set(key, e);
  else if (e.ph === "e" && open.has(key)) {
    const begin = open.get(key);
    open.delete(key);
    windows.push({ act: e.name.slice(4), pid: e.pid, start: begin.ts, end: e.ts });
  }
}
if (windows.length === 0) {
  console.error("no `ttp <act>` measures in this trace: was it written with POS_PERF_TRACE?");
  process.exit(1);
}

// The page's main thread, in the process the measures came from.
const mainThreads = new Set(
  events
    .filter((e) => e.ph === "M" && e.name === "thread_name" && e.args?.name === "CrRendererMain")
    .map((e) => `${e.pid}:${e.tid}`),
);
const slices = events
  .filter((e) => e.ph === "X" && mainThreads.has(`${e.pid}:${e.tid}`) && typeof e.dur === "number")
  .sort((a, b) => a.ts - b.ts || b.dur - a.dur);

// Self time per slice: its duration minus its direct children's.
const byThread = new Map();
for (const s of slices) {
  const key = `${s.pid}:${s.tid}`;
  if (!byThread.has(key)) byThread.set(key, []);
  byThread.get(key).push({ ...s });
}
const pieces = [];
const tasks = [];
for (const list of byThread.values()) {
  const stack = [];
  for (const s of list) {
    while (stack.length > 0 && stack.at(-1).ts + stack.at(-1).dur <= s.ts) stack.pop();
    s.parent = stack.at(-1);
    if (s.parent === undefined) tasks.push(s);
    stack.push(s);
  }
  // A slice's self time is its span minus its children's spans, cut into pieces so a window can
  // take the part of it that falls inside.
  const children = new Map();
  for (const s of list) {
    if (s.parent !== undefined) {
      if (!children.has(s.parent)) children.set(s.parent, []);
      children.get(s.parent).push(s);
    }
  }
  for (const s of list) {
    let cursor = s.ts;
    for (const c of children.get(s) ?? []) {
      if (c.ts > cursor) pieces.push({ pid: s.pid, from: cursor, to: c.ts, group: group(s.name) });
      cursor = Math.max(cursor, c.ts + c.dur);
    }
    const end = s.ts + s.dur;
    if (end > cursor) pieces.push({ pid: s.pid, from: cursor, to: end, group: group(s.name) });
  }
}

// `/api/*` requests, from send to finish, in the same process.
const sent = new Map();
const requests = [];
for (const e of events) {
  const data = e.args?.data;
  if (data === undefined) continue;
  if (e.name === "ResourceSendRequest" && typeof data.url === "string" && new URL(data.url).pathname.startsWith("/api/")) {
    sent.set(data.requestId, e.ts);
  } else if (e.name === "ResourceFinish" && sent.has(data.requestId)) {
    requests.push({ from: sent.get(data.requestId), to: e.ts });
    sent.delete(data.requestId);
  }
}

function overlap(from, to, start, end) {
  return Math.max(0, Math.min(to, end) - Math.max(from, start));
}

/** How much of [start, end) is covered by the union of `spans`. */
function covered(spans, start, end) {
  const inside = spans
    .map((s) => [Math.max(s.from, start), Math.min(s.to, end)])
    .filter(([a, b]) => b > a)
    .sort((x, y) => x[0] - y[0]);
  let total = 0;
  let reach = start;
  for (const [a, b] of inside) {
    if (b > reach) {
      total += b - Math.max(a, reach);
      reach = b;
    }
  }
  return total;
}

const acts = new Map();
for (const w of windows) {
  const sums = { total: w.end - w.start, script: 0, layout: 0, paint: 0, other: 0 };
  for (const p of pieces) {
    sums[p.group] += overlap(p.from, p.to, w.start, w.end);
  }
  sums.idle = Math.max(0, sums.total - sums.script - sums.layout - sums.paint - sums.other);
  sums.api = covered(requests, w.start, w.end);
  const frame = tasks.find((t) => t.ts <= w.end && t.ts + t.dur >= w.end);
  const before = tasks.filter((t) => frame !== undefined && t.ts + t.dur <= frame.ts && t.ts + t.dur > w.start);
  const lastEnd = Math.max(w.start, ...before.map((t) => t.ts + t.dur));
  sums.frame = frame === undefined ? 0 : Math.max(0, frame.ts - lastEnd);
  if (!acts.has(w.act)) acts.set(w.act, []);
  acts.get(w.act).push(sums);
}

const ms = (us) => (Math.round(us / 100) / 10).toFixed(1);
const columns = ["total", "script", "layout", "paint", "other", "idle", "frame", "api"];
console.log(`| Act | n | ${columns.join(" | ")} |`);
console.log(`|---|---|${columns.map(() => "---").join("|")}|`);
for (const [act, samples] of acts) {
  const mean = (k) => samples.reduce((sum, s) => sum + s[k], 0) / samples.length;
  console.log(`| ${act} | ${samples.length} | ${columns.map((k) => ms(mean(k))).join(" | ")} |`);
}
