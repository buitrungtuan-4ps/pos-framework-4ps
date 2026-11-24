---
kind: security
---
- **A security review of roadmap v4's new surfaces.** `docs/security-review-overhaul.md` reviews
  the guest page, the kiosk, the gateway, webhooks, delivery intake, the NATS wake, CEL limits,
  personal custom data and the tab label: no Critical or High finding, two Medium (the guest page
  shares the console's origin; a kiosk's token lives in a public browser) and twelve Low, each
  fixed or tracked. The pilot's real-hardware gates P13 to P17 are defined in
  `docs/real-hardware-gates.md`, and `docs/data-residency.md` states the B10.4 decisions the owner
  still owes. CEL's parser is now tested against nesting that uses no bracket.
