# Add a setting

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-20

A setting is a value one store may run differently from another: how long a till waits before it
locks, which languages a receipt prints in. It is a typed field with a default on a node the cloud
publishes, and **its default is what the edge did before the field existed**, so an upgrade changes
nothing until someone sets a value
([ADR-0160](../adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
decision 1). The console writes it at the tenant, a brand, a store group or one store, and the cloud
resolves each store's value (decision 3).

Every setting is listed once, in the register, `pos_proto::settings::register()`. Two committed
files are generated from it and diffed in CI, so a setting cannot disappear unseen (decision 9):
[`docs/configuration.md`](../configuration.md), which an operator reads, and
`docs/snapshots/settings.txt`. The console's **Settings** screen draws its forms from the same list.

There is no generator for a setting. It is a field on a `pos-proto` type, and `cargo xtask new`
never edits `pos-proto` ([ADR-0176](../adr/0176-the-adapter-and-country-standard.md) decision 5).
This guide walks through a real one instead, `session.idle_lock_seconds`, quoting the code as it is.

## 1. Write a short ADR first

A new setting changes `pos-proto`, so it needs its own ADR, merged before any code (AGENTS.md §7).
Keep it under a page. This skeleton is a starting point to copy into `docs/adr/`, not a record that
exists:

```markdown
# NNNN. <What a store may now run differently>

**Status** Proposed · **Owner** <team> · **Relates to** ADR-0160

## Context
What the edge does today, which store needs it to differ, and why a store and not the framework
should decide it (ADR-0160: a setting exists because a store runs the value differently).

## Options considered
| | Option | Against it |
|---|---|---|
| A | Leave it fixed | … |
| B | A setting on `<node>` | … |

## Decision
- **The field:** `<node>.<field>`, its shape (a choice, a whole number between two bounds, or on
  and off), and its unit.
- **The default:** today's behaviour, so no store changes on upgrade.
- **The preset,** if new stores should start elsewhere: the owner's to confirm.
- **Where it may be written:** tenant, brand, store group, store.
- **Whether any value needs a recorded confirmation** (ADR-0169).

## Consequences accepted
What the register, the console and the edge gain, and what a store on an older release does.
```

## 2. Put the field on its node

A field on the published node, `None` when the node does not carry it. Its accessor applies the
bounds and the default, so nothing reads the raw number:

<!-- guide:excerpt crates/pos-proto/src/session.rs -->
```rust
/// How many seconds an attended till may sit with no touch before it locks. Read it through
/// [`PublishedSession::idle_lock_seconds`].
#[serde(default, skip_serializing_if = "Option::is_none")]
pub idle_lock_seconds: Option<i64>,
```

<!-- guide:excerpt crates/pos-proto/src/session.rs -->
```rust
pub fn idle_lock_seconds(&self) -> u32 {
    within(self.idle_lock_seconds, &IDLE_LOCK_SECONDS).unwrap_or(DEFAULT_IDLE_LOCK_SECONDS)
}
```

## 3. List it in the register

One entry in `crates/pos-proto/src/settings/nodes.rs`, in its node's function: the shape and bounds,
the default read back from the node itself so the two cannot disagree, the preset the owner chose
for new stores, the scopes, the first release that honours it, and one sentence on what it decides:

<!-- guide:excerpt crates/pos-proto/src/settings/nodes.rs -->
```rust
Setting {
    node: PublishedSession::NODE,
    field: "idle_lock_seconds",
    shape: SettingShape::Int {
        min: *session::IDLE_LOCK_SECONDS.start(),
        max: *session::IDLE_LOCK_SECONDS.end(),
        unit: SettingUnit::Seconds,
        default: i64::from(unset.idle_lock_seconds()),
        // Confirmed by the owner on 2026-10-01: a new store's till locks after two minutes
        // without a touch. An existing store keeps a till that never locks.
        preset: Some(120),
    },
    scopes: STORE_WIDE,
    since: NEXT_RELEASE,
    confirm: None,
    summary: "A security setting: how long a till may sit with no touch, key, click or \
              scroll before it signs its person out and locks, until their PIN opens it \
              again on the screen they left. `0` never locks. The kitchen board, the pass \
              and the screens before sign-in never lock.",
},
```

`since` is what lets the console hide a setting from a store whose release does not honour it yet
(ADR-0160 decision 5). Set `confirm` when a value records something about a person (ADR-0169).

## 4. Make the edge read it

The edge reads the value through the accessor, never the raw field. Here, the session read hands it
to the till, which locks after that many idle seconds:

<!-- guide:excerpt crates/pos-edge/src/http/auth.rs -->
```rust
let idle_lock_seconds = session.session_settings.idle_lock_seconds();
```

## 5. Give the console its words

The Settings screen names a setting by the key `settings.item.<node>.<field>`, with `.title` and
`.help`, in every catalogue in `dashboard/src/i18n/`. The words are written by whoever owns them, in
`en` and `vi` alike; a setting with none is still drawn, under its key. These are the setting's
English words:

<!-- guide:excerpt dashboard/src/i18n/en.json -->
```json
"settings.item.session.idle_lock_seconds.help": "A security setting: how long a till may sit with no touch before it signs its person out and locks, until their PIN opens it again on the screen they left. 0 never locks. The kitchen board and the pass never lock.",
"settings.item.session.idle_lock_seconds.title": "Lock a till left untouched",
```

## 6. Regenerate the register, and prove it

`just snapshot` rewrites `docs/configuration.md` and `docs/snapshots/settings.txt` from the register.
Commit both with the change. Then check that the register takes the setting as the guide says, that
the edge reads it, and that nothing was removed:

<!-- guide:excerpt crates/pos-edge/tests/guide_add_a_setting.rs -->
```rust
// Its default is what the edge did before the field existed, so it is a value it takes.
assert_eq!(setting.check(&setting.default_value()), Ok(()));
// A value past its bound is refused when it is written and again when it is resolved.
let SettingShape::Int { max, .. } = setting.shape else {
    panic!("{KEY} is a whole number");
};
assert_eq!(
    setting.check(&serde_json::json!(max + 1)),
    Err(ValueRefusal::OutOfRange)
);
// It may be written at every scope, from the tenant down to one store.
assert!(setting.scopes.contains(&SettingScope::Tenant));
assert!(setting.scopes.contains(&SettingScope::Store));
// And the committed snapshot names it, one fact per line.
assert!(render_snapshot().contains(&format!("{KEY}\tdefault=0\n")));
```

<!-- guide:verify -->
```bash
cargo test -p pos-edge --test guide_add_a_setting
cargo test -p pos-edge --test acceptance the_session_read_says_when_the_till_locks
cargo test -p pos-proto settings
cargo xtask snapshot
```

## 7. Record it

A change file under `changes/` with an upgrade note: a setting adds a default value, and AGENTS.md §2
asks for one whenever a default changes. Say that the default is today's behaviour, and name the
preset a new store is given.

`just guides-check` holds every quote in this guide to the code it quotes, and `just guides` runs
the checks above ([How a guide stays true](README.md#how-a-guide-stays-true)).
