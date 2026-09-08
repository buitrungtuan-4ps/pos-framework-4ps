# ADR-0117 — A headless store keeps a log, and hands over its pairing code

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-08
**Relates to** [ADR-0030](0030-pairing-and-offline-auth.md) (the pairing code: short-lived, single-use, **never logged** — upheld here, not amended away) · [ADR-0035](0035-retention-and-pii-masking.md) (retention and in-place masking) · [ADR-0076](0076-subject-request-tooling.md) (erasure: the personal data is genuinely gone from the row) · [ADR-0078](0078-sync-and-ota-closure.md) §58-61 (the deferred remote log tail, whose named prerequisite is an edge-side buffer) · [ADR-0091](0091-durable-edge-auth-state.md) (pairings are durable; codes are not) · [ADR-0110](0110-edge-placement-is-a-deployment-axis.md) (the placements this has to serve) · [ADR-0112](0112-print-agents.md) (the print agent, and the CI gate that keeps it off `pos-edge`) · `docs/pos-spec.md` §9 (permissions) · [`docs/gate-register.md`](../gate-register.md) row P3 (nothing here can observe SCM)

**Context.** `telemetry::init()` (`crates/pos-edge/src/telemetry.rs`) installs one `tracing_subscriber::fmt()` whose only sink is **stdout**. A Windows service started by the Service Control Manager has no console, so that stdout is discarded. `crates/pos-edge/src/service.rs` — 215 lines of SCM protocol — sets up no sink, and `deploy/edge/install-pos-edge.ps1` configures no log file and no Event Log, only `RUST_LOG=info`. On Linux there is nothing to match, because journald captures the stream as systemd's implicit default: `deploy/edge/pos-edge.service` sets no `StandardOutput` at all.

The consequence is not a missing convenience. **It is an unbootable store.**

The pairing code is minted in exactly one place — `announce_pairing()` in `crates/pos-edge/src/server.rs`, called once per process start — and reaches the operator only as a `tracing` field. Codes are deliberately not persisted, live five minutes, are single-use, and **no route mints another**. So on Windows-as-a-service an operator can never obtain a pairing code, and no device can ever pair. Meanwhile the installer closes by telling them to open the box's address *"and pair it"*. The instruction and the mechanism disagree, and the mechanism wins.

The same discarded sink silently breaks every other diagnostic this repo's own documentation calls load-bearing:

* the `403`-on-every-relay-poll symptom of a store key issued `read_config` without `relay_orders` — the failure `deploy/edge/README.md` calls the nastiest in the document, because the box looks *healthy*;
* the missing-font warning, where that same README says **"that log line is the whole early warning"**;
* `deploy/edge/README.md`'s own claim that logging "through `tracing`" is "all any service manager needs" — which is false on Windows.

`crates/pos-print-agent` has the identical stdout-only subscriber and is registered with `sc.exe create`, so it has the same hole. Only aggregate signals reach the cloud through the heartbeat — liveness, outbox depth, lease generation, print backlog. Every line that says **why** is lost.

**Decision.**

1. **An opt-in file sink, keyed on `POS_EDGE_LOG_FILE`.** Unset — or when the process is an OTA `--self-test` child — the builder is byte-for-byte what it is today, so journald stays the only sink on Linux, `just run-edge` and the examples are untouched, and there is no double-logging. Nothing under `deploy/` sets the variable on Linux. No new dependency: every API used is already reachable through the `fmt` and `env-filter` features the crate declares.

2. **The durable log carries no credential and no personal data, enforced by the writer rather than by a comment.** `MakeWriterExt::with_max_level(INFO)` pins the *file* at info while stdout keeps whatever `RUST_LOG` admits — so an operator raising `RUST_LOG=debug` to diagnose pairing, which is exactly what an operator does, can never make a `DEBUG` span durable. `with_filter` excludes three targets from the file: `pos_edge::pairing_announce`, `pos_edge::http::auth` and `pos_edge::auth`.

3. **ADR-0030's rule is upheld, not amended away.** ADR-0030 says the pairing code *"is a short-lived secret and is likewise never logged"*. That sentence is **false in the tree today** — `announce_pairing` logs the code inside the pairing URL. Rather than weaken the ADR to match the code, the code moves to match the ADR: the three announce emissions take the `pos_edge::pairing_announce` target, keep their present text and level on stdout and journald, and are excluded from the durable file. `pairing.rs`'s module rule and `server.rs`'s narrower one are reconciled onto one statement. The rule becomes true for the first time.

4. **A headless operator obtains the code from a purpose-built ephemeral artefact.** When `POS_EDGE_PAIRING_FILE` is set, `announce_pairing` writes the resolved pairing URL there — truncate on write, mode `0600` on Unix — and `Pairing::redeem` unlinks it on success. Exposure is bounded by `CODE_TTL` and by single-use redemption: the same bound the console already had, now reachable without a console.

5. **Retention is not a code default, so ADR-0035's period does not apply.** The file's content is bounded to the current run plus one previous run by a rotate-at-start, and it carries only identifiers, counts and outcomes. The 8 MiB per-run cap is a **disk-full guard**, not a retention period — a store issued the wrong key scope logs a `403` every five seconds on a box nobody restarts, and the disk it would fill is the one holding `store.sqlite`.

6. **The per-employee sign-in, wrong-PIN and lockout stream never reaches disk.** This is why exclusion (2) names the auth targets. Without it, a shop-floor Windows box would accumulate a durable record of who signed in when and who mistyped a PIN — an attendance-and-failed-auth log outside `SubjectStore`, invisible to ADR-0035's in-place masking and to ADR-0076's erasure, and an employee-monitoring surface that would need a lawful basis, staff notification and a DPIA. This change creates none of that, and that is a decision, not an accident.

7. **Three collateral corrections ride along, because a durable log makes each one load-bearing.** `TraceLayer` records the request **path**, not the URI with its query string. `CloudHttpClient`, `Approval.code`, `HttpEdge` and `Config` get hand-written `Debug` impls that redact their credentials. The missing-glyph set in `printing.rs` is truncated, so a Vietnamese or Japanese buyer name cannot be reconstructed from a font warning.

8. **The rule is testable.** A `#[cfg(test)]` capturing subscriber asserts no PIN on a sign-in refusal, no line text on a print dispatch, and no pairing code in the filtered file stream — plus the case a naive test misses: when the **first** writer of the tee errors, which is every write under SCM where stdout is a null handle, the second still receives the whole line.

9. **No ACL is set, deliberately.** With the credential bounded to five minutes and self-deleting, this change makes `C:\ProgramData\pos-edge` strictly better than it is today, where `store.sqlite` already holds device token digests, staff sessions and subject rows permanently under the directory's inherited ACL. That pre-existing exposure is real and larger, and it is recorded as its own finding with its own gate-register row — not closed here by an `icacls` that would lock out a standard-user till.

**Alternatives rejected.**

**A Windows Event Log layer.** The most idiomatic Windows answer and the most expensive here: ~120–200 lines of `unsafe` FFI against this crate's `unsafe_code = "deny"`, so it needs a crate-level exception or a thinly-maintained third-party crate absent from the lock — re-triggering the dependency clause — plus an `EventMessageFile` registration, without which Event Viewer renders every line as *"The description for Event ID cannot be found"*. Windows-only, so ADR-0110's Linux and hosted placements get nothing and the class is fixed twice. Keep it as a later layer **on top of** this, not instead of it.

**The same sink built on `tracing-appender`.** Buys tested rotation and non-blocking writes for two new crates. Risks `deny.toml`'s `multiple-versions = "deny"`, forces a `Cargo.lock` diff that three `--locked` CI steps require to be clean, and adds a dependency argument to an ADR already arguing a security boundary. An `AtomicU64` and a rename get the value for none of that.

**The file sink with the pairing code left in it**, as first proposed — rejected on the evidence, and this is the correction that matters most. Its central claim was that the credential cannot be redacted because it *is* the payload. That is false: a distinct target plus `with_filter` separates the credential line from the durable stream in about five lines, and keeps the whole observability win without moving ADR-0030's boundary at all. It would also have retained every boot's code for the life of the file, answered only by a size cap — which is not a time bound.

**The file sink plus `icacls /inheritance:r` on the state directory** — rejected because it is a lockout shipped as a hardening. It leaves SYSTEM and Administrators only, so a standard-user till account cannot read the log, an unelevated editor cannot open it, and the documented rescue copy and by-hand `--self-test` become admin-only — contradicting the installer's own stated intent that the agent's log be *"readable by whoever is diagnosing a printer"*. The state directory is a script **parameter**, so an elevated `icacls $Root /inheritance:r` with the wrong value is a destructive ACL-reset primitive; and every external call in that script is piped to `Out-Null` while `$ErrorActionPreference = 'Stop'` does not trip on a non-zero exit code, so a failed `icacls` would report success.

**Documenting the foreground console run.** Works today — ADR-0091 made pairings durable while codes stayed in memory — and it goes into `deploy/edge/README.md` as the break-glass. Not the fix: an unattended store still gets zero observability, and it fails on the second tablet.

**A `--print-pairing-code` flag.** Reads cheap, is not viable. All three routes are closed: mint into its own memory (the service never learns it), read a persisted code (deliberately absent), or open the live store (refused on principle). Any implementation collapses into the reveal route below.

**A loopback-only unauthenticated reveal route** — the only option that moves a live boundary while fixing nothing about observability. The server uses `into_make_service()`, so no peer address reaches the router, and switching breaks every `oneshot` test. Worse, the loopback premise is void for ADR-0110's hosted placements behind a same-host proxy, where **every** peer is loopback: it becomes a credential-minting oracle for anyone who can reach the origin. Under ADR-0111 a published `/api/*` route is never removed, so it is a one-way door.

**A pairing-code file with no log.** This ADR adopts its better half. Rejected only as a *complete* answer: it leaves the class untouched.

**A longer TTL, or a re-mintable code.** A longer-lived code the operator cannot see is still invisible. Worth recording for whoever revisits it: extending the TTL is a smaller concession than ADR-0030 implies, because the failed-redemption limit and the lockout now own the guessing defence, and the TTL owns only shoulder-surfing.

**Surfacing the code on the paired-devices read.** Strict chicken-and-egg behind the paired-device gate: the first device on a virgin box gets `401`. Its residual value — an already-paired tablet fetching a code for the next one — is the right follow-up to the restart-per-till consequence below, not a fix for this bug.

**NSSM.** Honestly the cheapest complete fix available today, and the right *interim* answer for a store that needs one this week. But it is an unsigned, unversioned third-party binary this repo does not verify — contrast ADR-0047 for the edge binary itself — and it takes over the exit-action semantics `service.rs` depends on, so E4's OTA restart path would need re-verification against it.

**Consequences.**

* A Windows store can be brought online. The installer's closing promise becomes keepable, and the `403` diagnosis, the font early warning and the OTA install trail all become readable on an unattended box.
* ADR-0030's "never logged" and `pairing.rs`'s module rule stop being contradicted by the code.
* One mechanism covers the edge, the print agent and every ADR-0110 placement — Linux included, for anyone who sets the variable.
* The first half of ADR-0078's deferred log tail exists: a bounded local artefact a future tail could read. It adds no NATS subject, no request-reply and no ring buffer, and `MessageLink` stays outbound-only.
* **Adding a till to a trading store costs a service restart.** `announce_pairing` runs once per process start, and a stop drops every till's and KDS's `/ws` session and forces the outbox drain. The supported procedure is to commission tills together, or to restart outside service hours. The paired-devices follow-up above is what would remove N−1 of those restarts.
* With the variable set, the console also loses ANSI colour: `.with_ansi(false)` is a property of the fmt layer, not of one sink. Per-sink ANSI needs the layered form.
* The 8 MiB cap is **per run**, and a process that reaches it stops writing to the file rather than rotating mid-run; stdout continues. Deliberate — a per-event bound needs a lock on a till's logging path.
* `pos-cloud` keeps its own inline `.init()`. Two of the three subscriber copies are fixed here; unifying them is a separate cleanup, and `telemetry::init()` cannot be shared with the print agent at all, because ADR-0112's CI gate forbids that crate depending on `pos-edge`.
* **Unverifiable here.** Nothing in this repo can observe SCM (`docs/gate-register.md` row P3) or run `icacls`. Whether the sink produces a readable file on a real store box, and whether a code read from it redeems, is a new gate-register row — and a blocking precondition on the implementing pull request, not a footnote.
* `C:\ProgramData\pos-edge` still has no explicit ACL, and `store.sqlite` still sits there under the inherited one. Named, not fixed, with its own follow-up.
* The print agent still has no SCM handshake — no `service.rs`, no `windows-service` dependency — which is the error-1053 failure `service.rs` documents. Its log file will be empty until that is fixed, and the follow-up must reference this record.
