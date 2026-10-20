---
kind: changed
---
- **The adapter guide has an example that passes its contract suite.**
  `docs/guides/write-an-adapter.md` now generates the courier sandbox, which names no vendor,
  quotes the sandbox descriptor from the generator's golden file, and proves the adapter with the
  `ShippingDispatch` contract suite. It notes that `templates/adapter-template` is being retired
  (ADR-0176 decision 6); its steps stay until it is removed. `just guides-check` holds the command
  and quote, and `just guides` runs the steps.
