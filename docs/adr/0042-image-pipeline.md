# ADR-0042 — The image pipeline buys `image`, re-encodes to JPEG, and fits a byte budget by ladder

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0026](0026-port-shapes.md) · `docs/roadmap.md` P7

**Context.** `docs/roadmap.md` P7 requires an **image pipeline** that turns a tenant-uploaded menu
image into two renditions under hard byte budgets — a **≤30 KB thumbnail** and a **≤150 KB detail** —
so a store on a slow link loads a menu quickly and the cloud stores bounded objects. Two questions:
what does the decoding/resizing/encoding, and how is a *byte* budget (not a dimension) actually met.

**Decision.**

- **Buy `image` for the codec and resize work.** Decoding arbitrary PNG/JPEG, resampling, and
  re-encoding JPEG is exactly the "genuinely hard and general" infrastructure
  [ADR-0007](0007-in-house-vs-dependency.md) says to buy rather than hand-roll — a bespoke JPEG
  encoder would be a sub-project with its own security surface. The dependency is taken with
  **minimal features** (`default-features = false, features = ["png", "jpeg"]`): decode the two
  formats a tenant realistically uploads, encode JPEG. `cargo-deny` passes unchanged — every crate the
  minimal tree pulls is already-allowed permissive licensing, no new advisory, no duplicate-version
  ban — so the pipeline adds a dependency, not a `deny.toml` entry.

- **Both renditions are JPEG.** A photograph re-encodes far smaller as JPEG than PNG at the same
  perceptual quality, and a menu image is a photograph; JPEG has no alpha, which a menu image does not
  need, so the pipeline flattens to RGB and encodes JPEG for both the thumbnail and the detail.

- **The byte budget is met by a descending (max-edge, quality) ladder, not a formula.** JPEG size is
  not a closed-form function of dimension and quality — it depends on the image's content — so the
  pipeline *tries*: for each rendition it walks a fixed ladder of `(max_edge, quality)` attempts from
  largest/highest to smallest/lowest, resizing to fit within `max_edge × max_edge` (aspect preserved)
  and encoding at `quality`, and returns the **first attempt at or under budget**. The ladders end
  aggressively enough (a 64-px thumbnail at quality 35, a 480-px detail at quality 40) that any real
  image fits, and if even the smallest attempt somehow exceeds the budget the pipeline returns a
  `Budget` error rather than emit an over-budget object — the budget is a guarantee, not a hope. The
  ladder is fixed and content-independent, so the pipeline is pure and deterministic: the same bytes
  in give the same bytes out, unit-testable with no I/O.

- **The pipeline is pure `bytes → bytes`; storage and the upload route are separate.** `render(&[u8])
  -> Renditions { thumbnail, detail }` performs no I/O and knows nothing about where the renditions
  land. This slice is the pipeline itself — the roadmap's named deliverable and the part with the real
  algorithmic content. Persisting the renditions and the admin upload route that calls `render` are
  follow-on wiring; keeping them out of this slice keeps the pure, heavily-tested transform separate
  from a storage decision.

**Rejected.**

- **Hand-rolling resize/JPEG** — rejected by [ADR-0007](0007-in-house-vs-dependency.md): image codecs
  are the canonical buy-not-build, and a hand-rolled encoder is a security and correctness liability
  for zero benefit.
- **PNG or WebP output** — PNG is far larger for photographic content at the same quality, blowing the
  budget; WebP would encode smaller still but pulls a heavier codec and buys little over a
  budget-fitted JPEG for a menu thumbnail. JPEG is the right floor.
- **A dimension-only target** (resize to N px, accept whatever bytes result) — rejected: the roadmap's
  budget is in *bytes*, and a busy image at a fixed dimension can blow a byte budget a plain image
  meets. Fitting bytes needs the quality/dimension ladder.
- **The full `image` feature set** — rejected: it pulls every codec (GIF, TIFF, BMP, WebP, …) the menu
  pipeline never needs, enlarging the dependency and `cargo-deny` surface for nothing. Minimal
  features keep the tree to what the pipeline uses.

**Consequences.**

- `image` (0.25, `png`+`jpeg`) joins `pos-cloud`. It is a real codec tree, but a bounded one, and
  `cargo-deny` (advisories, bans, licenses, sources) passes with no new entry.
- The transform is pure and unit-tested without I/O: a synthetic image in gives two renditions that
  both decode as valid JPEG and are within their byte budgets, the thumbnail fits its pixel bound, and
  malformed input is a clean `Decode` error rather than a panic.
- **Storage and the upload route are the next slice.** Where renditions live — a Postgres `bytea`
  table rather than the `blob-garage` port, which [ADR-0031](0031-cloud-adapter-transports.md)
  schedules for deletion — and the admin route that accepts an upload, calls `render`, and stores the
  output, build on this pure pipeline behind a seam.

## Amendment 1 — where the render runs, and how many at once (2026-09-16)

**Status** Accepted · **Owner** @maintainers-cloud

This ADR ends with "the admin route that accepts an upload, calls `render`, and stores the output" as
the next slice. That slice shipped, and it called `render` directly from the async handler. The
transform is unchanged — this amendment is about the thread it runs on and how many run together.

**What was wrong.** `render` is the most expensive thing the cloud does per request: a decode, then up
to five Lanczos3 resizes and JPEG encodes per rendition, twice. Awaited inline it holds a Tokio worker
for the whole walk, and a worker that is busy is not polling anything else scheduled on it — so one
upload delayed unrelated requests that happened to share a thread. Nothing was wrong with the
pipeline; it was in the wrong place.

**The amendment.**

- The handler calls `images::render_blocking`, which hands the walk to the blocking pool. `render`
  itself stays exactly what this ADR decided: pure, synchronous, unit-tested without I/O. The async
  wrapper is the only new surface, and it is where the pool and the cap live, so nothing above it has
  to remember either.
- **A concurrency cap, because the pool is not one.** Tokio's blocking pool is hundreds of threads; a
  browser uploading a folder of photos would put every one of them on a core that does not exist,
  each holding a decoded bitmap. A process-wide semaphore, sized to
  `available_parallelism()` clamped to 1–8, bounds how many images decode at once to the cores the
  process actually has — the cgroup quota in a container, not the host's core count.
- **A full queue is a `RESOURCE_EXHAUSTED`, not a failure.** An upload waits five seconds for a slot
  before the server says it is busy, so an ordinary burst queues rather than producing a row of red
  toasts, and the refusal — when it comes — is the retryable status
  ([ADR-0096](0096-unprocessable-status.md)) rather than a 500 that reads as "your image
  is broken".

**What this does not bound.** The cap bounds how many images are decoded at once, not how large a
decoded image may be. The 8 MB body limit bounds the *upload*, and a JPEG of that size can still
declare a very large pixel count, so a deliberate decompression bomb is a memory spike this cap
divides by eight rather than prevents. Refusing an image by its pixel dimensions is a product
decision — it decides which real photographs stop working — and is recorded here as open rather than
smuggled in beside a threading change.
