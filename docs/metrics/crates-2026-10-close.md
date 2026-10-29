# Phase 6's crates, at close-out, October 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-10-15

This file records what phase 6's crate split cost and changed, once every crate ADR-0175 lists as a
move was done
([ADR-0170](../adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
[ADR-0175](../adr/0175-the-crate-list-follows-what-a-move-can-reach.md),
[plan](../plans/standardisation.md) row 6). The first four crates and decision gate 2 are in
[crates-2026-10.md](crates-2026-10.md). This file records one moment and is not regenerated; a
later measurement goes in a file of its own.

## What phase 6 did after gate 2

| Pull request | What it did | Size | Moved lines |
|---|---|---|---|
| #820 | The gate-2 figures ([crates-2026-10.md](crates-2026-10.md)) | docs only | — |
| #821 | ADR-0175: edge billing, shift and floor stay in `pos-edge-app`; catalog and fees are one crate; integrations and admin wait for a sub-state design | docs only | — |
| #822 | `pos-edge-app`: `Edge`, its commands, sign-in, the fan-out, and the clock, lease, queue and receipt authorities | 90 files, +308/−88 | 18,281 of 18,530 |
| #823 | `pos-edge-printing`: printing, the print-agent lane, its queue, the paper labels, and `ValueSource` with `log_in_force` | 53 files, +347/−127 | 8,269 of 8,505 |
| #824 | `pos-cloud-catalog`: the catalog with fees, media, tax rates, translations and the item CSV, their routes and stores | 63 files, +1,907/−1,591 | 16,684 of 17,063 |
| #825 | `pos-cloud-floor`: areas, tables, stations and routing, their publish, and the table QR codes | 34 files, +591/−398 | 3,304 of 3,510 |
| #826 | Preparing people: store groups and the tenant layout into `pos-cloud-config`, task health into `pos-cloud-api` | 19 files, +559/−524 | 1,235 of 1,316 |
| #827 | `pos-cloud-people`: employees, roles, assignments, their publish and republish queue, the capability flags | 39 files, +850/−610 | 6,193 of 6,457 |

- **Size** is `git diff --stat -M` of the squashed commit. A file moved unchanged counts as a
  rename, not as added lines.
- **Moved lines** is the share of added lines that `git diff --no-renames --color-moved` shows as
  moved.
- **The six code pull requests were move-only.** No route, permission, event, setting, OpenAPI
  entry or snapshot changed.
- **Tests:** the edge's 956 passing and 1 ignored tests, and the cloud's 1,153, were the same tests
  by name after each one.
- **Log targets:**
  - The edge's moved modules keep their `pos_edge::…` `tracing` targets through a `LOG_TARGET`
    constant per module. The durable log's exclusions (ADR-0117 decision 6) and telemetry's tests
    are unchanged.
  - The cloud has no target-based filter, so its moved modules log under their new crate's path.

## How it was measured

As at gate 2 ([crates-2026-10.md](crates-2026-10.md), "How it was measured"):

- **Machine and settings:**
  - one machine with 4 cores and `CARGO_BUILD_JOBS=2`;
  - no incremental compilation and no debug info (`CARGO_INCREMENTAL=0`,
    `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`);
  - rustc 1.94.1. mold is not installed, so it was not used.
- **Two sides, two worktrees, two target directories:**
  - "Before" is `main` at 4db130e, the commit #816 started from, so before any phase-6 crate.
  - "After" is `main` at 47ea4e4, after #827.
  - The sides ran one after the other, each in its own worktree with its own target directory.
    Both were deleted afterwards.
- **Warm-up:** every command below ran once on its side before any timed run, so a crate built alone
  had already built its dependencies for its own feature set.
- **The figures:**
  - Each one is the median of three runs, and each run follows a `touch` of the file named.
  - The three runs are in brackets.
  - Other sessions may share the machine. So a difference inside the bracketed ranges is noise.
- **The files touched:**

  | Area | Before | After |
  |---|---|---|
  | An edge bill command | `pos-edge/src/app/bills/settle.rs` | `pos-edge-app/src/app/bills/settle.rs` |
  | A printing document | `pos-edge/src/printing/documents.rs` | `pos-edge-printing/src/printing/documents.rs` |
  | A catalog route | `pos-cloud/src/http/catalog/items.rs` | `pos-cloud-catalog/src/routes/catalog/items.rs` |
  | A floor route | `pos-cloud/src/http/floor/tables.rs` | `pos-cloud-floor/src/routes/tables.rs` |
  | A people route | `pos-cloud/src/http/people/employees.rs` | `pos-cloud-people/src/routes/employees.rs` |
  | A handler outside the areas | `pos-cloud/src/http/reason_codes/authoring.rs` | the same file |

## Measurements

### Build and test times after one touch

| Touched | Measurement | Before | After |
|---|---|---|---|
| Edge bill command | `cargo build -p pos-edge-app` | — | **2.6 s** (2.5–2.8) |
| | `cargo build -p pos-edge` | 23.5 s (23.2–23.7) | 23.2 s (22.5–28.3) |
| | `cargo test -p pos-edge-app --no-run` | — | **7.1 s** (6.9–7.1) |
| | `cargo test -p pos-edge --no-run` | 138.8 s (137.0–141.4) | 133.5 s (126.2–135.0) |
| Printing document | `cargo build -p pos-edge-printing` | — | **1.4 s** (1.3–3.4) |
| | `cargo build -p pos-edge` | 25.3 s (22.4–26.3) | 21.2 s (20.8–22.6) |
| Catalog route | `cargo build -p pos-cloud-catalog` | — | **5.2 s** (5.1–5.4) |
| | `cargo build -p pos-cloud` | 62.7 s (61.9–68.2) | 59.5 s (59.2–73.2) |
| | `cargo test -p pos-cloud-catalog --no-run` | — | **7.8 s** (7.8–8.6) |
| Floor route | `cargo build -p pos-cloud-floor` | — | **1.4 s** (1.4–6.3) |
| | `cargo build -p pos-cloud` | 62.7 s (56.0–73.6) | 62.7 s (56.7–69.3) |
| | `cargo test -p pos-cloud-floor --no-run` | — | **2.2 s** (2.2–2.4) |
| People route | `cargo build -p pos-cloud-people` | — | **2.4 s** (2.3–2.5) |
| | `cargo build -p pos-cloud` | 73.7 s (71.8–84.6) | 61.1 s (57.2–63.6) |
| | `cargo test -p pos-cloud-people --no-run` | — | **2.9 s** (2.9–3.1) |
| | `cargo test -p pos-cloud --test cloud --no-run` | 87.7 s (82.8–93.8) | 69.6 s (67.1–75.9) |
| Handler outside the areas | `cargo build -p pos-cloud` | 75.0 s (71.5–76.6) | 59.1 s (52.3–59.9) |

Two notes on these figures:

- **The 6.3 s and 3.4 s runs.** The first timed run of `pos-cloud-floor` and `pos-edge-printing`
  each took longer than the two after it. The bracket keeps it; the median does not.
- **Rebuild scope.** Touching a file in an area crate rebuilds that crate and every crate above it.
  The `pos-edge` and `pos-cloud` figures for an area touch include all of them.

### What a change means opening

These are the files a person or an agent opens to change one thing in the area: the handler or
command, the area's route module or command module, its records and store trait, and the impl that
persists them. For the edge, the command, the area module, the records it writes and `Edge`. The
`store-postgres` and `store-sqlite` table files are the same on both sides and are left out.

| Change | Before | After |
|---|---|---|
| A bill's settle (edge) | 1,099 lines; largest file 412 (`app/mod.rs`) | 1,101 lines; largest file 413 (`app/mod.rs`) |
| A printed document (edge) | 927 lines; largest file 465 (`printing/documents.rs`) | 927 lines; largest file 465 |
| A catalog item write | 8,358 lines; largest file 5,750 (`persistence.rs`) | **3,690**; largest file 2,018 (`catalog.rs`) |
| A table write | 6,635 lines; largest file 5,750 (`persistence.rs`) | **1,239**; largest file 455 (`floorplan.rs`) |
| An employee write | 7,665 lines; largest file 5,750 (`persistence.rs`) | **2,467**; largest file 1,070 (`people.rs`) |

The edge's files did not change in this measure:
- the edge's `app/` and `printing/` were already split into modules by area before the crates
  (the last of those pull requests were #808 and #809);
- the edge had no shared persistence file to leave behind;
- so the crates moved the same files without making any of them smaller.

### What CI runs

These lines are the output of `cargo xtask affected --crates` for a one-line change to the touched
file. Each side was measured on its own commit.

| Change | Before | After |
|---|---|---|
| An edge bill command | `-p minimal-edge -p pos-edge` | `-p minimal-edge -p pos-edge -p pos-edge-app -p pos-edge-printing` |
| A printing document | `-p minimal-edge -p pos-edge` | `-p minimal-edge -p pos-edge -p pos-edge-printing` |
| A catalog route | `-p pos-cloud` | `-p pos-cloud -p pos-cloud-catalog -p pos-cloud-floor -p pos-cloud-people` |
| A floor route | `-p pos-cloud` | `-p pos-cloud -p pos-cloud-floor` |
| A people route | `-p pos-cloud` | `-p pos-cloud -p pos-cloud-people` |
| A handler outside the areas | `-p pos-cloud` | `-p pos-cloud` |

### Crate sizes

Source lines are every `.rs` file under the crate's `src/`, at each commit.

| Crate | Before (4db130e) | After (47ea4e4) |
|---|---|---|
| `pos-edge` | 54,738 (160 files) | 28,221 (67 files) |
| `pos-edge-app` | — | 18,344 (67 files) |
| `pos-edge-printing` | — | 8,335 (29 files) |
| `pos-cloud` | 88,829 (235 files) | 49,417 (146 files) |
| `pos-cloud-api` | 8,000 (19 files) | 9,171 (24 files) |
| `pos-cloud-config` | — | 6,122 (16 files) |
| `pos-cloud-catalog` | — | 16,838 (38 files) |
| `pos-cloud-floor` | — | 3,338 (13 files) |
| `pos-cloud-people` | — | 6,258 (16 files) |
| `pos-cloud-devices` | — | 4,059 (13 files) |
| `pos-cloud-inventory` | — | 2,376 (8 files) |
| `pos-cloud-reports` | 3,789 (14 files) | 3,789 (14 files) |

The edge crates come to 54,900 lines after, 162 more than before. The cloud crates come to 101,368,
750 more than before. The difference is new `lib.rs` files, imports, module headers, rewrapped doc
comments, `# Errors` sections and `Debug` impls, and the edge's log-target constants. Manifests and
READMEs are not counted.

### Tests

These are cited from each pull request's hand-back, not re-run for this file.

| Side | Edge | Cloud |
|---|---|---|
| Before | 956 passed, 1 ignored: `pos-edge` lib 493, binary 6, 57 integration-test files | 1,153: `pos-cloud` lib 559, `tests/cloud.rs` 444, `pos-cloud-api` 112, `pos-cloud-reports` 38 |
| After | 956 passed, 1 ignored: `pos-edge` lib 262, `pos-edge-app` 145, `pos-edge-printing` 86, the binary and the 57 integration-test files unchanged | 1,153: `pos-cloud` lib 337, `tests/cloud.rs` 444, `pos-cloud-api` 128, `pos-cloud-config` 71, `pos-cloud-catalog` 91, `pos-cloud-floor` 16, `pos-cloud-people` 15, `pos-cloud-devices` 11, `pos-cloud-inventory` 2, `pos-cloud-reports` 38 |

## Findings

1. **The edit loop moved for every area that became a crate.**
   - A change inside one of the five new crates builds that crate in 1.4–5.2 s.
   - Its tests build in 2.2–7.8 s.
   - The binary above it takes what it took before: `pos-edge` about 21–23 s, `pos-cloud` about
     59–63 s.
2. **`pos-cloud` builds faster for every touch measured, but only partly beyond noise.**
   - The two touches whose ranges do not overlap:
     - a handler outside the areas: 75.0 s → 59.1 s;
     - `--test cloud --no-run` after a people touch: 87.7 s → 69.6 s.
   - A people touch is 73.7 s → 61.1 s, with ranges that overlap by under a second.
   - The catalog touch (62.7 s → 59.5 s) and the floor touch (62.7 s both) are inside the noise.
   - `pos-cloud` lost 39,412 lines across the phase. These figures do not say which part of the
     drop that accounts for.
3. **The edge's test build is still the slowest thing measured.**
   - `cargo test -p pos-edge --no-run` takes 133.5 s after an edge bill touch, against 138.8 s before.
   - It builds the library's tests and 57 integration-test binaries, each linked on its own.
   - `pos-cloud`'s single integration-test binary, 444 tests, builds in 69.6 s after a touch.
4. **What a change means opening shrank where a shared file was left behind.**
   - A catalog, table or employee write opened 6,635–8,358 lines, always including pos-cloud's
     5,750-line `persistence.rs`.
   - It now opens 1,239–3,690 lines, and the largest file is 455–2,018 lines.
   - The edge's figures did not change (see above).
5. **`affected` widens most where crates sit on each other.**
   - A catalog change now runs four crates, because floor and people depend on the catalog.
   - An edge-app change runs printing too.
   - A handler outside the areas still runs `pos-cloud` alone.
   - CI ran everything on each move PR anyway, because the manifests, the lockfile and `CODEOWNERS`
     changed.
6. **The route tests still live in `pos-cloud`.**
   - Every area's HTTP tests drive the whole router. So they stayed in `pos-cloud/tests/cloud.rs`,
     444 tests, unchanged. The edge's route tests stayed in `pos-edge/tests/`.
   - Checking a route still means the binary's test build: 69.6 s for the cloud, 133.5 s for the edge.
7. **What the moves needed besides the moves.** Across #822–#827, these were the only non-move
   lines:
   - imports naming where a helper now lives;
   - visibility widened to `pub` where the binary still calls an item;
   - the `# Errors`, `#[must_use]` and `Debug` lines clippy's pedantic lints then ask for;
   - doc links into the binary written as code;
   - `ROUTE_CRATES` entries;
   - the edge's log-target constants;
   - the size baseline's paths.

   Three of the six PRs needed a `size-baseline: grow` line, because the ratchet recognises a
   moved function but not a moved file.

## Recommendation

- **Phase 6's moves are done.** Every crate ADR-0175 lists as a move is a crate. What remains is
  the two areas ADR-0175 decision 3 holds back:
  - **`pos-cloud-integrations`:** its routes are 1,439 lines, plus `webhook/` at 3,259. Four of its
    webhook handlers take `CloudApp`.
  - **`pos-cloud-admin`:** `http/admin.rs` and `http/admin/` are 2,020 lines. Twenty-one of its
    handlers take `CloudApp`.

  Neither is a move today. Each needs the written sub-state design first: the `FromRef` state it
  takes, the store fields it carries, where `webhook/` goes, and whether admin folds into
  `pos-cloud-api`. Until then, both stay in `pos-cloud`.
- **The `pos-edge` test-binary merge is worth one measured trial, as its own pull request.**
  - The edge's test build (133.5 s) is the largest figure in this file. It is the one an edge
    change pays in CI, since `affected` runs `pos-edge` for every edge-crate change.
  - **Estimate, not measured:** about 110 s of it goes to building and linking the 57
    integration-test binaries. That is the test build (133.5 s) minus the library build after the
    same touch (23.2 s).
  - `pos-cloud` already uses the single-binary shape (`tests/cloud.rs` with modules beneath it).
  - The trial would:
    - fold `pos-edge/tests/*.rs` into one binary with one module per file;
    - keep the same 956 passing and 1 ignored tests by name;
    - measure the touched test build before and after, as here.
  - It should merge only if the figure drops by more than the bracketed noise.

## Orchestrator

Reported by the orchestrator, which reviewed and merged each pull request.

- **Pull requests:** #816 to #827, eleven move pull requests plus the gate-2 figures (#820) and
  ADR-0175 (#821). Each merged on its first green CI run (24 of 24 checks, docs-only ones 9), five
  to ten minutes after it opened. One needed a second push: #823's generated ADR index was out of
  date because the check had run before the new README was committed.
- **Review:** each move was checked by listing every added code line that is not, after removing
  indentation and visibility, already a line of the crate it came from. What remained in every pull
  request was imports, `mod` lines, rustfmt rewraps, the `ErrorResponse` path, the `ROUTE_CRATES`
  list, log-target constants on the edge, and two hand-written `Debug` impls.
- **Sub-agents:** two, one after the other: the first did the survey, #816 to #821 and the gate-2
  figures; the second did #822 to #827 and these figures, resumed between steps.

**Phase 6 is closed** on these figures. Integrations and admin stay in `pos-cloud` until their
sub-state design is written (ADR-0175 decision 3). The edge test-binary merge goes to the backlog as
a measured trial.

