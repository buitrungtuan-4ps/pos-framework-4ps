---
kind: changed
---
- **The Floor screen and the item pickers do less work per row.** The Floor screen indexes area
  names once per change instead of searching the area list for every table row, and
  `ComboboxField` and `MultiComboboxField` fold the search query to lower case once per keystroke
  instead of once per option. Nothing an operator sees changes. (#694, #684)
