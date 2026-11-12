---
kind: added
---
- **The screen designer previews a screen with the till's own renderer** (ADR-0209). Under the
  chosen layout's tree, *Preview on the till* draws it as the till does, after every edit, at a
  phone's or tablet's width for those layouts.
  - Headings, text, stacks, frames and buttons draw with the till's own code; a part the till fills
    in on the store (the order's lines, the tickets) is a dashed outline that names it.
  - A test holds the preview to the till: both draw one committed fixture, the default package and a
    designed screen, and must give the same shape (`packages/package-renderer/fixtures/`).
