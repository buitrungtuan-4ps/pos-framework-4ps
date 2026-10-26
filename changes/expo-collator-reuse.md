---
kind: changed
---
- **The Expo screen reuses a single `Intl.Collator` instance when sorting table groups.** Reusing a single collator avoids repeated allocations during client-side sorting.
