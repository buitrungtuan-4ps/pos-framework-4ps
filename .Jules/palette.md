## 2026-08-18 - Consistent focus-visible indicators across touch pads
**Learning:** Shared on-screen keyboard/pad components (like CodePad) must include explicit `focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent` styles to ensure keyboard navigation visibility parity with cash Keypad.
**Action:** When creating or updating interactive pad or button group components, ensure focus-visible utility classes are consistently present alongside touch and active states.
