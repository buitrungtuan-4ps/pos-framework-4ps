---
kind: added
---
- **A cloud can say which cell it is.** An optional `[cell]` section in `cloud.toml` names this
  deployment's cell id, country alias, home country, base URL and the country its servers run in
  (ADR-0179 C1). It is checked at boot, and a malformed key stops the process naming the key. Without
  the section the deployment is the one cell `default`, so nothing changes for a single deployment.
  The boot logs the cell, and warns, never refuses, when the servers run outside the home country.
