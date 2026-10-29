---
kind: changed
---
- **The country guide has an example that builds.** `docs/guides/add-a-country-module.md` now walks
  through generating an illustrative country, `XA` with the testing currency `XTS`, whose values are
  marked as invented and not any country's law, and proving it: the module's tests and contract
  suite, `cargo xtask countries`, and both binaries checked with it selected. `just guides-check`
  holds the command to what the generator accepts, and `just guides` runs the steps.
