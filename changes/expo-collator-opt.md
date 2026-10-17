---
kind: changed
---
- **The Expo screen reuses a collator instance for table sorting.** `Expo.tsx` uses a module-level `Intl.Collator` instance and direct string comparisons for table order sorting, avoiding repeated collator allocation and options parsing.
