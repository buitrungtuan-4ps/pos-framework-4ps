# ADR-0102 — A store draws the text its printer cannot spell

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Closes production-readiness **P9** (a Vietnamese kitchen ticket cannot be printed)
· Extends [ADR-0100](0100-receipt-and-ticket-printing.md)
· Relates to [ADR-0026](0026-port-shapes.md) §5, `docs/pos-spec.md` §13

## The problem

A store cannot print its own menu.

`pos_edge::printing::dispatch` checks every line of a document against the printer's code page and
returns `Unprintable` if any line fails. For a Vietnamese menu that is every ticket and every
receipt: the till reports `UNPRINTABLE_TEXT`, the kitchen display carries the order, and no paper
comes out. Japanese and Indic menus are in the same position, and worse.

The check itself is right. `pos-spec.md` §13 and ADR-0026 §5 both say a line outside the code page
must become a bitmap, and sending the bytes anyway prints a row of question marks in front of a
customer — which, on a kitchen ticket, is how the wrong dish gets made. What was missing is the
other half: **nothing in the tree could produce a bitmap.** `PrintBlock::Bitmap` had an encoder and
no producer, and `PrinterCapabilities::prints_bitmaps` was hard-coded `false` with a comment saying
why.

## Why not just use the printer's own character set

This is the question worth answering carefully, because "make the text print as text" sounds like
the simpler fix and for two of the three scripts it is not a fix at all.

An ESC/POS printer renders text from bitmap fonts in its firmware ROM, selected by a **code page** —
a single-byte encoding, so at most 256 characters at a time, of which the first 128 are ASCII.

- **Vietnamese** needs 134 precomposed letters beyond ASCII. CP1258 exists and encodes Vietnamese,
  but as a *combining* page — the tone mark is a separate byte composed over the vowel — and support
  for it is partial and inconsistent across the models a fleet actually contains. A printer that
  half-supports it prints half the menu correctly, which is worse than a printer that prints none of
  it, because nobody notices until a customer does.
- **Japanese** needs a JIS ROM holding several thousand kanji. Only Japanese-market printers carry
  one. A printer bought in Ho Chi Minh City does not have the glyphs, and no configuration adds them.
- **Indic scripts** cannot be done by a code page **in principle**, not just in practice. In
  Devanagari the vowel sign of a syllable is written *before* the consonant it follows, consonants
  combine into conjunct glyphs that are not any of their parts, and marks reorder around the cluster.
  That is text *shaping*, not text *encoding*. A byte stream in any code page cannot express it.

So a code page is a per-script, per-model optimisation that covers the easy case and cannot be made
to cover the general one. Rasterising covers every script on every printer that accepts `GS v 0` —
which is all of them, because it is how logos have been printed since the format existed.

## The decision

**The framework rasterises. Text mode stays as the fast path where it is sufficient.**

1. A new crate, `pos-render`, shapes a line and draws it to one bit per pixel. Shaping is
   [`rustybuzz`](https://github.com/harfbuzz/rustybuzz), the HarfBuzz algorithm ported to Rust: it
   carries the Indic, Arabic, Khmer and Myanmar shapers and the OpenType mark positioning that
   stacks a Vietnamese tone correctly over a vowel that already has a diacritic. Outlines come from
   `ttf-parser`, which `rustybuzz` already uses, so there is one font parser in the tree.
2. `pos_edge::printing::prepare` replaces the whole-document refusal. A line the printer's code page
   covers is still sent as `Text`; a line it does not is rendered and sent as `Bitmap`. **Both halves
   matter** — rasterising an ASCII receipt would be correct and would be slower for nothing, since
   text is a fraction of the bytes and comes out of the head faster.
3. `PrinterCapabilities` gains `dots_per_line`, because a raster wider than the print head does not
   clip — it wraps and shears — and the width is not derivable from `columns`.
4. `prints_bitmaps` now describes **this box**, not the printer. `GS v 0` is universal, so the
   hardware was never the constraint; the framework's ability to produce a raster is, and that
   depends on whether a font is installed.

### Everything after the font parser is integer arithmetic

Two stores must render the same receipt to the same bytes. Floating point does not promise that: the
same expression can round differently across targets and optimisation levels, and a rounding
difference at a coverage threshold flips a pixel, which changes the raster, which changes what goes
on the wire and what a snapshot compares.

So `pos-render` works in 16.6 fixed point. Curves are flattened by exact integer evaluation, coverage
is *counted* rather than measured, and a pixel prints when it is at least half covered. The workspace
already bans `f32` and `f64` for the neighbouring reason — money is an integer — and the one place a
float can be named is `ttf_parser::OutlineBuilder`, a trait this crate must implement. That is
documented in `crates/pos-render/clippy.toml`, which lifts the ban on the *type* while
`clippy::float_arithmetic` stays denied, so a float can be converted and cannot be computed with.

### Fonts are a deployment asset, not framework code

Embedding a font would ship a Vietnamese store several megabytes of kanji it will never print and
still not cover the next country. So the framework carries the machinery and the deployment supplies
the glyphs: `font_directories` in `config.toml`, defaulting to the platform's own font paths, which
is where the packages in `deploy/edge/README.md` install to.

The cost is a box that finds no font and can print only ASCII — the failure this ADR set out to fix,
reintroduced by a missed install step. It is paid for by making the state **loud and early**: the
edge reports at start-up which scripts it can and cannot print, by name. A missing package is then a
log line at boot rather than a kitchen ticket that never arrives during service.

A character no installed font covers prints as the font's own "no such character" box and is
reported. A ticket reading `Phở [] size L` is workable; a ticket that never printed is not.

## What this does not decide

- **Which font a store should use.** Any TrueType or OpenType face works. `deploy/edge/README.md`
  names packages that cover the pilot country, and a fork adds its own.
- **The paper width.** 576 dots (80 mm at 203 dpi) is assumed, matching the 42 columns already
  assumed. A 58 mm printer needs 384, and the real value should arrive from the console with the
  device — the same gap `ASSUMED_COLUMNS` already has, and the same fix closes both.
- **Serial baud rate**, which belongs to [ADR-0103](0103-directly-attached-printers.md).

## Consequences

- A store with a font package installed prints Vietnamese, Japanese, Chinese, Korean, Devanagari,
  Thai and Arabic — anything the installed faces cover, because nothing in the pipeline is
  script-specific.
- A store without one behaves exactly as it did before this ADR, and says so at boot.
- Seven crates enter the dependency tree (`rustybuzz`, `ttf-parser`, `unicode-script`, `unicode-ccc`,
  `unicode-bidi-mirroring`, `core_maths`, `libm`). All are MIT or Apache-2.0, all are pure Rust with
  no C toolchain — which is what keeps the edge cross-compiling to Windows and ARM — and none
  introduces a duplicate version.
- Rendering costs a few milliseconds per line and the raster is larger on the wire than text. Both
  are paid only on lines that could not have printed at all before.
