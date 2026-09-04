# Gate register — everything only a human or real hardware can clear

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-04
**Relates to** [`go-live.md`](go-live.md) (the runbook these gates sit inside) · [`fork-checklist.md`](fork-checklist.md) (the values a fork supplies) · [`deploy-runbook.md`](deploy-runbook.md) (the deploy itself) · [`release-runbook.md`](release-runbook.md) (cutting a signed release) · [`roadmap-v3.md`](roadmap-v3.md) (slices A·P5 and B·W10, which are these gates) · [`capacity-and-reliability.md`](capacity-and-reliability.md) (what the model deliberately does not measure)

Most of this system enforces its own rules: the dependency rule is a test, the lints are `deny`, the
locale parity is a build step, ten `xtask` gates run on every pull request. This page is the
complement — **the things no amount of code can close**, because they need a person to decide, a
credential a machine must not hold, a physical device to be plugged in, or an outside body to
register something.

They are collected here for one reason: each of them is a **silent** blocker. Nothing goes red. CI
stays green, `main` stays green, the acceptance suite passes. The failure shows up later — at the
first release, at the first power cut, at the first tax audit — which is precisely the failure mode
this repository has spent a program fixing everywhere else.

**How to read a row.** *Kind* says who or what can clear it. *Blocks* is what stays broken until it
is cleared, stated as the symptom an operator would actually see. *Recorded in* is where the decision
lives once it is made — a gate whose answer is not written down is a gate that has to be cleared
again by the next person.

---

## 1. Human decision — before the first release

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| H1 | **Generate the minisign keypair**, on a trusted machine — not a runner, not the VPS ([D1](roadmap-v3.md)) | Every release. `release.yml` **fails before it builds** without `MINISIGN_SECRET_KEY`, because a release is never published unsigned | [`release-runbook.md`](release-runbook.md) · [ADR-0047](adr/0047-minisign-verification.md) |
| H2 | **Set `POS_EDGE_TRUSTED_KEYS`** (a repository *variable*, the second line of `minisign.pub`) | Every OTA install. It is a **build input and only a build input** — `pos-edge` exposes no runtime way to supply one, and a binary built without it refuses to install rather than trusting nothing | [ADR-0092](adr/0092-artifact-trust-chain.md) · `crates/pos-edge/src/trusted_keys.rs` |
| H3 | **Custody of the private half** — password manager or hardware key, offline, with a second key baked in where possible | Key rotation. Retiring a compromised key otherwise needs a release that the compromised key itself must sign | [`release-runbook.md`](release-runbook.md) · [ADR-0047](adr/0047-minisign-verification.md) |

H2 is the one most likely to be got wrong, because the *plausible* place to put a trust anchor is the
cloud's configuration tree, where it would be parsed, typed, and already in `EdgeSession`. A key
taken from there is a key an attacker who controls the cloud can choose, which turns the signature
check into a formality that verifies the attacker's artifact against the attacker's key. The shape
enforces it: `trusted_keys()` takes no arguments and the parser is private.

## 2. Human decision — before the first deploy

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| H4 | **A GitHub Environment named `production`, with a required reviewer** | The two-human property of the admin break-glass. Without a reviewer, `reset_admin` is a single-actor action that clears the super-admin and every session | [ADR-0045](adr/0045-first-boot-admin-enrolment.md) · [`fork-checklist.md`](fork-checklist.md) |
| H5 | **Branch protection + `CODEOWNERS` review routing** | Nothing at runtime — it gates the repository, and it is the reason a boundary change lands on someone accountable | [`engineering-guide.md`](engineering-guide.md) · `CODEOWNERS` |
| H6 | **The four VPS access secrets** (`VPS_HOST`, `VPS_USER`, `VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`) | The deploy. `VPS_KNOWN_HOSTS` in particular: without it the deploy is trust-on-first-use | [`fork-checklist.md`](fork-checklist.md) §1 |
| H7 | **Choose the TLS posture** (`TLS_MODE`) — nothing is inferred from the hostname | Certificates, and transitively the event bus (§4). The wrong posture fails at bootstrap, by design, rather than silently downgrading | [ADR-0090](adr/0090-tls-postures.md) · [`fork-checklist.md`](fork-checklist.md) §4 |
| H8 | **Choose a hostname you own, before the first real store** | Nothing immediately — an `*.sslip.io` name works and gets a real certificate. But it is **bound to the IP**: change the VPS address and every store's `cloud_url` must be re-issued | [`fork-checklist.md`](fork-checklist.md) §4 |

## 3. Human decision — at first boot, and after

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| H9 | **Enrol the first super-admin** and take custody of the TOTP secret (it is shown once) | The console. The setup route then refuses further enrolments (`409`) — the token is spent | [`deploy-runbook.md`](deploy-runbook.md) §4 · [ADR-0045](adr/0045-first-boot-admin-enrolment.md) |
| H10 | **Set an off-box backup target** (`RCLONE_REMOTE`, on the box) | Off-box durability. It is **not** a GitHub secret — setting it there does nothing and leaves off-box backups silently disabled | [ADR-0046](adr/0046-backups-and-restore.md) · [`fork-checklist.md`](fork-checklist.md) §2 |
| H11 | **Add the certificate-export cron line** on the two `acme-*` modes | A renewal reaching `secrets/tls/`, which is the path every other service reads — including the event bus. **Nothing alerts on a stale export**, so a stopped exporter surfaces weeks later, at expiry | [`deploy-runbook.md`](deploy-runbook.md) §5 · [ADR-0090](adr/0090-tls-postures.md) |
| H12 | **Restrict `4222` at the host firewall** wherever the stores' addresses are knowable | Nothing functionally — which is the problem. Publishing the port makes the broker internet-facing with nothing in front of it but its TLS and its token. Stores on residential or mobile connections have no stable address, so no compose file can decide this | [ADR-0089](adr/0089-edge-event-bus-transport.md) · [`deploy-runbook.md`](deploy-runbook.md) §6 |
| H13 | **Mint the Garage S3 access keys on the box** (`garage key create`) | The OTA artifact store (roadmap R2). Garage generates them at runtime, so they cannot be pre-created or shipped | [ADR-0088](adr/0088-ota-artifact-hosting.md) Correction 1 |

## 4. Human decision — per store

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| H14 | **Issue the store key with both `read_config` and `relay_orders`** | Orders. With only `read_config` the store looks **healthy** — configuration syncs, the fleet view shows it alive — while the relay answers `403` on every poll and cloud-placed orders never reach the kitchen. The wizard pre-selects both; a hand-issued key can still get it wrong | [`fork-checklist.md`](fork-checklist.md) §3 |
| H15 | **Install the `env` file as root, mode 0600** at `/etc/pos-edge/env` | Nothing visibly. It carries the store's sync key and the broker token in cleartext | [`fork-checklist.md`](fork-checklist.md) §3 · [ADR-0086](adr/0086-edge-keyvault-and-activation.md) |

## 5. Privacy and legal — before a real customer's data is processed

These are not deferrals. They gate the *first live store*, not a later country pack.

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| L1 | **Confirm the lawful basis** for processing under Vietnam's PDPD (Decree 13/2023) before the first live store, and record it | Nothing technical. The tooling deliberately does not decide this — fulfilment is a human obligation | [ADR-0076](adr/0076-subject-request-tooling.md) |
| L2 | **Confirm consent status, retention period, and a DPIA for customer analytics** | Analytics on customer data. `retention_days` is a value someone chooses, not a default to inherit | [ADR-0076](adr/0076-subject-request-tooling.md) · [ADR-0035](adr/0035-retention-and-pii-masking.md) |
| L3 | **Clear any cross-border transfer** — a DTA or explicit consent — before data leaves the country of collection | Any hosting region outside it. The hosting-region decision itself (APPI/DPDP) is B10.4 | [ADR-0076](adr/0076-subject-request-tooling.md) · [`roadmap-v3.md`](roadmap-v3.md) B10.4 |
| L4 | **Name the Data Protection contact** an EU-resident rights request escalates to | Nothing technical. The tool hands over the payload; it never auto-fulfils | [ADR-0076](adr/0076-subject-request-tooling.md) |
| L5 | **Independent pentest**, after the security review | Production sign-off | [`roadmap-v3.md`](roadmap-v3.md) B10.4 |

## 6. Real hardware — cannot be cleared in CI

Every row here is something the deterministic model and the in-process suites explicitly do **not**
measure, and say so at the place they stop.

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| P1 | **Prove `4222` is reachable from outside the VPS.** `nats` sits on an `internal: true` Docker network; a published port is host→container DNAT, which *should* be unaffected — **this is recorded as unverified** | Every store's event publish, and transitively rollups, revenue, X/Z and reconciliation. A one-line fallback is recorded; report which one your deployment needed | [ADR-0089](adr/0089-edge-event-bus-transport.md) · [`deploy-runbook.md`](deploy-runbook.md) §6 |
| P2 | **Headless-Linux keyring durability across a reboot.** The sessionless keyring is volatile; the production answer is a TPM-sealed credential (`systemd-creds`) and needs real hardware plus a privileged decrypt-at-start path | A headless Linux box surviving a power cycle without re-activation. Until then the documented interim is the install-time device credential plus `POS_EDGE_SYNC_KEY` | [ADR-0086](adr/0086-edge-keyvault-and-activation.md) |
| P3 | **Windows service wrapper + the WAL-on-Windows spike** | Windows as a supported store target, and the Windows half of the release matrix | [`roadmap-v3.md`](roadmap-v3.md) E4 · [`capacity-and-reliability.md`](capacity-and-reliability.md) |
| P4 | **OTA install → self-test → rollback on a real box** (the 5–10 minute Windows swap) | Trusting the rollout model on hardware. The simulator proves the *decision*; it cannot prove the swap | [ADR-0055](adr/0055-edge-ota-updater.md) |
| P5 | **Sudden power loss mid-transaction**, on the target machine | The recovery claim in the reliability matrix | [`capacity-and-reliability.md`](capacity-and-reliability.md) · [`roadmap-v3.md`](roadmap-v3.md) A·P5 |
| P6 | **Sustained soak at 222 events/s** against live PostgreSQL, for hours, without leaking | The throughput figure. The model deliberately does not measure it — NVMe `fsync` is the deciding factor and wall-clock time is the method | [`capacity-and-reliability.md`](capacity-and-reliability.md) |
| P7 | **Printer, KDS and card-terminal soak** on the pilot country's actual devices | The device matrix, and the card-terminal adapter | [`roadmap-v3.md`](roadmap-v3.md) A·P5, B10.3 |

## 7. External registration — a third party must act

| # | Gate | Blocks | Recorded in |
|---|---|---|---|
| X1 | **Japan qualified-invoice registration number** | The JP receipt block, and therefore JP go-live | [`roadmap-v3.md`](roadmap-v3.md) B10.1 |
| X2 | **India GSP / IRP sandbox registration** | The e-invoice adapter (IRN + signed QR), and therefore IN go-live | [`roadmap-v3.md`](roadmap-v3.md) B10.2 |
| X3 | **Hosting-region decision** under APPI / DPDP | Where the cloud may run for those countries. Pairs with L3 | [`roadmap-v3.md`](roadmap-v3.md) B10.4 |

---

## Keeping this page honest

Every row above cites a file that already made the claim. Nothing here is a new requirement invented
by this page, and that is deliberate: a register that *adds* obligations becomes a second source of
truth and drifts from the first. When a gate is cleared, record the answer where the *Recorded in*
column points, and mark it here — this page tracks whether a gate is open, not what the answer was.

When a gate stops needing a human — H13 disappears the day artifact credentials are provisioned by
bootstrap, P1 disappears the day someone runs `nc -zv` against a real box and writes down the result
— delete the row. A register that only grows is one nobody reads.
