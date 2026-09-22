## 2026-08-18 - Command Palette Combobox & Modal Overlay Accessibility

**Learning:** Overlay command palettes using list filtering and keyboard navigation require explicit WAI-ARIA combobox/listbox roles (`role="dialog"`, `role="combobox"`, `role="listbox"`, `role="option"`, `aria-selected`) for screen readers. Furthermore, keyboard navigation in scrollable lists needs explicit `scrollIntoView({ block: "nearest" })` on active option updates so items don't move off-screen during arrow key traversal. Safe optional chaining (`scrollIntoView?.()`) is essential for jsdom compatibility in Vitest suite.
**Action:** Always pair `aria-selected` and active option scrolling when building keyboard-navigated search/switcher overlays.

## 2026-08-18 - WAI-ARIA Breadcrumb Navigation & Current Page Indication

**Learning:** Breadcrumb trails implemented as un-ordered elements without structured list items (`<ol>` / `<li>`) or `aria-current="page"` fail to communicate structural parent-child navigation hierarchy and current page location to screen reader users.
**Action:** Wrap breadcrumb items in semantic `<ol>` / `<li>` lists inside `<nav aria-label="...">` and mark the final item representing the active screen with `aria-current="page"`.

## 2026-08-18 - Click-Outside Dismissal for Topbar Dropdown Overlays

**Learning:** Topbar dropdown popovers and overlays (like `AccountMenu`, `NotificationBell`, and `ContextPicker`) that rely solely on button toggles and Escape keys create friction when users attempt to dismiss them by clicking elsewhere on the page. Binding a document `pointerdown` listener scoped to the open state with a root `container` ref enables seamless click-outside dismissal across pointer and touch interactions.
**Action:** Always wrap topbar popovers in a root element `ref` and attach a document `pointerdown` click-outside dismiss effect while `open()` is true.
