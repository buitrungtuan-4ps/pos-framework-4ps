---
kind: fixed
---
- **The console shows and keeps a sales channel it does not know.** A channel token a newer cloud
  sends is shown as it arrived instead of as a blank label in a placement's price summary, and a
  placement's price on such a channel is kept when the placement is saved instead of being dropped.
  The five copies of the channel labels are one, in `dashboard/src/lib/channels.ts`, with the
  open-token helpers `isKnown` and `tokenLabel` in `dashboard/src/lib/tokens.ts`.
