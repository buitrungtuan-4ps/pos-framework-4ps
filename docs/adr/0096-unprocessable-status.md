# ADR-0096 — A twelfth status, because nine refusals cannot say what is wrong with them

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-03
**Extends** [ADR-0094](0094-console-optimistic-concurrency.md) — the precedent: an eleventh status,
added the same way, for the same reason. The envelope itself is defined in `docs/naming-and-api.md`
§4 rather than in an ADR of its own.
**Relates to** [ADR-0033](0033-config-tree.md) · [ADR-0043](0043-translation-grid.md) ·
[ADR-0071](0071-config-without-json.md) · [ADR-0072](0072-floor-and-kitchen.md) ·
`docs/naming-and-api.md` §4

**Context.** Q3a and Q3b put the cloud's refusals onto the AIP-193 envelope —
`{"error":{code,status,message,details}}` — across 258 response paths. Nine were left behind, and
they were left behind for one reason: **`ErrorStatus` has no variant that maps to 422**, so a handler
that wanted to say "this is well-formed and still wrong" had nothing to say it with. Each of the nine
invented its own body instead. There are now three shapes on the wire where there should be one:

| Shape | Sites | Console |
|---|---|---|
| `{"violations":[…]}` | 4 — `http.rs` 4618, 8206, 8301, 8700 | read, as a joined string |
| `{"missing_fallback":[…]}` | 1 — `http.rs` 13205 | **not read at all** |
| bare text | 4 — `http.rs` 9803, 12522, 13880, 15148 | read as the raw body |

Nine sites answer 422 today. **Seven of them should; two of them should not** — see *Two of the nine
are not 422 at all* below. Adding the status is not a licence to keep every existing use of it.

The middle row is a live defect, not an untidiness. `dashboard/src/api/client.ts` declares
`violations?: string[]` and branches on it; nothing looks at `missing_fallback`. A translation grid
rejected for a missing `en` therefore falls through to the raw-text path, and the operator's toast
reads `{"missing_fallback":["menu.rice"]}` — JSON, in a dialog, where a sentence belongs.

The console's own comment says the quiet part: it describes "three body shapes" and calls the
migration incomplete. It undercounts. There are four, and it handles two.

**Is 422 even right, or should these be 400?** This is the question the record never asked, and it
has to be answered before adding a status, because the cheap fix is to call all nine
`INVALID_ARGUMENT` and be done.

They are not `INVALID_ARGUMENT`. Every one of the nine has parsed the request, validated each field
individually, and found the *combination* wrong:

- a floor plan whose routing rule names a station that does not exist (4618);
- capability flags that violate a §10 inter-flag rule — this flag requires that one (8206, 8301, 8700);
- a translation grid where some key lacks the always-present `en` fallback (13205);
- a price book that does not compile from otherwise-valid parts (12522).

The distinction matters to the person reading the error. `400 INVALID_ARGUMENT` means *you typed
something wrong, look at the field named in `details`*. These mean *every field is fine and the
result is still inconsistent, go and look at the other screen*. Collapsing them into 400 would send
an operator hunting for a typo in a form where there is none. That is a worse error message than the
raw JSON we ship today, because it is confidently wrong rather than obviously broken.

**Two of the nine are not 422 at all.** Found while reading the sites to implement this, and worth
stating before the status exists, because a new variant is exactly the kind of thing that gets
applied by find-and-replace to every site that currently answers the code.

`http.rs` 13880 and 15148 both refuse a password shorter than `MIN_PASSWORD_LEN`. That is **one field,
out of range** — the definition of `INVALID_ARGUMENT` given above, and the shape this repo already
uses everywhere else for exactly this: `("pin", "OUT_OF_RANGE")` at 1791, `("cutoff_hour",
"OUT_OF_RANGE")` at 2781, `("tax_rates", "OUT_OF_RANGE")` at 6639. Those two sites answering 422 is
a pre-existing inconsistency, not evidence about what 422 is for. They become
`400 INVALID_ARGUMENT` with `("password", "OUT_OF_RANGE")`, and the doc comment at 13852 that
promises `422` is corrected with them.

The other seven are cross-field or cross-referential and stay 422:

- 4618, a routing rule naming a station that does not exist;
- 8206, 8301, 8700, capability flags violating a §10 inter-flag rule;
- 13205, a translation key missing its `en` fallback;
- 12522, a price book that will not compile from otherwise-valid parts;
- 9803, an image the pipeline understood and could not reduce within the size budget — the request
  is well-formed and the *content* cannot be processed, which is the textbook case.

So the split is **seven and two**, and the test that separates them is the same one that justifies
the status at all: is there a field to name? If yes it is 400, whatever the handler answers today.

**Decision. Add `ErrorStatus::Unprocessable`, wire token `UNPROCESSABLE`, mapping to HTTP 422.**

The twelfth variant, added exactly as ADR-0094 added the eleventh: a new arm in the enum, in `ALL`,
in `as_wire`, and in `http_code`; `is_retryable` unchanged (it is a `matches!` over the retryable
set, and this is not in it — resending an inconsistent configuration produces the same
inconsistency).

`docs/naming-and-api.md` §4 says "Two statuses are ours, not AIP's". It becomes three. That sentence
is the licence this uses: the repo already decided that AIP's list is a starting point rather than a
ceiling, and 412 was added the moment a real refusal needed it. This is the same case.

**The nine sites then divide by what they actually know.**

- **Prose violations → the message.** The floor, capability and menu-compiler refusals carry
  human-readable sentences from `pos-core`, not field paths. They join into `message`. Inventing a
  `field` for them would be fabricating structure the domain never produced.
- **Keyed violations → `details`.** The translation grid knows exactly which keys are at fault, so
  each becomes one `{field: "<key>.en", reason: "REQUIRED"}` entry. `<key>.en` rather than `<key>`
  because it is the `en` value that is missing, and a path a reader can act on beats a name they
  have to interpret. This is the case that most needs the envelope and currently has the worst
  behaviour of the nine.

**Scope.**

In: the twelfth variant; the seven conversions to it; the two re-classifications to
`INVALID_ARGUMENT` and the doc comment that promises otherwise; the `pos-edge` match arms the new
variant breaks; the console's dead `violations` branch and its stale comment; the CHANGELOG's "Known
limitation" note, which undercounts the plain-text sites and omits the JSON-shaped ones entirely.

Out, with reasons:

- **The 22 plain-text `400` sites.** A separate, larger conversion. They are expressible today —
  `InvalidArgument` exists — so they are a migration backlog, not a missing capability. Nothing here
  blocks them, and folding them in would hide a nine-site behavioural fix inside a thirty-site sweep.
- **The proposed `envelope` xtask gate.** It should exist, and it should be written *after* this: a
  gate added today fails on day one with those 22, and a gate whose first act is to be silenced with
  a 22-entry allow-list teaches everyone to add entries to it.
- **`pos-edge`'s own refusals.** The edge has its own ~21 refusal constructions and its own client
  that cannot read the envelope. Converting it is a real slice; this one only keeps its `match`
  arms compiling.

**Alternatives.**

- **Call all nine `INVALID_ARGUMENT` (400) and add no variant.** Rejected above: it is one line of
  code and a permanently worse error message, because it erases the difference between a bad field
  and an inconsistent document.
- **Keep the bespoke bodies and teach the console to read all four shapes.** Rejected. It fixes the
  visible bug and entrenches the cause — four parsers for one concept, each of which a future route
  can quietly fail to match, which is precisely how `missing_fallback` got missed.
- **Reuse `FailedPrecondition` (409).** Rejected. 409 means the resource is in the wrong *state* for
  this request — a second `bill:settle`. These requests are refused on their own content, whatever
  state the resource is in; a caller that retried after the state changed would be refused
  identically. Same objection to `VersionMismatch`, which is about a version, not content.
- **A `violations` array as a first-class envelope field.** Rejected. `details` already carries
  `{field, reason}` and is already parsed by the console; a second list beside it would be a second
  thing to check and a second thing to forget.

**Consequences.**

- A wire status appears that older readers have not seen. This is safe by construction:
  `Open<ErrorStatus>` keeps an unrecognised token as text rather than failing the parse, which is why
  the type exists. A build that predates this reads `UNPROCESSABLE` as an unknown status and still
  renders the message — degraded, not broken.
- `pos-edge` will not compile until its `ErrorStatus` matches gain the arm. That is the intended
  effect: those matches are exhaustive on purpose so a new status cannot be added without every
  reader deciding what it means. The relay's status→class mapper must decide explicitly rather than
  letting its `_` arm swallow the new one into `failed_precondition`.
- The console loses a branch and a field. Four body shapes become two — the envelope, and raw text
  for whatever has not been converted yet.
- None of the nine routes appears in `docs/openapi.json`, so no published contract changes. The
  status is nonetheless documented in `naming-and-api.md` §4, where the other eleven are.

**Delivery.** This ADR, then one slice: the variant, the seven conversions, the two
re-classifications, the edge match arms, the console cleanup, and the test assertions that currently
pin the old body shapes.
