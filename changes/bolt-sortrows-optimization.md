---
kind: changed
---
- Pre-extract string conversion in DataTable `sortRows` during the $O(N)$ mapping pass to avoid repeated `String()` evaluations during sort comparisons.
