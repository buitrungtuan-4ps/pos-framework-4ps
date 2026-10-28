---
kind: changed
---
- **The Integrations screen memoizes provider and connection lookups by family.** Pre-grouping providers and connections into Map accessors and memoizing active families and table columns reduces $O(F \cdot (P + C))$ filter passes and allocations during SolidJS renders down to $O(1)$ lookups.
