---
kind: added
upgrade_note: A package version may now carry `home_screens` (ADR-0210), an additive field of the package content, the publish and dry-run request and `GET /api/package`; a version that states none reads and runs as before.
---
- **A package version names the screen a till opens after sign-in** (ADR-0210). The screen
  designer's *Home screen after sign-in* picks one of the version's screens, or the release's own.
  - The cloud and the edge check the homes: at most a role and a device class per home, one base,
    and only screens the version declares. The dry run says which home is at fault.
  - After a sign-in the till opens the home for its device class, at the screen's own route or at
    `/screen/:id`. A version without homes starts where the till always has.
