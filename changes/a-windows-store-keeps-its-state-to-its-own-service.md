---
kind: changed
---
- **A Windows store runs as its own service account, and only that account and administrators can
  read its data** (ADR-0174).
  - **The account:** the installer runs `pos-edge` as `NT SERVICE\pos-edge`, restricted to
    `SeChangeNotifyPrivilege` and `SeCreateSymbolicLinkPrivilege`, and grants the second in local
    security policy so over-the-air updates keep working.
  - **The layout:** the database, its WAL files, the sealed vault and the pairing file move to
    `C:\ProgramData\pos-edge\data`, which only the service and administrators can read. The log moves
    to `logs\`, which a standard-user till account can still read. The installer reads every ACL back
    and says `FAIL` if one is wrong.
  - **One path:** the installer accepts only `%ProgramData%\pos-edge`, and refuses a link, a
    junction or a directory another account owns, before it changes anything.
  - **Upgrade note:** run the installer again on each Windows store. It moves the files, starts the
    service once as LocalSystem so it seals its Credential Manager secrets into `data\vault`, then
    moves it to its own account. If any step fails the service stays on LocalSystem and keeps
    trading. The pairing file is now readable from an elevated window only. A release from before
    this one, put back by an update rollback after the move, does not update over the air until the
    installer is run again.
