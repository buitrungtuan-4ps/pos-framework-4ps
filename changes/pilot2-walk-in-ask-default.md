---
kind: changed
upgrade_note: New-store default only. A store the console creates from now on is given counter.walk_in_channel = WALK_IN_CHANNEL_ASK by the new-store values, so its counter asks each walk-in guest to eat in or take away. Existing stores are not changed and keep WALK_IN_CHANNEL_TAKEAWAY unless someone sets the value in Shared settings.
---
- **A new store's counter asks each walk-in guest whether they eat in or take away.** The owner
  chose this new-store value on 2026-10-29 (pilot run 2, N6): without it a walk-in was always taken
  away, so a guest who ate in was sold from the takeaway book and taxed at its rate unless somebody
  had set `counter.walk_in_channel` by hand. Existing stores keep what they have.
