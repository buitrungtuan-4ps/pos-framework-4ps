---
kind: added
---
- **ADR-0174: a Windows store keeps its state to its own service.** The store server runs as
  `NT SERVICE\pos-edge`, not LocalSystem. Its database and sealed secrets sit in a `data\`
  directory only SYSTEM, Administrators and the service can read, while its log stays readable by
  Users. The secrets are sealed with DPAPI, and the installer applies and verifies the ACLs on its
  one fixed path. The setup screen warns when the state is too open, Linux gets the same rule, and
  an existing box migrates when the installer is run again. The implementation follows in its own
  pull request.
