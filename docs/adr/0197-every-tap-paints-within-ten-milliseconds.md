# ADR-0197 — Every tap paints within ten milliseconds

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md) (decision 6, the budgets) ·
Replaces roadmap-v3 PF1's targets for the till · Relates to
[ADR-0109](0109-counting-the-taps-an-operator-makes.md),
[ADR-0188](0188-seating-a-free-table-is-painted-at-once.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner's target: at most 10 ms from the till receiving a tap to the next painted frame, 95th
percentile, on a release build, leaving out acts that wait on an external device or connection.
There is no reference NUC yet, so the plan measures by CPU-throttled emulation in CI and by
telemetry from real tills.

What exists:

- **A harness** (`ui/perf/tap-to-paint.spec.mjs`, `pnpm perf`). It records the tap's `pointerdown`
  timestamp in the capture phase, watches for the answering DOM change with a `MutationObserver`,
  and ends the sample at the first `requestAnimationFrame` after it. A nightly job runs it against a
  release `minimal-edge` and a production till build.
- **Budgets** (ADR-0173 decision 6): 50 ms for what the till answers alone, 150 ms for an act that
  waits for the edge, 300 ms from a reload to a drawn order screen.
- **Figures** on a 4-core Xeon at 1× ([`till-latency-2026-10-after.md`](../metrics/till-latency-2026-10-after.md)):
  p50 about 11 ms and p95 between 12.3 and 19.5 ms for the acts the till paints alone.
- **Per-commit checks assert no timing** ([ADR-0109](0109-counting-the-taps-an-operator-makes.md)):
  a shared runner's number is a number about the runner.
- **Roadmap-v3 PF1** set p99 targets per operation on a weak box (add item under 30 ms, settle under
  60 ms) that nothing measures.

## Options considered

1. **A new metric** (the browser's Event Timing `duration`). It rounds to 8 ms, too coarse for a
   10 ms budget, and would break the comparison with every figure recorded so far.
2. **The harness's metric, a new budget, enforced on an unthrottled runner.** The runner is several
   times faster than the reference box, so a pass says little about a store.
3. **The harness's metric, enforced under a stated CPU throttle, and the same metric from real tills**
   (chosen).

## Decision

1. **The metric is the harness's.** For each act: from the `pointerdown` timestamp of the tap to the
   first animation frame after the DOM change that answers it, on the page's own clock. One figure
   per act: its 95th percentile, over at least 100 samples per run.
2. **The budget is 10 ms at p95 for the first answering paint of every act.** With paint-first
   (ADR-0196) every act answers locally at once: the change itself, provisionally, or a state that
   says what it waits for ("Waiting for the card terminal"). That first paint is what the budget
   covers, for every act in the action catalogue (ADR-0191).
3. **What is exempt, and only this:**
   - the **completion** of a wait on an external device or connection: the card terminal, a printer,
     the drawer, a scale, the cloud gateway, and the edge's commit. ADR-0173's 150 ms for an edge
     answer stays, as a figure recorded, not enforced;
   - a **reload or first boot**, which keeps ADR-0173's 300 ms.
   Any other exemption needs a new ADR.
4. **In CI: emulation, nightly.**
   - The harness gains a CPU throttle through the Chrome DevTools Protocol
     (`Emulation.setCPUThrottlingRate`), its rate from `POS_PERF_CPU_THROTTLE`.
   - The gate runs at **4×** until a reference NUC is measured. Then the rate becomes the NUC's 1×
     p50s divided by the runner's, on the same harness, rounded up, and recorded under
     `docs/metrics/`.
   - It stays nightly, with `POS_PERF_ENFORCE=1`; per-commit checks stay timing-free (ADR-0109).
   - **An act's budget becomes 10 ms when the phase 2 work that renders it lands**, as ADR-0173
     enforced each budget once its change had landed. Until then its ADR-0173 budget stays.
5. **On real tills: telemetry, with no personal data.**
   - The till measures the same interval with the same method, keyed by the act's catalogue id.
   - It counts samples into fixed buckets (1, 2, 4, 6, 8, 10, 12, 16, 24, 32, 50, 100 ms and above),
     per act, device class and release, and hands them to the edge every five minutes. The buffer
     holds at most one interval; what cannot be sent is dropped.
   - The edge forwards them through the `MetricsSink` port, whose queue is bounded and drops when
     full.
   - **No person, order, table or device id** is attached: the store, the device class and the
     release are the only labels. The figures cannot rank or follow a person.
   - The console shows p95 per act per release, and a release whose canary ring reads above 10 ms is
     flagged on its rollout page. It does not stop a ring by itself.
6. **The edge's own budgets stay** (`crates/pos-edge/tests/perf_budget.rs`); PF1's targets for the
   till are replaced by this record.

## Consequences accepted

- **The frame is part of the figure.** A display at 60 Hz paints every 16.7 ms, so a change that
  misses a frame waits for the next. Meeting 10 ms at p95 needs work that finishes early in a frame,
  and the figure is honest about that rather than measuring work alone. The phase 0 baseline
  (`docs/metrics/till-latency-2026-11-baseline.md`) records per act how much of today's figure is
  script, style and layout, paint, and waiting.
- **4× is a stand-in**, said to be one wherever its figures appear, until a NUC calibrates it.
- **The till gains a small measuring module** and sends counts. It sends nothing a person could be
  identified by, so it needs no consent step under Decree 13/2023, the GDPR or the CCPA.
- **A nightly red is the signal.** A regression reaches `main` and is caught the next morning; that
  is the price of not asserting timing on shared runners per commit.
