# Permission matrix

Generated from `crates/pos-core/src/permission.rs`. Do not edit by hand — run `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core`. A `✓` is a default grant; roles are data, so a deployment overrides these in the cloud.

| id | group | risk | PIN | OWNER | MANAGER | SUPERVISOR | CASHIER | SERVER | COOK |
|---|---|---|---|---|---|---|---|---|---|
| `sales.line.void_fired` | SALES | HIGH | yes | ✓ | ✓ | ✓ | · | · | · |
| `sales.item.open` | SALES | MEDIUM | · | ✓ | ✓ | ✓ | ✓ | ✓ | · |
| `sales.item.mark_unavailable` | SALES | LOW | · | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `sales.order.transfer` | SALES | LOW | · | ✓ | ✓ | ✓ | · | ✓ | · |
| `billing.discount.apply` | BILLING | MEDIUM | · | ✓ | ✓ | ✓ | ✓ | ✓ | · |
| `billing.discount.override_ceiling` | BILLING | HIGH | yes | ✓ | ✓ | · | · | · | · |
| `billing.comp.apply` | BILLING | HIGH | yes | ✓ | ✓ | ✓ | · | · | · |
| `billing.price.override` | BILLING | HIGH | yes | ✓ | ✓ | · | · | · | · |
| `billing.bill.void` | BILLING | HIGH | yes | ✓ | ✓ | · | · | · | · |
| `billing.refund.issue` | BILLING | HIGH | yes | ✓ | ✓ | · | · | · | · |
| `billing.receipt.reprint` | BILLING | MEDIUM | · | ✓ | ✓ | ✓ | ✓ | · | · |
| `cash.drawer.open_no_sale` | CASH_AND_SHIFTS | HIGH | yes | ✓ | ✓ | ✓ | · | · | · |
| `cash.shift.open` | CASH_AND_SHIFTS | LOW | · | ✓ | ✓ | ✓ | ✓ | · | · |
| `cash.shift.close` | CASH_AND_SHIFTS | MEDIUM | · | ✓ | ✓ | ✓ | ✓ | · | · |
| `cash.movement.record` | CASH_AND_SHIFTS | MEDIUM | · | ✓ | ✓ | ✓ | ✓ | · | · |
| `inventory.stocktake.perform` | MENU_AND_INVENTORY | MEDIUM | · | ✓ | ✓ | ✓ | · | · | · |
| `inventory.receipt.record` | MENU_AND_INVENTORY | LOW | · | ✓ | ✓ | ✓ | · | · | · |
| `inventory.waste.record` | MENU_AND_INVENTORY | LOW | · | ✓ | ✓ | ✓ | · | · | ✓ |
| `menu.item.edit` | MENU_AND_INVENTORY | MEDIUM | · | ✓ | ✓ | · | · | · | · |
| `admin.device.manage` | STORE_ADMINISTRATION | HIGH | · | ✓ | ✓ | · | · | · | · |
| `admin.config.edit` | STORE_ADMINISTRATION | MEDIUM | · | ✓ | ✓ | · | · | · | · |
| `cloud.tenant.manage` | CLOUD_ADMINISTRATION | HIGH | · | ✓ | · | · | · | · | · |
| `cloud.staff.manage` | CLOUD_ADMINISTRATION | HIGH | · | ✓ | · | · | · | · | · |
