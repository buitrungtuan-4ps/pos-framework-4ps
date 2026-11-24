---
kind: added
---
- **HQ gives a store its e-invoice symbol** (ADR-0225 decision 2). `PUT /admin/config/einvoice-symbol`
  writes the symbol on the store's own `einvoice` node and keeps the node's settings. It needs
  `console.config.publish` and is audited. It is checked against the store's country, for Vietnam
  a form digit and six characters such as `1C26MAA`; a symbol that is not one answers `400
  INVALID_FORMAT` on `symbol`. It is how a pilot store that issues an e-invoice with every settle
  gets the symbol it needs to settle. Allocating a symbol automatically per lease generation is
  still to come.
