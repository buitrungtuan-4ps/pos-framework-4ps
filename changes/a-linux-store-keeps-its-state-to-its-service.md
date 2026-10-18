---
kind: changed
---
- **A Linux store's state directory is readable by its service alone** (ADR-0174). The unit sets
  `StateDirectoryMode=0700` and `UMask=0077`, and the console's Linux installer and the appliance's
  `provision.sh` create `/var/lib/pos-edge` with mode 0700. A box set up before this is closed the
  next time its installer runs; until then its setup screen says its data is open.
