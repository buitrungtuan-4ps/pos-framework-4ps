# State machines

Generated from `crates/pos-core/src/machines.rs`. Do not edit by hand — run
`POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core` to regenerate. A cell is the resulting
state's wire token, or `·` when the trigger is not valid in that state.

### `table`

| state | seat | request_bill | settle | clean |
|---|---|---|---|---|
| `TABLE_STATE_FREE` | `TABLE_STATE_OCCUPIED` | · | · | · |
| `TABLE_STATE_OCCUPIED` | · | `TABLE_STATE_AWAITING_PAYMENT` | · | · |
| `TABLE_STATE_AWAITING_PAYMENT` | · | · | `TABLE_STATE_NEEDS_CLEANING` | · |
| `TABLE_STATE_NEEDS_CLEANING` | · | · | · | `TABLE_STATE_FREE` |

### `order`

| state | settle | void |
|---|---|---|
| `ORDER_STATE_OPEN` | `ORDER_STATE_SETTLED` | `ORDER_STATE_VOIDED` |
| `ORDER_STATE_SETTLED` *(terminal)* | · | · |
| `ORDER_STATE_VOIDED` *(terminal)* | · | · |

### `order_line`

| state | hold | resume | fire | void |
|---|---|---|---|---|
| `ORDER_LINE_STATE_ADDED` | `ORDER_LINE_STATE_HELD` | · | `ORDER_LINE_STATE_FIRED` | `ORDER_LINE_STATE_VOIDED` |
| `ORDER_LINE_STATE_HELD` | · | `ORDER_LINE_STATE_ADDED` | `ORDER_LINE_STATE_FIRED` | `ORDER_LINE_STATE_VOIDED` |
| `ORDER_LINE_STATE_FIRED` | · | · | · | `ORDER_LINE_STATE_VOIDED` |
| `ORDER_LINE_STATE_VOIDED` *(terminal)* | · | · | · | · |

### `bill`

| state | settle | void |
|---|---|---|
| `BILL_STATE_OPEN` | `BILL_STATE_SETTLED` | `BILL_STATE_VOIDED` |
| `BILL_STATE_SETTLED` *(terminal)* | · | · |
| `BILL_STATE_VOIDED` *(terminal)* | · | · |

### `shift`

| state | count | close |
|---|---|---|
| `SHIFT_STATE_OPEN` | `SHIFT_STATE_COUNTED` | · |
| `SHIFT_STATE_COUNTED` | · | `SHIFT_STATE_CLOSED` |
| `SHIFT_STATE_CLOSED` *(terminal)* | · | · |
