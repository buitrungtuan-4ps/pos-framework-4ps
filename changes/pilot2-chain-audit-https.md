---
kind: fixed
---
- **The cloud no longer reports an honest store's chain as broken.** A store hashes each event
  before the cloud files it under the store's tenant and brand, and the cloud recomputed the chain
  over its stamped copy, so every link from the second record of every registered store read as
  broken: the Chain integrity screen filled with findings for a store that had done nothing. The
  cloud now also hashes each record with the owner fields a store writes, and either matching is
  the store's own hash; an edit to anything else is still caught. Events published over HTTPS and
  over the broker were both affected. No event, route or setting changes. (pilot run 2, N4)
