# Go live — fork to a trading store, end to end

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-04
**Relates to** [`gate-register.md`](gate-register.md) (the gates this sequence stops at) · [`fork-checklist.md`](fork-checklist.md) (every value a fork supplies) · [`deploy-runbook.md`](deploy-runbook.md) (the cloud deploy) · [`release-runbook.md`](release-runbook.md) (cutting a signed release) · [`guides/bring-a-store-online.md`](guides/bring-a-store-online.md) (per-store provisioning) · [`guides/start-from-zero.md`](guides/start-from-zero.md) (the two-minute local version) · [`roadmap-v3.md`](roadmap-v3.md) (slice A·P5)

Five documents already describe parts of getting this system into production, each correct and each
complete about its own part. What none of them says is **what order to do them in, and where you
have to stop and wait for a person**.

That is what this page is: a spine. It repeats nothing — every step is a link and a sentence about
why it comes where it does — and its real content is the **gate** markers, because a first go-live
does not fail on a step somebody could not perform. It fails on a step nobody knew was waiting.

> **Gate markers.** A **⛔ GATE** is a point where the sequence stops until a human decides, a
> credential is issued, or hardware is verified. Every one links to its row in the
> [gate register](gate-register.md), which says who can clear it and what stays broken meanwhile.

---

## Phase 0 — Before you touch a server

Nothing here is deployed. Everything here is a decision that is expensive to change afterwards.

1. **Fork the repository.** [`fork-checklist.md`](fork-checklist.md) is the complete list of values a
   fork supplies, derived from the workflows themselves rather than from prose.

2. ⛔ **GATE — the signing key** ([H1, H2, H3](gate-register.md#1-human-decision--before-the-first-release)).
   Generate the minisign keypair offline, store the secret half on the `production` Environment, and
   set `POS_EDGE_TRUSTED_KEYS` as a repository **variable**. Do this *before* the first build: the
   trust anchor is a compile-time input, so a binary built without it can never install an update,
   and re-cutting the release is the only fix.

3. ⛔ **GATE — governance** ([H4, H5](gate-register.md#2-human-decision--before-the-first-deploy)).
   Create the `production` Environment with a **required reviewer**, and turn on branch protection.
   The reviewer is the second human the admin break-glass depends on; without one, `reset_admin` is a
   single-actor action.

4. ⛔ **GATE — hostname and TLS posture** ([H7, H8](gate-register.md#2-human-decision--before-the-first-deploy)).
   Pick `TLS_MODE` explicitly. An `*.sslip.io` name is the fastest path to a real certificate and is
   fine for a pilot — but it is **bound to the IP**, and moving the VPS later re-issues every store's
   `cloud_url`. Decide before the first real store, not after.

5. ⛔ **GATE — privacy basis** ([L1–L4](gate-register.md#5-privacy-and-legal--before-a-real-customers-data-is-processed)).
   Confirm the lawful basis, the retention period, and the Data Protection contact **before** a live
   store processes a real customer's data. The tooling deliberately does not decide any of these
   ([ADR-0076](adr/0076-subject-request-tooling.md)); it will happily run without them, which is why
   this is a gate and not a warning.

## Phase 1 — Bring up the cloud

Follow [`deploy-runbook.md`](deploy-runbook.md) start to finish. Three points in it are gates:

6. ⛔ **GATE — VPS access** ([H6](gate-register.md#2-human-decision--before-the-first-deploy)). The
   four SSH secrets, `VPS_KNOWN_HOSTS` included — without it the deploy is trust-on-first-use.

7. **Run the deploy workflow.** No command is typed on the server. `bootstrap.sh` mints the
   database password, the table-token secret, the internal shared secret and the broker token, and
   carries every existing one across on a re-run.

8. ⛔ **GATE — enrol the first super-admin** ([H9](gate-register.md#3-human-decision--at-first-boot-and-after)).
   The `otpauth://` URI is shown **once**. Take custody of it before closing the terminal; the setup
   route then answers `409` forever, and recovery is `reset_admin` with a reviewer's approval.

9. ⛔ **GATE — choose a backup destination** ([H10](gate-register.md#3-human-decision--at-first-boot-and-after)).
   Which provider, which bucket, whose credentials — that is the decision. Today you also have to put
   `RCLONE_REMOTE` on the box by hand and add the certificate-export cron line yourself
   ([A2, A3](gate-register.md#8-manual-today-and-should-not-be)); neither *needs* a person, and both
   are on the list to automate.

   Do them anyway until then, because both fail silently: an unset backup target looks identical to a
   working one right up until a restore, and a stale certificate export surfaces weeks later, at
   expiry.

## Phase 2 — Open the event bus

The store's events are what every rollup, revenue report, X/Z aggregation and reconciliation is
downstream of. A store can sell without this; the business cannot see anything it sold.

10. **Understand that the port opens by itself, or not at all.** `bootstrap.sh` publishes `4222` with
    TLS when a certificate exists in `deploy/secrets/tls/`, and binds it to loopback otherwise. On
    the ACME modes the **first deploy leaves it closed** — Caddy has not issued yet. Add the export
    cron (step 9) and redeploy.

11. ⛔ **GATE — prove reachability from outside** ([P1](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci)).
    `nc -zv <your DOMAIN> 4222`. The interaction between Docker's `internal: true` flag and a
    published port is recorded as **unverified** in [ADR-0089](adr/0089-edge-event-bus-transport.md),
    with a one-line fallback. This is the first thing in the sequence that genuinely needs a real
    box, and it cannot be inferred from a green CI run.

12. ⛔ **GATE — firewall it** ([H12](gate-register.md#3-human-decision--at-first-boot-and-after)).
    Publishing `4222` makes the broker internet-facing with nothing in front of it but its TLS and
    its token. Restrict it wherever the stores' addresses are knowable — a judgement no compose file
    can make for you.

## Phase 3 — Create the organisation

Now in the console, and entirely self-service. [`guides/bring-a-store-online.md`](guides/bring-a-store-online.md)
covers this in full.

13. **Tenant → brand → store.** The console gates every tenant-scoped screen until a tenant is
    chosen, so this is the first thing it will ask for.

14. **Author the menu.** Items, categories, tax classes, modifiers, then the per-channel menus. Nothing
    reaches a store yet — authoring and publishing are separate on purpose.

15. **Author the floor and the stations.** Areas and tables if you seat guests; stations and routing so
    the right item prints at the right kitchen printer.

## Phase 4 — Provision the store box

16. **Run the guided new-store wizard.** It emits both files an operator carries to the box: a
    `config.toml` (no secret) and an `env` file.

17. ⛔ **GATE — the store key's scopes** ([H14](gate-register.md#4-human-decision--per-store)). It needs
    **both** `read_config` and `relay_orders`. The wizard pre-selects both, and this is worth checking
    anyway, because the failure is the nastiest one in this document: with only `read_config` the
    store looks **healthy** — configuration syncs, the fleet view shows it alive — while every
    cloud-placed order gets a `403` and never reaches the kitchen.

18. ⛔ **GATE — install the `env` file as root, mode 0600** ([H15](gate-register.md#4-human-decision--per-store)).
    It carries the sync key and the broker token in cleartext.

19. **Start `pos_edge`, activate by code, name the devices.** The activation screen needs `cloud_url`
    to be present, which is why the wizard always emits it.

20. ⛔ **GATE — reboot the box once, deliberately** ([P2](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci)).
    On headless Linux the sessionless keyring is **volatile across a reboot**, so a power cycle
    re-activates the store unless the documented interim (`POS_EDGE_SYNC_KEY` in the mode-0600 env
    file) is in place. Find that out on your schedule, not during service.

## Phase 5 — Publish, then trade

21. **Publish the configuration.** The store pulls; the cloud never pushes into a shop. A published
    menu, tax table, permission set or capability flag hot-swaps the live session without a restart.

22. **Verify the loop closed, in this order.** Each of these is a different link in the chain, and
    checking them in order tells you which one is broken:
    - the store appears **alive** in the fleet view → the heartbeat loop dials;
    - the published menu appears **on the till** → config-pull works;
    - a sale on the till appears in **today's rollup** → the outbox reaches NATS and the projector ran;
    - an order placed **in the cloud** reaches the kitchen → the relay has its scope (step 17).

23. **Trade.** Open a shift, sell, settle, close. The store keeps selling with the internet down; that
    is the property everything above exists to protect.

---

## Pilot checklist

A pilot is not a longer version of the above. It is the set of things a go-live sequence cannot
prove, run deliberately, on the real machine, before a second store depends on the answer. Every row
is a gate-register entry — this is the order to attempt them in.

| # | Do this | Clears | Why it cannot be done earlier |
|---|---|---|---|
| 1 | `nc -zv <DOMAIN> 4222` from outside the VPS; record which network configuration was needed | [P1](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | Docker networking behaviour on a real host, recorded as unverified |
| 2 | Power-cycle the store box; confirm it comes back **without** re-activating | [P2](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | Keyring durability is a property of the OS and the hardware, not of the code |
| 3 | Pull the plug **mid-transaction**; confirm SQLite WAL recovery and that only the uncommitted transaction is lost | [P5](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | The reliability matrix asserts this; nothing in CI performs it |
| 4 | Cut a release, publish it to a one-store ring, let it install, self-test and (deliberately) roll back | [P4](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | The simulator proves the rollout *decision*. It cannot prove the swap |
| 5 | Print through the real receipt printer and the real KDS, for a full service | [P7](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | Device quirks are the whole point of a device matrix |
| 6 | Soak at **222 events/s** against live PostgreSQL for hours; watch for leaks | [P6](gate-register.md#6-real-hardware--cannot-be-cleared-in-ci) | NVMe `fsync` decides it and wall-clock time is the method — deliberately outside the model |
| 7 | Restore from an off-box backup into a scratch box and sign the result off | [H10](gate-register.md#3-human-decision--at-first-boot-and-after) | A backup nobody has restored is a hypothesis |
| 8 | Independent pentest | [L5](gate-register.md#5-privacy-and-legal--before-a-real-customers-data-is-processed) | By definition, someone else does it |

**Write down what you found**, including the boring answers. Half the rows above exist because an
earlier decision was recorded as "unverified" rather than guessed at, and the person who runs the
pilot is the only one who can turn those into facts. A pilot whose findings are not written down has
to be run again.

## What this page is not

- **Not a substitute for the runbooks.** Each phase links to the document that owns it; nothing is
  duplicated, because a second copy of a deploy step is a copy that goes stale.
- **Not a checklist of the code's own guarantees.** The dependency rule, the lints, the locale parity
  and the ten `xtask` gates already run on every pull request. This page only covers what they cannot.
- **Not complete for a country pack.** Japan and India need registrations no engineer can perform
  ([X1–X3](gate-register.md#7-external-registration--a-third-party-must-act)); those gate a country
  go-live, not this one.
