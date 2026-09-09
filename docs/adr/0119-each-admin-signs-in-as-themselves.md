# ADR-0119 — Each admin signs in as themselves

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-09
**Relates to** [ADR-0034](0034-super-admin-auth.md) (the single super-admin credential this retires) · [ADR-0067](0067-multi-admin-console-rbac.md) §73 (**this is the "later slice" it named**) · [ADR-0045](0045-first-boot-admin-enrolment.md) (the break-glass this change would otherwise break) · [ADR-0069](0069-audit-trail.md) (which records the actor this fixes) · [ADR-0030](0030-pairing-and-offline-auth.md) (the store's own identity — untouched) · `crates/adapters/store-postgres/migrations/0018_cloud_admin_users.sql`

## The credential is written, and never read

That is the whole defect, and it is worth stating before anything else because every consequence below
follows from it:

* `admin_users` carries `password_phc`, `totp_secret` and `last_used_totp_step` per admin
  (migration `0018_cloud_admin_users.sql`:29-31).
* The invite-acceptance route **writes** them for every invited admin
  (`crates/pos-cloud/src/http.rs`:20847 — `NewAdminUser { …, password_phc, … }`).
* Migration 0018 **backfills** the existing super-admin's hash and secret into that table, verbatim,
  so an upgraded installation's owner is already a row there.
* `login()` reads `super_admin` — the singleton — and never `admin_users`
  (`crates/pos-cloud/src/auth/admin.rs`:749).

So the console asks a new admin to choose a password, hashes it with Argon2id, stores it, tells them
they can sign in, and then authenticates against a different credential entirely.

## Context

Two consequences, and the second is worse than the first.

**An invited admin can never sign in.** Track G1 built the roster, the invitations, the self-enrolment
flow, the roles and the per-role permission gate. All of it is reachable only by the one person who
holds the original super-admin password. Every screen in `/admins`, every role dropdown, every
"invite sent" toast describes a capability that does not exist. ADR-0067 §73 said so plainly at the
time — *"sign-in is still the single super-admin credential … per-admin email login is a later
slice"* — and this is that slice.

**The audit trail names the wrong person.** `login()` resolves the session's owner with
`acting_owner_id()`, which returns *the first active owner it finds*, whoever actually authenticated.
With one owner that is accidentally correct. With two, every action either of them takes is recorded
against whichever row came back first. ADR-0069 exists to answer "who did this"; today, on any
installation with two owners, it answers with a coin flip. Nothing warns about it, because the
session is perfectly valid — it just belongs to the wrong person.

A third thing is not a consequence but a hazard the fix creates, and it is the reason this record
exists rather than a commit message:

**The break-glass would stop recovering.** `deploy/reset-admin.sh` (ADR-0045) clears `super_admin`
and `admin_sessions`. It does not touch `admin_users`. Switch login to read `admin_users` without
changing that script and the last resort — the one an operator reaches for when both factors and all
ten recovery codes are lost — deletes a credential nothing reads any more and leaves the one that
matters in place. The operator is then locked out with the break-glass already spent.

## The decision that is actually load-bearing: what the existing owner types

Everything else here is mechanical. This is not.

Migration 0018 gave the backfilled owner a **synthetic, non-routable placeholder** address —
`owner@super-admin.invalid` — because the `super_admin` row it migrated carried no email at all. Its
own comment says the owner "replaces [it] from the console", and no route to do that was ever built.

So if email becomes a **required** field on `POST /admin/login`, then at the instant this deploys,
every existing installation's owner must type an address they have never seen, which appears in no
document, and which they cannot change because the console offers no way to change it. That is not a
migration; it is a lockout with extra steps, recoverable only by the break-glass — which needs a
second human on the `production` Environment and, per the hazard above, must itself be fixed first.

### Options

**(a) Email required.** Correct, simple, and locks out every installation on upgrade. Rejected on
that alone.

**(b) Email optional; absent email falls back to the `super_admin` singleton.** Never locks anyone
out, and keeps both code paths — including the one that binds the session to the wrong owner —
indefinitely. It preserves the defect as a supported path, which is the one thing a fix must not do.

**(c) Email required, with the migration setting a real address from deploy-time configuration.**
Needs an address in `cloud.toml` that nobody has today, on a box that cannot be asked interactively,
and gets worse the moment an installation has more than one admin.

**(d) Email optional on the wire; an absent email resolves to the single admin when exactly one
exists, and is refused when more than one does.** ← **chosen**

### Why (d)

* **It cannot lock anyone out.** Every installation today has one admin. For all of them, sign-in on
  the day this deploys is byte-for-byte what it is now: password + TOTP, no email.
* **It fixes the defect it is for.** The moment a second admin exists — which is the moment an
  invited admin needs to sign in, and the moment "the first active owner" becomes the wrong
  answer — email is required and each admin authenticates against their own row.
* **It retires itself.** The fallback is unreachable exactly when it would start being wrong. There
  is no flag to remember to turn off and no second path to keep testing.
* **It opens no enumeration surface.** With one admin, omitting the email is equivalent to naming
  it; there is nothing to learn. With two or more, an absent email is the same generic `401` as a
  wrong one.
* **It is discoverable.** The account menu (Stage 6) already displays the signed-in admin's address,
  so the owner *sees* `owner@super-admin.invalid` rather than having to be told it. Replacing it is
  the self-service route this record schedules below.

## Decision

1. **`LoginRequest` gains an optional `email`.** Additive: an existing client that omits it keeps
   working, which is what makes (d) safe.
2. **A new `AdminStore` read returns one admin's credential by email** — the hash, the TOTP secret,
   the last-used step, the id, the role and the status. It is a new method on the seam, which is why
   this record exists before the code (AGENTS.md §2).
3. **`login()` authenticates against that row** and binds the session to *that* admin's id. A
   `suspended` admin is refused with the same generic `401` as a wrong password: whether an address
   is suspended or absent is not a distinction worth handing out.
4. **When the request carries no email**, the store is asked for its admins; exactly one resolves to
   that admin, and two or more is a generic `401`.
5. **`record_totp_step` becomes per-admin.** It is global today, which with per-admin login would be
   an availability bug rather than an inconvenience: admin A signing in at step *N* would burn step
   *N* for everyone, so admin B presenting a valid code in the same 30-second window would be
   refused as a replay.
6. **Recovery codes and TOTP re-enrol act on the authenticated admin**, not on "the owner".
7. **`deploy/reset-admin.sh` clears `admin_users` as well** — and `admin_invites` before it, whose
   `invited_by` foreign key would otherwise block the delete — so the break-glass still returns the
   installation to first-boot enrolment. This ships in the same change as (3); a deploy that carries
   one without the other is the lockout described above. Every admin goes, not only the owner: the
   last resort returns the installation to first boot, and an invited admin whose own credential
   survived would be a way back in that the operator reaching for the script cannot see.
8. **`super_admin` stays in the schema and stops being read.** Dropping a table that holds the only
   copy of a credential, in the same release that changes which credential is read, is not a risk
   worth taking for a table with no rows to spare. Its removal is a later, separate, boring change.

## Consequences

**What an operator sees.** On a single-admin installation: nothing. Sign-in is unchanged. On an
installation with two or more admins: the login screen asks for an email, and each admin's own
password and authenticator work — which is the first time that has been true.

**What first-boot enrolment now does.** `POST /admin/setup` writes `admin_users` first and the
legacy `super_admin` row second, best-effort. The order was the other way round while `super_admin`
was the authority, and leaving it would have meant a store blip on the second write enrolling an
operator who could then never sign in. `create_admin_user` is first-writer-wins on the
`lower(email)` unique index, so it is also the "already enrolled" gate, and answers the same way on
an upgraded installation whose owner the migration already backfilled.

**What the audit trail gains.** The actor is the admin who authenticated. On a two-owner
installation this is a correction, not an addition: the entries it wrote before were attributed to
whichever owner row came back first.

**What the owner must still do, and what is not yet built.** The backfilled owner's address is
`owner@super-admin.invalid` until they change it, and this record does **not** build the route that
changes it — it is scheduled, not delivered. Until then a second admin can be invited and can sign
in, but the owner signs in by omitting the email, and the placeholder is what the account menu and
the audit trail show. That is honest and visible rather than silent, which is the trade for not
holding the fix behind another route.

**One thing this makes visible without fixing.** The `otpauth://` URI every enrolment returns
labels the account `super-admin`, so two admins importing their own secrets get two identically
named entries in the authenticator. That is a pre-existing defect — the invitation route has
returned that label since it shipped — and only the label is wrong; the secret is each admin's own,
so both codes verify. Putting an email in the label needs percent-encoding the `@`, which is a
small change with a real failure mode (a wrongly encoded URI produces a QR that silently enrols the
wrong secret), and it does not belong in the same change as the sign-in rewrite. Recorded, not
fixed.

**What is deliberately not decided here.** Per-email rate limiting: ADR-0067 §55 scheduled the
`email:…` limit for "when per-admin email login lands", and the limiter already refuses if any
presented key is over. Adding the key is a two-line follow-up, but it changes what an attacker can
do to a *named* admin (lock them out by exhausting their bucket), which deserves its own thinking
rather than a ride-along. The IP key keeps working meanwhile.

**Console-only, still.** These identities authenticate the console. Store staff keep the edge's
offline PIN (ADR-0030). Nothing here touches a store, a device, or the `/sync` surface.
