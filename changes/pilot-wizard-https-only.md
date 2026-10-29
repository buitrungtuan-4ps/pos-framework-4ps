---
kind: fixed
---
- **The console hands a store no file from a plain-http address.** Over http, the new-store
  wizard's last step and the Stores screen's **Move to a new box** drawer wrote
  `cloud_url = "http://…"` into `config.toml` and both installers, and the store refused that
  cloud and ran LAN-only with no staff or menu. Both now offer no file over http and say to open
  the console at its https address, as the Windows setup file already did. The console's replay
  harness opens the console through an https front for the same reason.
