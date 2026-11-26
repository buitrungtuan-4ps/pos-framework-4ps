# ADR-0253 — The console prints each table's QR code with the till's encoder

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (phase 10 open item: no printable table QR) · Adds a front-end
dependency (`AGENTS.md` §7) · Follows [ADR-0139](0139-the-till-draws-the-pairing-code-as-a-qr.md) ·
Relates to [ADR-0224](0224-self-order-is-a-guest-page-and-a-kiosk.md)

## Context

The console's Floor screen loads each active table's signed QR token, and a table's guest address is
`<cloud>/qr/#t=<token>` (ADR-0224 decision 2). The screen shows the token as text only, so nobody can
print a code to stand on a table, which the pilot needs. The till already draws QR codes with
`qrcode-generator` 2.0.4 (ADR-0139); the console has no encoder.

## Options considered

1. **Write an encoder in the console.** Reed-Solomon and masking by hand, for one screen; refused by
   ADR-0007's rule on what we write ourselves.
2. **Have the cloud render PNGs.** A Rust dependency and a new route for an image the browser can draw.
3. **Use the till's encoder in the console.** The same package, version and licence the repo already
   reviewed (MIT, no runtime dependencies).

## Decision

1. **Option 3.** `qrcode-generator` 2.0.4 becomes a dependency of `dashboard/`, pinned in its lockfile,
   **loaded on demand** with a dynamic `import()` by the Floor screen's QR sheet only, so the console's
   main bundle does not grow.
2. **What a code holds:** the table's guest address, `<the console's own origin>/qr/#t=<token>`, the
   same text printed beside it with the table's name, so a phone without a camera can type it.
3. **Printed by the browser:** the sheet is drawn as SVG, one card per table, and **Print** opens the
   browser's print dialog with only the sheet on the page.

## Consequences accepted

- One more package in `dashboard/pnpm-lock.yaml`, the same one `ui/` holds; a version bump moves both.
- A printed code names a token signed with the cloud's table-token secret: rotating that secret
  makes every printed code stale, and the sheet must be printed again.
