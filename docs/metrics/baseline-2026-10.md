# Baseline, October 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-10-13

The "before" figures for the standardisation work
([ADR-0170](../adr/0170-repository-layout-code-naming-and-size-rules.md),
[ADR-0171](../adr/0171-ci-runs-what-a-change-affects.md)). Each later phase reports against them.
This file is a record of one moment: it is not regenerated, and a later measurement goes in a file
of its own.

**Measured at** `1aa8e40` on `main` (after #700), unless a section says otherwise. The next commit
on `main`, `b6a6809` (#701), adds only the two ADRs above, so its code measures the same.

## 1. Code

Produced by `cargo xtask code-stats --root <checkout of 1aa8e40>`.

How it counts:

- **Files:** every `.rs` file outside `target/`, `vendor/` and `node_modules/`. That includes
  `xtask/`, `examples/` and `apps/pos-station/src-tauri/`. A test file is one under a `tests/` or
  `benches/` directory.
- **Functions:** every `fn` with a body, test functions included. "Outside test code" leaves out
  test files and everything below a file's first `#[cfg(test)]`.
- **Lengths** run from the `fn` line to the closing brace.
- **Parameters** include `self`, as clippy's `too_many_arguments` counts them.

These are wider counts than the roadmap's quick survey of 2026-10-08 (497 files, 8,138 functions,
653 over 40 lines, 55 over 100), which did not count test code the same way. Later phases compare
against the tool's own figures below, never the survey's.

### Rust files

| | Files | Lines | > 500 | > 800 | > 1,000 | > 1,200 | > 1,500 |
|---|---|---|---|---|---|---|---|
| Source | 425 | 235116 | 100 | 47 | 30 | 25 | 16 |
| Test (`tests/`, `benches/`) | 101 | 83409 | 19 | 7 | 7 | 6 | 5 |

The largest ten:

| File | Lines |
|---|---|
| `crates/pos-cloud/src/http.rs` | 38555 |
| `crates/pos-cloud/tests/cloud.rs` | 36890 |
| `crates/pos-edge/src/app.rs` | 12322 |
| `crates/adapters/store-postgres/tests/integration.rs` | 11673 |
| `crates/pos-cloud/src/persistence.rs` | 6439 |
| `crates/pos-edge/src/printing.rs` | 6410 |
| `crates/pos-core/src/billing.rs` | 3800 |
| `crates/pos-cloud/src/auth/admin.rs` | 3468 |
| `crates/pos-edge/src/config_client.rs` | 3437 |
| `crates/pos-edge/tests/acceptance.rs` | 2564 |

### Rust functions

| | Functions | > 40 lines | > 60 | > 100 | Longest | > 5 params | > 7 |
|---|---|---|---|---|---|---|---|
| All | 11036 | 1264 | 618 | 115 | 1168 | 181 | 46 |
| Outside test code | 5035 | 491 | 230 | 47 | 1168 | 145 | 42 |

The longest ten:

| Function | Lines | Params |
|---|---|---|
| `run` in `crates/pos-cloud/src/main.rs:65` | 1168 | 0 |
| `session_from_config` in `crates/pos-edge/src/config_client.rs:166` | 546 | 2 |
| `register` in `crates/pos-proto/src/settings.rs:269` | 475 | 0 |
| `migrate` in `crates/adapters/store-postgres/src/store.rs:409` | 347 | 1 |
| `holds` in `crates/pos-cloud/tests/assignment_contract/mod.rs:87` | 286 | 3 |
| `a_wider_assignment_reaches_through_its_group_and_its_tenant` in `crates/adapters/store-postgres/tests/integration.rs:6720` | 283 | 0 |
| `people_routes_crud_lifecycle_audited_without_pii` in `crates/pos-cloud/tests/cloud.rs:21274` | 263 | 0 |
| `run` in `crates/adapters/store-sqlite/src/writer.rs:483` | 252 | 2 |
| `compose` in `crates/pos-edge/src/server.rs:684` | 252 | 8 |
| `granted_once_on_the_drawers_terms` in `crates/adapters/store-postgres/tests/integration.rs:7186` | 247 | 3 |

### Workspace crates

4 of 30 have a `README.md`.

| Crate | Path | Rust lines | README |
|---|---|---|---|
| `pos-country-in` | `countries/in` | 387 | yes |
| `pos-country-jp` | `countries/jp` | 354 | yes |
| `pos-country-vn` | `countries/vn` | 314 | yes |
| `pos-country-zz` | `countries/zz` | 341 | yes |
| `blob-garage` | `crates/adapters/blob-garage` | 962 | no |
| `cloud-sync-http` | `crates/adapters/cloud-sync-http` | 2088 | no |
| `erp-sap` | `crates/adapters/erp-sap` | 846 | no |
| `key-vault-keyring` | `crates/adapters/key-vault-keyring` | 1203 | no |
| `link-nats` | `crates/adapters/link-nats` | 1347 | no |
| `metrics-vm` | `crates/adapters/metrics-vm` | 568 | no |
| `printer-escpos` | `crates/adapters/printer-escpos` | 959 | no |
| `shipping-ahamove` | `crates/adapters/shipping-ahamove` | 984 | no |
| `shipping-grabexpress` | `crates/adapters/shipping-grabexpress` | 964 | no |
| `store-postgres` | `crates/adapters/store-postgres` | 25724 | no |
| `store-sqlite` | `crates/adapters/store-sqlite` | 7063 | no |
| `updater-minisign` | `crates/adapters/updater-minisign` | 240 | no |
| `pos-cloud` | `crates/pos-cloud` | 132871 | no |
| `pos-contract-tests` | `crates/pos-contract-tests` | 6915 | no |
| `pos-core` | `crates/pos-core` | 13049 | no |
| `pos-country` | `crates/pos-country` | 706 | no |
| `pos-edge` | `crates/pos-edge` | 79428 | no |
| `pos-fakes` | `crates/pos-fakes` | 3207 | no |
| `pos-ports` | `crates/pos-ports` | 6301 | no |
| `pos-print-agent` | `crates/pos-print-agent` | 1747 | no |
| `pos-proto` | `crates/pos-proto` | 18874 | no |
| `pos-providers` | `crates/pos-providers` | 1057 | no |
| `pos-render` | `crates/pos-render` | 2410 | no |
| `pos-simulator` | `crates/pos-simulator` | 810 | no |
| `minimal-edge` | `examples/minimal-edge` | 122 | no |
| `xtask` | `xtask` | 2879 | no |

### Front-end roots

| Root | Files | Lines | > 300 | > 500 | > 1,000 | Largest |
|---|---|---|---|---|---|---|
| `ui` | 56 | 20279 | 11 | 11 | 5 | `ui/tests/replay.spec.mjs` (4312) |
| `dashboard` | 175 | 54861 | 49 | 30 | 11 | `dashboard/src/api/client.ts` (2665) |
| `apps/pos-station/ui` | 6 | 584 | 0 | 0 | 0 | `apps/pos-station/ui/i18n.js` (202) |


## 2. CI

From the GitHub check runs of the twelve pull requests merged before this one, #670 to #681, all
on the 10-minute test timeout that #700 has since raised to 15. Each pull request ran all 13 jobs.

| Pull request | `test` job | Longest job | First start to last finish | Job minutes |
|---|---|---|---|---|
| #681 | 9:23 | 9:23 (`test`) | 9:24 | 36 |
| #680 | 9:28 | 9:28 (`test`) | 26:59 ¹ | 39 |
| #679 | 9:16 | 10:58 (`replay`) | 10:59 | 42 |
| #678 | 7:33 | 7:33 (`test`) | 7:34 | 36 |
| #677 | 7:29 | 7:29 (`test`) | 7:29 | 36 |
| #676 | 9:31 | 9:31 (`test`) | 9:32 | 42 |
| #675 | 7:44 | 8:58 (`replay`) | 8:58 | 38 |
| #674 | 5:49 | 7:54 (`replay`) | 7:55 | 37 |
| #673 | 6:20 | 7:31 (`replay`) | 7:31 | 38 |
| #672 | 9:11 | 9:11 (`test`) | 34:38 ¹ | 38 |
| #671 | 6:02 | 13:04 (`replay`) | 13:04 | 41 |
| #670 | 8:52 | 11:26 (`replay`) | 11:27 | 42 |
| **Median** | **8:18** | **9:17** | **9:28** | **38** |
| **Max** | **9:31** | **13:04** | **34:38** ¹ | **42** |

¹ Includes a re-run started later, so the span is waiting rather than work. The longest job is the
better measure of how long a pull request takes.

Per job, over the same twelve runs:

| Job | Median | Max |
|---|---|---|
| `test` | 8:18 | 9:31 |
| `replay` | 7:42 | 13:04 |
| `build (windows-2022)` | 5:10 | 5:59 |
| `console-replay` | 3:57 | 6:47 |
| `deny` | 2:29 | 3:00 |
| `lints` | 2:23 | 2:29 |
| `postgres` | 2:17 | 3:01 |
| `dashboard` | 2:15 | 2:36 |
| `build (ubuntu-24.04)` | 2:01 | 2:33 |
| `rules` | 1:02 | 1:10 |
| `ui` | 0:18 | 0:22 |
| `secrets` | 0:07 | 0:11 |
| `docs-gate` | 0:07 | 0:09 |

## 3. Local build

**Machine:** 4 cores (`nproc`), Intel Xeon at 2.80 GHz, 15 GB of memory; Rust 1.94.1.

**Settings:**
- `CARGO_BUILD_JOBS=2`;
- `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`, as every agent build here runs;
- a target directory emptied with `cargo clean` before the first build.

| Step | Time | Recompiled |
|---|---|---|
| Clean `cargo build -p pos-cloud` | 4:02 | everything |
| The same, after touching `crates/pos-cloud/src/http.rs` | 1:32 | `pos-cloud` |
| The same, after touching `crates/pos-core/src/billing.rs` | 1:27 | `pos-core`, `pos-cloud` |
| `cargo test -p pos-cloud --no-run`, the first time after the build above | 1:18 | `pos-cloud`'s test targets |
| `cargo test -p pos-cloud --no-run`, after touching `http.rs` | 2:04 | `pos-cloud` and its test targets |

Touching a one-line leaf in `pos-core` costs about as much as touching `http.rs`, because almost all
of it is recompiling `pos-cloud`, one crate of 132,871 lines.

## 4. AI tokens

As the orchestrator reported them, for one agent's work on each change:

| Change | Tokens | Tool calls |
|---|---|---|
| ADR-0168 slice 3 (pos-edge, shift report) | 778,060 | 64 |
| ADR-0168 slice 4 (pos-cloud, rollup and export) | 382,292 | 142 |
| ADR-0168 slice 5 (dashboard) | 529,071 | 67 |
| The slice 5 caret fix (a few lines) | 556,857 | 16 |

The last row is the point: a change of a few lines still cost over half a million tokens, mostly to
reload the context around it.

## 5. Duplication

| What | Size | Why it matters |
|---|---|---|
| TypeScript types copied by hand from Rust | `dashboard/src/api/types.ts` 2,252 lines; `ui/src/api/types.ts` 901 | Nothing reports a drift between backend and frontend |
| The console's API client | `dashboard/src/api/client.ts` 2,665 lines | One function per route, written by hand |
| Admin routes outside the OpenAPI document | 151 in `UNDOCUMENTED` (`crates/pos-cloud/src/openapi_admin.rs`) | Blocks generating types from OpenAPI |
| Files mirrored between `ui/` and `dashboard/` | 4 pairs: `tokens.css`, `wcag-contrast.mjs`, `i18n-lint.mjs`, `i18n-parity.mjs` | Edited twice; `cargo xtask mirrored-files` keeps them equal |
| `CHANGELOG.md` | 1.24 MB (1,235,636 bytes), 15,419 lines; 184 commits since `v0.14.0` | Conflicts between any two open pull requests |
