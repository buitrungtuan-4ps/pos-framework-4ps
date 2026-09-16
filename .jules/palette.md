## 2026-08-18 - Command Palette Combobox & Modal Overlay Accessibility

**Learning:** Overlay command palettes using list filtering and keyboard navigation require explicit WAI-ARIA combobox/listbox roles (`role="dialog"`, `role="combobox"`, `role="listbox"`, `role="option"`, `aria-selected`) for screen readers. Furthermore, keyboard navigation in scrollable lists needs explicit `scrollIntoView({ block: "nearest" })` on active option updates so items don't move off-screen during arrow key traversal. Safe optional chaining (`scrollIntoView?.()`) is essential for jsdom compatibility in Vitest suite.
**Action:** Always pair `aria-selected` and active option scrolling when building keyboard-navigated search/switcher overlays.

## 2026-08-18 - Media Grid Accessibility & Fallback Role Attributes

**Learning:** Static `<div>` fallback elements with `aria-label` are ignored by assistive technologies unless given an explicit `role="img"`. Additionally, image grids where buttons share identical `aria-label` text prevent screen reader users from identifying choices; appending asset identifiers (like `media_id`) gives each button a unique, distinguishable accessible name.
**Action:** Add `role="img"` to placeholder container divs and append item identifiers to grid selection `aria-label` attributes.
