---
kind: added
---
- **A broken chain raises an alert that stays until somebody acknowledges it.** When recomputing a
  store's chained log finds a fork or a broken link (ADR-0132), the Alerts screen now shows a
  Critical **Chain broken** alert for that store, with the chain positions involved and a link to
  Chain integrity. Unlike every other alert it never clears by itself, because a broken chain does
  not heal: acknowledging it closes it, and the audit trail records who did and when. A later
  finding on the same store opens a new alert. A gap in what the cloud has received is still only a
  prompt to reconcile and raises nothing.
