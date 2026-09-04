# ADR-0099 — The console's landing page answers "is this shop all right", not "how much did it make"

**Status** Accepted · **Date** 2026-09-04 · **Owner** @maintainers-architecture

**Relates to** [ADR-0060](0060-cloud-back-office-dashboard.md) · [ADR-0068](0068-fleet-liveness.md) · [ADR-0073](0073-alerting.md) · [ADR-0081](0081-reports-and-analytics.md) · [ADR-0087](0087-edge-relay-and-event-publish.md)

## Context

Roadmap v3 **Q4** was "store hub + URL context". The URL half shipped — a tenant is a path segment
and the store a `?store=` query, so a console link is shareable ([ADR-0060](0060-cloud-back-office-dashboard.md),
Track F1). **The screen it was for was never built**, and the slice was recorded as done on the
strength of the half that landed.

So the console has twenty-nine screens and no answer to the first question an operator actually has:
*I run six shops; is anything wrong with this one right now?* The tenant-scoped index (`/t/<tenant>`,
with a store) renders **Reports** — a windowed revenue and product-mix view. It is a good screen and
it is the wrong first screen: it answers how much the shop made before it says whether the shop is
online, whether it is running the configuration that was published to it, whether anyone has opened
a till today, whether the kitchen has run out of anything, and whether an alert is firing against it.
Each of those lives on a different screen today, so answering them means five navigations per shop.

Reports is **not** broken for the roles that cannot see money — it already hides the revenue panels
for anyone but Owner/Admin, because prices are T2 ([ADR-0081](0081-reports-and-analytics.md)). The
problem is not a permission failure. It is that the landing page is a finance screen.

## Decision

**A per-store hub becomes the tenant-scoped index; Reports moves to `/reports`.** Six cards, each one
question, each linking to the screen that can act on the answer. It is **read-only**: a hub that can
change things is a second copy of every editor, and the thing it must never do is become a place
where a price is edited by accident.

**Each card reads an endpoint that already exists.** Nothing here needs a new route, a new
projection, a migration or a permission. That is the point of the shape: a landing page assembled
from reads the console already makes is one screen of work, and a landing page that needs its own
server-side read model is a track.

| Card | Question | Read |
| --- | --- | --- |
| **Online** | Is the box talking to us, and when did it last? | `GET /admin/fleet/{store}` — `online`, `last_seen_at_ms` ([ADR-0068](0068-fleet-liveness.md)) |
| **Configuration** | Is it running what we published? | the same read — `config_version_held` vs `config_version_published`, `config_current` |
| **Today** | What has it taken today? | `GET /admin/stores/{store}/revenue/daily` for the current business date |
| **Working** | Has anyone opened a till? | `GET /admin/stores/{store}/rollups/daily` — `cash.shift.opened` minus `cash.shift.closed` |
| **Out of stock** | Has the kitchen run out of anything? | the same read — `inventory.item.sold_out` minus `inventory.item.restored` |
| **Alerts** | Is anything firing against this shop? | `GET /admin/alerts`, matched on `dedup_key` ([ADR-0073](0073-alerting.md)) |

**Two of the six are honest approximations, and say so on the card.** This is the part worth
recording, because the tempting alternative is a card that looks precise and is not.

- **"Working" is a count of shifts, not a list of names.** The cloud has no read model for *who is
  on the floor right now*: `cash.shift.opened` and `cash.shift.closed` are counted by the activity
  rollup, and the names on them are not projected anywhere. A count is a true statement — *two tills
  opened, one closed* — and it answers the operational question ("is the shop actually trading?")
  without inventing a roster the cloud does not have. A roster would also be **T1**: staff identity
  per shift is employee personal data, and it needs a lawful-basis and retention decision, not a
  card. Flagged below.
- **"Out of stock" is today's net count, not the current 86 list.** `inventory.item.sold_out` and
  `inventory.item.restored` are events, and the difference over the trading day is how many items
  are out — but *which* items needs a projection that folds those two event types into a live set,
  and none exists. Under a replay-from-zero the difference is exact; within a day it is exact too,
  because both events are counted. What it cannot do is name the dish, and it does not pretend to.

**"Today" is the only card behind a permission.** Revenue is T2, so the card renders for Owner/Admin
and shows a "you cannot see money here" line otherwise — the same rule Reports already applies, and
deliberately **not** extended to the other five. A shift count and an out-of-stock count are
operational facts, not prices; gating them behind the revenue permission would mean an Ops admin
could not see that the kitchen had run out of something, which is exactly backwards. This is why the
hub makes two reads rather than one: `GET …/reports/xz` returns activity, revenue and cash together
in one response, and using it would have dragged the operational cards behind the money gate.

**A card that cannot load says which card failed.** One failed read does not blank the page: each
card holds its own error line, because "the alert service is down" and "this shop has no revenue yet"
must not look the same, and a single page-level banner makes them look identical.

## Rejected

- **Six cards over `GET …/reports/xz`, one request instead of three.** Rejected: it is the T2
  revenue route, so the shift count and the out-of-stock count would inherit the money permission.
  One fewer request is not worth an Ops admin who cannot see that the kitchen is out of a dish.
- **Building the read models first — a live 86 set and a shift roster — and then the hub.** Rejected
  for this slice, not on principle: both are worth having, and neither is a landing page. The 86 set
  is a projection over two event types; the roster is T1 employee data needing a lawful basis under
  [ADR-0076](0076-subject-request-tooling.md)'s framing. Shipping the hub over reads that exist is what
  makes the next two slices optional rather than blocking.
- **Leaving Reports on the index and putting the hub at `/store`.** Rejected: then the hub is a
  screen an operator must know to look for, and the first thing the console shows after picking a
  shop is still a finance report. The index *is* the landing page; that is the decision.
- **Making the cards editable — publish config, acknowledge an alert, re-open a shift.** Rejected: a
  hub that writes is a second copy of five editors, each drifting from the real one, and every one of
  those actions is already audited from the screen that owns it ([ADR-0069](0069-audit-trail.md)).
  Every card links to that screen instead.
- **Auto-refresh on a timer.** Rejected for now: the reads are cheap but not free, six shops open in
  six tabs multiplies it, and a landing page that silently re-fetches is a surprising thing to leave
  on a screen overnight. The context effect already reloads on a tenant/store switch, which is when
  the numbers actually change for the reader.

## Consequences

- **The tenant-scoped index changes destination.** A bookmark of `/t/<tenant>?store=X` now opens the
  hub rather than Reports. Reports keeps every capability at `/reports`, reachable from the nav, the
  palette, and the hub's own "Today" card. No link 404s; one link lands somewhere better.
- **No server change at all.** No route, no projection, no migration, no permission, no `pos-proto`
  change, and nothing on the store side. The hub is a composition of four existing reads.
- **Two flagged follow-ups, both named on the cards themselves.** A live out-of-stock projection
  (which dishes, not how many), and a shift roster (who, not how many) — the second gated on a
  lawful-basis and retention decision, because staff identity per shift is T1 and a card is not the
  place to decide that.
- **The console's first screen is now falsifiable.** "Is this shop all right" has one place that
  answers it, so a wrong answer is a bug in one screen rather than a judgement an operator assembles
  from five.
