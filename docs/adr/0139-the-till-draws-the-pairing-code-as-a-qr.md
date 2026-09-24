# ADR-0139 — The till draws the pairing link as a QR code

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-23
· Adds a front-end dependency (`AGENTS.md` §7) · Builds the "pairing QR" [ADR-0030](0030-pairing-and-offline-auth.md)
and `dashboard/src/installers.mjs` describe and nothing drew

## The problem

A new tablet joins a store by opening `http://<store-pc>:<port>/pair?code=NNNNNN`. A manager mints the
code on the Devices screen, and the screen shows the link and the code as text — so the person holding
the tablet types an IP address, a port, a path and six digits on a touch keyboard, against a code that
expires in five minutes. ADR-0030 and the per-store installer both describe a *pairing QR*; neither the
till nor the edge can draw one, and there is no QR encoder anywhere in the tree.

## Options considered

1. **Keep typing.** The step the setup plan is trying to remove, on every device a store adds.
2. **Print it.** The printers already draw QR natively (`PrintBlock::QrCode`, ESC/POS `GS ( k`), so no
   encoder is needed — but a new store's box has no published `devices` node yet, so on the day the
   first tablets are paired there is no printer to print on.
3. **Write an encoder.** Reed–Solomon, masking and format bits are a few hundred lines that are easy
   to get subtly wrong and hard to test without a decoder, which would be a dependency of its own.
4. **Draw it on the screen with `qrcode-generator`**, loaded only by the screens that show a link.

## Decision

**Option 4.** `qrcode-generator` 2.0.4 in `ui/` — MIT, **no runtime dependencies**, ESM with types,
maintained since 2009 by the author of the encoder most QR libraries descend from.

- **Loaded on demand** with a dynamic `import()`, so it is a separate chunk that only the screens
  showing a pairing link fetch; the till's main bundle does not grow.
- **The link is the QR's whole content.** The same URL the screen already shows as text, so a camera
  that opens it lands on `/pair?code=…` exactly as a typed one does, and the text stays beside it for
  a device with no camera.
- **Drawn as SVG** from the module matrix, in the page's own colours, so it scales to the screen and
  prints from the browser if a technician wants paper.

## Consequences accepted

- **One more package in `ui/pnpm-lock.yaml`**, reviewed like any other; the lockfile gate
  (`pnpm install --frozen-lockfile`) keeps it pinned.
- **A QR code is only as reachable as the link in it.** A box whose `advertised_ip` is unset mints a
  path-only link, which a phone cannot open from a QR; the screen shows the QR only for an absolute
  link and says why otherwise.
