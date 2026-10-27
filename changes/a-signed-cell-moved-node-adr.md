---
kind: added
---
- **ADR-0182, proposed: a signed "cell moved" config node.** When a tenant moves cell, its stores
  learn it through the configuration they already pull from the old cell, as a node signed with the
  directory key, which no cell holds. A store checks the signature before it changes where it sends
  its events. This is ADR-0179's item C9. It waits on the owner, since it adds to `pos-proto`, and no
  code changes until it is accepted.
