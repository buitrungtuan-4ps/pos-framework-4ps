# ADR-0208 — A tap is measured to its frame ready to paint, and gated on every pull request

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-05
· Amends [ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md) decisions 1, 2, 4 and 5
· Amends [ADR-0109](0109-counting-the-taps-an-operator-makes.md) (per-commit checks assert no timing),
for this gate only
· Relates to [ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

Accepted by the owner on 2026-11-05. The owner chose metric A and a pull-request gate at 2×, blocking
on the median of each act; the 95th percentile is recorded and tightened later.

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

   The idle wait between the change and that frame's start is not counted. ADR-0197's target stands:
   **10 ms at p95** for the first answering paint of every act, and its decision 3's exemptions stand
   unchanged. What CI enforces of it, until real hardware is measured, is decision 3's.
2. **The frame rule.** A tap never makes the till miss a frame. The answering change is painted in
   the first frame after it. No frame boundary in the sample is more than 1.5 frame intervals
   after the one before it (25 ms at 60 Hz). This is checked per sample and recorded as
   `longFrames`, per act. CI records it and does not fail on it, until a run of records shows it
   steady enough on a shared runner to enforce, by amending this record.
3. **CI: every pull request, at 2×, blocking on the median.** `just perf-gate` runs the harness
   with `POS_PERF_CPU_THROTTLE=2` (Chrome DevTools Protocol `Emulation.setCPUThrottlingRate`, which
   slows the page's main thread only). It runs against a release `minimal-edge` and a production
   till build, with at least 100 samples per act. An act fails when its **p50 is over 10 ms**, or
   when it has fewer than 100 samples. Its p95 and its long frames are printed and written with
   every run, and fail nothing. A failing act is measured once more, and the gate fails only if the
   second run fails too. An act is enforced once the default package draws it (ADR-0197 decision
   4). Until then its ADR-0173 budget stays. For this gate alone, the gate asserts a timing per
   commit.
4. **p95 is tightened in phase 9.** When a reference NUC is measured, the gate's rate becomes the
   NUC's 1× p50 divided by the runner's, rounded up, and the gate moves from p50 towards ADR-0197's
   p95, recorded under `docs/metrics/`, by amending this record. **4× stays nightly, recorded and not
   enforced**, until then.
5. **On real tills: the same metric.** Telemetry measures frame-ready by the same method. It counts
   samples into ADR-0197 decision 5's buckets and also counts long frames, per act, device class
   and release. It still carries no person, order, table or device id.

## Consequences accepted

- **What the operator sees is later than the figure**, by up to one frame interval: about 8 ms on
  average at 60 Hz. The display's wait is not the till's to spend, and the frame rule keeps the
  till from adding a frame of its own.
- **A shared runner is noisy.** Its tail is noisier than its median, which is why the per-commit
  gate holds the median, re-runs a failing act once, and records the tail and the long frames
  rather than failing on them. A runner that turns noisier is answered with more samples, never a
  higher budget.
- **Up to half of an act's taps may take over 10 ms, and the gate still passes.** Until phase 9, a regression that only
  lengthens the tail is seen in the recorded p95, not stopped by the gate. Reviewers read it.
- **2× is a stand-in**, said to be one wherever its figures appear.
- **Acts over budget at 2× are phase 2 work**, done before the gate is turned on for them.
