# Add a business area

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-20

A business area is one thing the business does: reason codes, fees, cash drawers. It is not a
crate. It is a set of additions, each in the home its tier already has, joined by its wire types and
its events. This guide says where each part goes, which parts need an ADR before any code, and then
follows one real area, **reason codes**
([ADR-0115](../adr/0115-reason-codes-are-a-managed-list.md)), through every tier, quoting the code as
it is.

## 1. The ADRs come first

Write and merge an ADR before any code that does one of these (AGENTS.md §7): changes `pos-proto`
or the event schema, adds or changes a port in `pos-ports`, adds an infrastructure dependency,
changes how money, tax or receipt numbering works, or touches data retention or personal data.
A new area almost always adds wire types, so it almost always starts with an ADR. Reason codes
started with ADR-0115.

**A new area does not get a new crate by default.** It goes into an existing area crate (such as
`pos-cloud-catalog` or `pos-cloud-people`) or into `pos-cloud`, and onto `pos-edge-app` at the edge.
The crate list is fixed by [ADR-0175](../adr/0175-the-crate-list-follows-what-a-move-can-reach.md);
a new crate needs an ADR that amends or supersedes that list.

## 2. Where each part goes

| Part | Its home | Reason codes put it in |
|---|---|---|
| Rules and state machines | a module of `pos-core`: no I/O, tested with fakes | — (a void is the `Bill` machine's, in `pos-core/src/machines.rs`) |
| Wire types and events | `pos-proto`, additive only, held by snapshots | `pos-proto/src/reason_codes.rs` |
| What the till does | `pos-edge-app` ([ADR-0175](../adr/0175-the-crate-list-follows-what-a-move-can-reach.md) decision 1) | `pos-edge-app/src/app/bills/void.rs` |
| Applying the published node, and the edge's routes | `pos-edge` | `pos-edge/src/config_client/apply.rs`, `pos-edge/src/http/reason_codes.rs` |
| Authoring and publishing in the cloud | an existing area crate, or `pos-cloud` | `pos-cloud/src/reason_codes.rs`, `pos-cloud/src/http/reason_codes/` |
| The cloud's storage | a store trait beside the area, its Postgres impl, and a fake | `PostgresReasonCodes` in `pos-cloud/src/persistence.rs` |
| Who may do it | the till's catalogue in `pos-core`, the console's in `pos-cloud-api` | `console.reason_codes.manage` |
| The console API's document | `#[utoipa::path]` on each `/admin` handler | `pos-cloud/src/http/reason_codes/authoring.rs` |
| Screens | `cargo xtask new screen` | the console's `reasonCodes` screen |

## 3. Follow reason codes through the tiers

**The wire type carries the rule every tier asks.** A reason code may be cited for an action only
if the list holds it, it declares that action, and it is not retired:

<!-- guide:excerpt crates/pos-proto/src/reason_codes.rs -->
```rust
pub fn accepts(&self, id: ReasonCodeId, action: ReasonAction) -> bool {
    self.find(id).is_some_and(|code| code.is_valid_for(action))
}
```

**A state machine is `pos-core`'s.** Whether a bill may be voided at all is the `Bill` machine's
answer, from another area, and the till does not re-decide it:

<!-- guide:excerpt crates/pos-core/src/machines.rs -->
```rust
fn next(from: BillState, trigger: BillTrigger) -> Option<BillState> {
    Some(match (from, trigger) {
        (BillState::Open, BillTrigger::Settle) => BillState::Settled,
        (BillState::Open, BillTrigger::Void) => BillState::Voided,
        (BillState::Open, BillTrigger::Reduce) => BillState::Open,
        (BillState::Open, BillTrigger::Split) => BillState::Split,
        (BillState::Open, BillTrigger::Merge) => BillState::Merged,
        _ => return None,
    })
}
```

**The till composes them.** The void checks the reason against the published list, then the person's
permission, then asks the machine:

<!-- guide:excerpt crates/pos-edge-app/src/app/bills/void.rs -->
```rust
if !session
    .reason_codes
    .accepts(reason_code_id, ReasonAction::VoidBill)
{
    return Err(AppError::VoidReasonNotValid);
}
let approver = self.resolve_step_up(&mut ctx, Permission::VoidBill, approval)?;
ctx.require(Permission::VoidBill)?;
// The machine owns whether this bill may be voided at all: `Settled` and `Voided` are both
// terminal, so a second void and a void-after-payment are refused here rather than by a
// hand-rolled state check that could disagree with the machine.
let next_state = Bill::step(record.state, BillTrigger::Void)
    .map_err(DomainError::from)
    .map_err(AppError::Domain)?;
```

**The edge applies what the cloud publishes.** A present node replaces the framework's built-in list,
and an absent or unreadable one leaves it in place, so a store can still trade before anyone has
opened the console:

<!-- guide:excerpt crates/pos-edge/src/config_client/apply.rs -->
```rust
if let Some(published) = document
    .get("reason_codes")
    .and_then(|value| serde_json::to_string(value).ok())
    .and_then(|text| serde_json::from_str::<PublishedReasonCodes>(&text).ok())
    .filter(|published| !published.is_empty())
{
    session.reason_codes = published;
}
```

**The cloud authors the list and compiles it into that node.** The authoring routes sit in one
router, each handler annotated for the console's OpenAPI document, and the publish compiles the
authored entries into the node:

<!-- guide:excerpt crates/pos-cloud/src/http/reason_codes.rs -->
```rust
.route(
    "/admin/reason-codes",
    get(admin_list_reason_codes::<R, A, C>).post(admin_create_reason_code::<R, A, C>),
)
.route(
    "/admin/reason-codes/{reason_code_id}",
    get(admin_get_reason_code::<R, A, C>)
        .put(admin_update_reason_code::<R, A, C>)
        .delete(admin_delete_reason_code::<R, A, C>),
)
```

<!-- guide:excerpt crates/pos-cloud/src/reason_codes.rs -->
```rust
pub fn to_node(codes: Vec<PublishedReasonCode>) -> PublishedReasonCodes {
    PublishedReasonCodes::from_parts(codes)
}
```

**Who may author it is a console permission,** in `pos-cloud-api`'s catalogue. What a person at the
till may do, such as `Permission::VoidBill` above, is `pos-core`'s catalogue, which
`docs/permissions.md` and its snapshot are generated from:

<!-- guide:excerpt crates/pos-cloud-api/src/auth/console_rbac.rs -->
```rust
ManageReasonCodes {
    id: "console.reason_codes.manage",
    roles: [Owner, Admin],
    description: "Author the managed reason codes a void, discount or refund must cite",
},
```

**The console's screen is one registry entry,** which `cargo xtask new screen` writes for a new one:

<!-- guide:excerpt dashboard/src/state/screens.ts -->
```ts
reasonCodes: {
  path: "/reason-codes",
  key: "nav.reasonCodes",
  scope: "tenant",
  roles: ADMIN_MANAGERS,
  tenantScoped: true,
  icon: "clipboard-list",
},
```

## 4. Scaffold your own

The generators write a piece in each home that `cargo xtask` accepts as written. Try them on a name
that is not a real area. The crate and the screen below are the generators' own test inputs, and
are **illustrative**: they are not an area, and they are removed again afterwards.

<!-- guide:command -->
```bash
cargo xtask new crate pos-example --layer tool \
  --purpose "An example crate the generator's golden test renders." --adr 170 --adr 176
cargo xtask new screen --app dashboard stockCount --path /stock --title "Stock count" \
  --title-vi "Kiểm kho" --scope store --icon package --nav-group overview
```

<!-- guide:verify -->
```bash
cargo test -p pos-example
cargo xtask crates --check
```

`pnpm test` in `dashboard/` runs the screen's generated test. Remove the example with
`rm -r crates/pos-example dashboard/src/screens/StockCount.tsx dashboard/tests/stock-count.test.tsx`
and `git checkout -- Cargo.toml Cargo.lock dashboard`. For a real area, the crate needs its ADR
first (section 1); the screen does not.

`just guides-check` holds every quote here to the code it quotes, and `just guides` runs the steps
above ([How a guide stays true](README.md#how-a-guide-stays-true)).
