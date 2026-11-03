---
kind: added
---
- **Roadmap v4, and ADR-0189, 0195 and 0197, accepted: the POS becomes a customization platform.**
  The owner's plan for the overhaul replaces roadmap v3's ordering. HQ pins each store to a version
  of a customization package, locked or chosen among allowed versions; a publish wakes the store to
  pull it within about a second, with the 30 s poll kept; every tap on a till paints within 10 ms at
  the 95th percentile, measured nightly under CPU throttling and from real tills without personal
  data. The code follows in its own changes.
