## 2026-08-18 - Command Palette Combobox & Modal Overlay Accessibility

**Learning:** Overlay command palettes using list filtering and keyboard navigation require explicit WAI-ARIA combobox/listbox roles (`role="dialog"`, `role="combobox"`, `role="listbox"`, `role="option"`, `aria-selected`) for screen readers. Furthermore, keyboard navigation in scrollable lists needs explicit `scrollIntoView({ block: "nearest" })` on active option updates so items don't move off-screen during arrow key traversal. Safe optional chaining (`scrollIntoView?.()`) is essential for jsdom compatibility in Vitest suite.
**Action:** Always pair `aria-selected` and active option scrolling when building keyboard-navigated search/switcher overlays.
