---
kind: fixed
---
- **Closing a row's action menu with Escape returns focus to its button.** In the dashboard, Escape
  closed `RowActions` but dropped keyboard focus to the page, so a keyboard or screen-reader user
  lost their place in the table. Focus now goes back to the `⋯` button when it was inside the menu,
  and a press outside the menu closes it through the shared `useClickOutside`, as the header's
  dropdowns do. (#659)
