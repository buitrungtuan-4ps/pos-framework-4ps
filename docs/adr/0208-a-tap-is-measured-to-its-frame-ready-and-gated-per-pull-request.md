# ADR-0208 — A tap is measured to its frame ready to paint, and gated on every pull request

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-05
· Amends [ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md) decisions 1, 2, 4 and 5
· Amends [ADR-0109](0109-counting-the-taps-an-operator-makes.md) (per-commit checks assert no timing),
for this gate only
· Relates to [ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

Accepted by the owner on 2026-11-05. The owner chose metric A and a pull-request gate at 2×.

## Context

The phase 0 baseline ([`till-latency-2026-11-baseline.md`](../metrics/till-latency-2026-11-baseline.md))
found that ADR-0197's metric, from the tap to the first frame after its change, cannot reach 10 ms at
60 Hz, however fast the till is. The display paints every 16.7 ms, and the wait for the next frame
alone is about 12.5 ms under the harness. Every act would fail, and the till's code would not be
the reason.

## Options considered

- **A. To the frame being ready to paint.** The till's work for the tap is done, and the frame
  that shows it is committed. The wait for the display is left out, and a separate rule says the
  tap never makes the till miss a frame.
- **B. To the frame actually shown.** Rejected: at 60 Hz the vsync wait alone is about 12.5 ms.

## Decision

1. **The metric (A).** For each act, from the tap's `pointerdown` timestamp to the moment the frame
   that answers it is ready to paint. That figure is two parts added together:
   - the time to the answering change in the DOM (`tapToChange`);
   - the main thread's work in that frame: from its `requestAnimationFrame` callback to the end of
     its style, layout and paint, read as the first task after the frame.

   The idle wait between the change and that frame's start is not counted. The budget is **10 ms at
   p95**, over at least 100 samples, for the first answering paint of every act. ADR-0197
   decision 3's exemptions stand unchanged.
2. **The frame rule.** A tap never makes the till miss a frame. The answering change is painted in
   the first frame after it. No frame boundary in the sample is more than 1.5 frame intervals
   after the one before it (25 ms at 60 Hz). This is checked per sample and recorded as
   `longFrames`.
3. **CI: every pull request, at 2×.** `just perf-gate` (`cargo xtask perf-gate`) runs the harness
   with `POS_PERF_CPU_THROTTLE=2` (Chrome DevTools Protocol `Emulation.setCPUThrottlingRate`, which
   slows the page's main thread only). It runs against a release `minimal-edge` and a production
   till build, with at least 100 samples per act. An act fails when its p95 is over 10 ms, or when
   more than 1 sample in 100 breaks the frame rule. A failing act is measured once more, and the
   gate fails only if the second run fails too. An act is enforced once the default package draws
   it (ADR-0197 decision 4). Until then its ADR-0173 budget stays. For this gate alone, the gate
   asserts a timing per commit.
4. **4× stays nightly, recorded and not enforced**, until a reference NUC is measured. Then the
   gate's rate becomes the NUC's 1× p50 divided by the runner's, rounded up, recorded under
   `docs/metrics/`, by amending this record.
5. **On real tills: the same metric.** Telemetry measures frame-ready by the same method. It counts
   samples into ADR-0197 decision 5's buckets and also counts long frames, per act, device class
   and release. It still carries no person, order, table or device id.

## Consequences accepted

- **What the operator sees is later than the figure**, by up to one frame interval: about 8 ms on
  average at 60 Hz. The display's wait is not the till's to spend, and the frame rule keeps the
  till from adding a frame of its own.
- **A shared runner is noisy.** One automatic re-run, and a long-frame allowance of 1 in 100, are
  the price of a per-commit gate. A runner that turns noisier is answered with more samples, never
  a higher budget.
- **2× is a stand-in**, said to be one wherever its figures appear.
- **Acts over budget at 2× are phase 2 work**, done before the gate is turned on for them.
