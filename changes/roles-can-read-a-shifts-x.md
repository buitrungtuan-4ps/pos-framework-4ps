---
kind: changed
upgrade_note: Migration `0088_roles_can_read_a_shifts_x` runs once at the cloud's boot, behind a `data_migrations` marker. It grants `reports.shift.read_x` directly to every role that grants `admin.config.edit` directly (archived roles included), changes no with-approval list, and queues every store whose people have been published to be published again. A later boot does not grant it back where an owner removed it.
---
- **Roles that run the store read a shift's X with its expected cash**
  ([ADR-0187](../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)). Roles that
  existed before `reports.shift.read_x` and grant `admin.config.edit` directly, the manager and the
  owner out of the box, now hold it, as a new tenant's starting roles do.
