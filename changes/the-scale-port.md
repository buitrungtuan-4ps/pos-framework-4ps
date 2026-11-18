---
kind: added
---
- **A scale is a port** (ADR-0223). `Scale`, the twenty-third, reads the scale beside a till: one
  stable net weight in whole grams, or unstable, overloaded or under zero, and an absent scale is
  unavailable rather than a zero. It ships with its shared contract suite and an in-memory fake;
  a real scale's driver comes with the devices in phase 8.
