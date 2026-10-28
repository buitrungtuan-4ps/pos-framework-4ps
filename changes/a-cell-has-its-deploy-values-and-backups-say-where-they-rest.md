---
kind: added
upgrade_note: Nothing changes for a single deployment. A fork running more than one cell commits deploy/cells/<cell>.env (copy deploy/cells/example.env) and passes its values to bootstrap.sh as CELL_* and BACKUP_COUNTRY; bootstrap.sh then writes the [cell] section of cloud.toml once and records the backup country in .env. An existing [cell] section is never touched.
---
- **A cell has its deploy values, and backups say where they rest.** `deploy/cells/<cell>.env`
  holds one cell's non-secret values: id, country alias, home country, base URL, the country it runs
  in, its canary rank and `BACKUP_COUNTRY` (ADR-0179 C8). Given them, `bootstrap.sh` writes the
  `[cell]` section once, keeps one already there, and only warns about a value the cloud would
  refuse; it never fails a deploy over them. Every off-box backup logs the country it rests in and
  warns when that is not the cell's home country or is not recorded. The backup country is C7's
  second half, moved here beside the cell's deploy values. The one-GitHub-Environment-per-cell,
  canary-ordered deploy is drafted for the owner and not part of this change.
