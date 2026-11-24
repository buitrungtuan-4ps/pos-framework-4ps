---
kind: security
---
- **Leaving a kiosk waits for its binding.** On a store with a kiosk terminal, a person's PIN
  sign-in on a device whose terminal binding cannot be read is refused `503`, rather than let a
  person who may not leave the kiosk through as if it were an ordinary till (phase 9 security
  review, finding K4).
