---
kind: changed
---
- **A screen's own read is retried, and says in place when it fails** (ADR-0173). Today's bills
  and takings, the Devices screen, the guests waiting, the counter list, activation, the claim
  page and the boards' queue numbers are each retried with the engine's backoff while the screen
  is open. A failure shows on the screen with a **Retry**, where most used to be silent.
  - **Activation fixed:** a store server that briefly does not answer is no longer told it "runs
    without a cloud". Only a box with no cloud is.
  - **Unchanged:** the status bar's sync reading, whose timer is already its retry.
