# Custom data at the end of phase 3, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Recorded** 2026-11-07

The exit proof of roadmap-v4 phase 3 (custom data). The criterion reads: *a custom object type is
designed, synced, entered on a till, reported and printed, and a field marked personal never reaches
an event or a log.* This record walks each part to the code and the test that shows it. Like
[till-latency-2026-11-phase-2-exit.md](till-latency-2026-11-phase-2-exit.md), it records one moment
and is not regenerated.

**Recorded at** `MAIN_SHA` on `main` (#1216, the last pull request before this record).

## The criterion, part by part

| Part | Where it is built | What shows it |
|---|---|---|
| **Designed** | A package version declares custom fields, lists and object types (`pos_proto::package::PackageData`, #1167). `pos_core::package::check_data` checks it on publish and on apply (#1168), and `check_evolution` keeps every later version additive (#1169). `POST /admin/packages/{package_id}/versions` publishes it with its data part (#1171). | `crates/pos-core/src/package/data/tests.rs`; `crates/pos-cloud/tests/cloud/packages.rs` (`a_data_part_is_published_checked_and_only_ever_added_to`) |
| **Synced** | The version rides the store's `package` node to the edge, which checks it again before it runs it (ADR-0189). Values travel back in `data.custom_field.set` and `data.custom_record.changed` (#1172), which carry no personal value. | `crates/pos-edge/src/demo/package.rs` (`the_custom_data_profile_runs_here`); `crates/pos-proto/src/events/custom_data/tests.rs` |
| **Entered on a till** | `Edge::save_custom_record` and `Edge::set_custom_fields` (#1208) behind `/api/custom-records` and `/api/custom-fields` (#1209); the till's Records screen, labelled from the tenant's grid (#1210, #1211). | The replays *a reservation is entered and changed on the till, labelled from the tenant's grid* and *a personal value is saved behind its consent, never echoed, and read back only by a reader* (`ui/tests/replay.spec.mjs`) |
| **Reported** | The projector folds the events into `custom_values` (#1173, #1174); `GET /admin/stores/{store_id}/custom-values/summary` counts a store's objects by one non-personal field, and refuses a personal one (#1176, #1177); the console's Custom data screen (#1178). | `crates/pos-cloud-reports/src/custom_data/tests.rs`; `crates/pos-cloud/tests/cloud/custom_values.rs`; the console replays |
| **Printed** | The `receipt` node's template per store and channel (ADR-0207, #1212) prints a receipt's custom fields (#1213) and a record's slip (#1214), a personal value only on the person's own copy. | `crates/pos-edge/tests/record_slip.rs`; `crates/pos-edge-printing` tests |
| **A field marked personal never reaches an event or a log** | A personal value is written only to the store's subject store, in the event's transaction, under a `subject_id` the event carries (#1208). It expires on its own period (#1206), an erasure reaches it by id (#1215), and the cloud accepts a personal field now that both have landed (#1216). | `crates/pos-edge/tests/personal_values.rs` (below) |

## The personal-value test

`crates/pos-edge/tests/personal_values.rs` runs a made-up value through every path a personal
value takes on a store, through the till's own routes:

1. refused without its consent, and nothing written;
2. entered behind its consent, then changed;
3. listed and read as a record;
4. its slip asked for;
5. read back: refused to a person without `data.personal.read`, answered to one who holds it;
6. erased by the cloud's `erasures` node.

It then searches, for both the first and the changed value:

- every event the store logged, as stored;
- every answer of every route but the subject read;
- every line the edge wrote to its log, at every level and from every target, captured by a
  `tracing` subscriber set to `TRACE`.

None holds either value. The subject read does hold the changed value, and the capture holds the
read's audit line, so each search could have found one. After the erasure the subject record is
masked.

## Replays

The full replay suite (`ui/tests/replay.spec.mjs`) against a `minimal-edge` stamped
`POS_EDGE_RELEASE_VERSION=0.14.1`: REPLAY_FULL normally and REPLAY_PIN pinned to two CPUs
(`taskset -c 0,1`). The console's replays (`dashboard/tests/replay.spec.mjs`): CONSOLE_REPLAY.

## Left to the owner

ADR-0206 lists what the software does not decide, and they stay open past this phase:

- the lawful basis of each personal field under Vietnam's PDPD (Decree 13/2023/NĐ-CP), and the GDPR
  where it applies;
- the consent wording behind each purpose's label key;
- each field's retention period;
- a data protection impact assessment before personal custom data is used in a report;
- escalating an EU resident's rights request to the Data Protection contact.

Go-live gains gate L8: a box that holds personal custom values runs with full-disk encryption
([`go-live.md`](../go-live.md) step 21).

## What phase 3 shipped

Pull requests #1165 to #1178 and #1205 to #1216:

- the two ADRs (ADR-0206, ADR-0207) and their acceptance;
- the data part of a package, checked and additive;
- the two custom-data events and the cloud's projection and report;
- a subject's own expiry;
- the edge's entry and routes;
- the tenant's translation grid on the store;
- the Records screen;
- the receipt template, receipt printing and the record slip;
- erasure by subject id;
- lifting the personal-field refusal.
