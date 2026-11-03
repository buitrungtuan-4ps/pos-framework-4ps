# Spike: CEL on both sides (throwaway, never merged)

`cel` 0.15.0 (`default-features = false`: no regex, chrono or base64), rustc 1.94.1. The machine is
a 4-core Xeon @ 2.10 GHz container, and the browser is headless Chromium 153.

The environment is `Env::default().with_stdlib()`, which has no macros, plus a hand-written `has()`
macro (about 10 lines, public API only). The guards are run against a fake order. The code is in
`src/lib.rs`, `src/bin/native.rs` and `bench-browser.cjs`.

## Numbers (µs per call, mean)

| Guard | Native compile | Native eval | Native ctx+eval | Chromium 1× eval | 1× ctx+eval | Chromium 4× eval | 4× ctx+eval |
|---|---|---|---|---|---|---|---|
| `order.total >= 500000 && order.channel == 'DINE_IN'` | 14.0 | 0.41 | 4.2 | 1.00 | 7.6 | 3.3 | 33.9 |
| `order.channel in ['DINE_IN','TAKEAWAY'] && 'cat-drinks' in order.categories` | 21.9 | 0.44 | 4.5 | 0.64 | 6.4 | 3.2 | 28.9 |
| `has(order.guest_count) && order.guest_count > 4` | 17.4 | 0.39 | 4.5 | 0.71 | 6.8 | 2.6 | 31.6 |
| `!has(order.member_id) \|\| order.total - order.discount >= 100000` | 20.2 | 0.20 | 3.9 | 0.34 | 6.8 | 1.4 | 27.8 |
| `size(order.items) >= 3 && order.items[0].qty > 1 && order.table.startsWith('T')` | 35.5 | 0.98 | 5.0 | 1.83 | 8.7 | 6.2 | 31.7 |

- **Compile in Chromium:** 33–64 µs at 1× and 150–300 µs at 4×. This is done once per package
  version, not per tap.
- **Boot:** instantiating the wasm takes 6 ms at 1× and 9 ms at 4×. `setup()` builds the
  environment and stdlib and compiles 5 guards: 27 ms at 1× and 109 ms at 4×. That must happen at
  boot or on a package switch, never in a tap.
- **Size:** at `opt-level=3`, LTO and `strip`, the wasm is 1,019,969 B raw and **293 KB gzip -9**.
  At `opt-level=z` it is 810 KB raw and **237 KB gzip**, with eval about 1.5–2× slower (still under
  15 µs at 4×). Today's whole till bundle is 111 KB gzip, so CEL would roughly triple the download.
  It is cached, but it is the largest single cost found. `wasm-opt` is not installed here, so its
  effect was not measured.
- No imports: plain `WebAssembly.instantiate(bytes, {})`, no wasm-bindgen. The `wasm32` target
  installed through the proxy without trouble.

**Verdict:** even 10 guards per tap at 4× is under 0.4 ms on the tap path. Evaluation speed is not
a risk for 10 ms. **Building the input** dominates: making the `order` value costs 5–10× the
evaluation, and that was a Rust-side fake. Moving a real order from JS into wasm (a JSON string and
`serde_json`, i.e. the `json` feature) was not measured, and it is the next number to get.

## What the crate does or does not do (probes in `src/bin/native.rs`)

- **Loop macros are rejected only at run time.** Without the macros,
  `order.items.all(i, i.qty > 0)` compiles and fails at run time with `UndeclaredReference("i")`.
  The console and the edge need a compile-time walk that rejects `all`, `exists`, `exists_one`,
  `map` and `filter` (and anything not on an allow-list) before publish.
- **No type checker.** The crate parses and interprets. A wrong field (`order.member_id` → `NoSuchKey`)
  or `'a' + 1` (`UnsupportedBinaryOperator`) surfaces only when a guard runs. Publish-time
  validation must run each guard against a typed sample of every object, or a checker must be
  written.
- **Doubles exist.** `double(order.total) * 1.1` works. A money guard needs a lint that forbids
  `double()` and float literals next to money fields (AGENTS.md: integer money).
  `order.total * 1.1` is a run-time error, not a silent float, which is good.
- **Safe failures:** division by zero, `i64` overflow and an index out of range are all errors,
  not panics or wraps. The parser stops at a recursion depth of 96, and 200-deep parentheses are a
  syntax error. The 512-character source limit is ours (`MAX_SOURCE_LEN`), because the crate has
  none.
- **Missing without default features:** `matches()` (regex), `timestamp()` and `duration()`
  (chrono). Time-slot rules need either the `chrono` feature (adds chrono and chrono-tz, a larger
  wasm) or a till-supplied `now_minute_of_day` integer. The second fits ClockSource better.
- **Dependencies pulled in:** `antlr4rust` (the ANTLR runtime, most of the wasm size), `nom`,
  `lazy_static`, `pastey`, `serde` and `thiserror`. Adding `cel` to the workspace needs cargo-deny
  and licence review (MIT). This spike adds nothing to the workspace.
- `has()` on a map field works with a hand-written macro over public types (`Macro::global`,
  `Expr::Select { test: true }`).
