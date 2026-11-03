---
kind: fixed
---
- **The till's menu and the counter's order list name items in the store's language.** `GET
  /api/menu` resolved courses and modifier groups to the store's display language but sent each
  item's name as the menu carried it, and the counter list did the same, so a store whose menu was
  not resolved at install (a session the language reached after the menu) showed English item names
  beside Vietnamese courses. Both now resolve each item's name the same way, falling back to the
  default name where there is no translation.
