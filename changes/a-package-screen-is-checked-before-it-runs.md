---
kind: added
---
- **A package screen is checked before it is published or run.** The cloud, when a version is
  published, and the edge, before it applies one, refuse a screen that the release could not draw:
  an unknown component, one not drawn on the device class the variant runs on, a tree over 400 nodes
  or 12 levels, a property the component does not take or of the wrong kind, an unknown act, an
  amount or entry fixed by the package, or a required parameter of an act that nothing supplies
  (ADR-0190, ADR-0203 decision 4). The console's field error names the screen and node by position.
