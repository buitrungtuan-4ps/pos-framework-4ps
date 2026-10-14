---
kind: added
---
- **The setup screen warns when other accounts on the store server's computer can read the store's
  data** (ADR-0174). The store server checks the directory that holds its database at every start
  and on every `GET /api/activation`, which gains an optional `state_access`: `STATE_ACCESS_OK`,
  `STATE_ACCESS_TOO_OPEN` or `STATE_ACCESS_UNCHECKED`.
  - **What counts as closed:** on Windows, an access-control list that lets only SYSTEM,
    Administrators and the store server's own account read it; on Linux and macOS, a directory
    that grants nothing to group or others.
  - **What is said:** the warning and a log line at start say to run the installer again. They
    never name the file or the account.
