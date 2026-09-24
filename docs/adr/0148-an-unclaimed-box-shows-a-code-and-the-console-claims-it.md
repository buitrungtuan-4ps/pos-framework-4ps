# ADR-0148 — An unclaimed box shows a code, and the console claims it

**Status** Accepted · **Owner** @maintainers-security · **Date** 2026-09-24
· Relates to [ADR-0051](0051-device-credential-provisioning.md), [ADR-0003](0003-cattle-not-pets.md),
[ADR-0140](0140-a-store-pc-installs-itself-from-one-file.md), [ADR-0143](0143-the-device-credential-syncs-and-events-travel-over-https.md)

## The problem

Plan item 4.1 asks for a box that is plugged in and runs. The box should come from **one image for
every store**, not an install that knows its store before it boots. Today the store id is written
into `config.toml` at install time (ADR-0140), and a box without one does not start. A golden image
therefore cannot be generic.

## Options considered

1. **Pre-register the hardware serial.** The console binds a serial number to a store, and the box
   presents its serial at first boot. But a serial is printed on the case and readable by any program
   on the box: it identifies the box, it does not authenticate it. Whoever reads one could collect
   that store's credential.
2. **Vendor attestation** (TPM-backed enrolment, as Windows Autopilot does). This is the real thing,
   but it ties the chain to one hardware and OS vendor and to a service we do not run.
3. **A claim code, as a TV signs in.** The box asks the cloud for a short code and shows it. A person
   with console rights types it in and picks the store. The box then collects its credential with a
   secret it never showed anyone.

## Decision

**Option 3**, the device-authorisation pattern (RFC 8628).

- **`pos-edge claim --cloud <url>`** runs on a box with no `config.toml`. A golden image starts it on
  first boot: a systemd unit gated on the file's absence, or a Windows first-logon task.
  1. It calls `POST /claim`, which is unauthenticated and rate-limited like `/activate`.
  2. It receives a `claim_id`, an eight-character `user_code`, and a 256-bit `secret`. The secret
     never leaves the box.
  3. It shows the code in its window and on a local page (`http://127.0.0.1:8080/`) as text and as a
     QR linking to the console with the code filled in.
- **The console claims it.** On Activation → **Claim a box**, a user with `console.devices.manage`
  enters the code and picks the store and the device slot. `POST /admin/claims/bind` binds the claim
  and audits it. A code expires after an hour, and a bound code cannot be bound again.
- **The box collects once.** It polls `POST /claim/{claim_id}/collect` with its secret. When the claim
  is bound, the cloud mints the device credential for the slot in the same transaction that marks the
  claim collected (ADR-0051's rule), and answers `{ tenant_id, store_id, device_id, credential }`
  exactly once.
  - The box writes `config.toml` and keeps the credential in its keyring.
  - It then installs and starts the service. That credential syncs (ADR-0143), so the box needs
    nothing else.
- **Only the secret's `SHA-256` is stored**, as for every machine-generated credential here.

## Consequences accepted

- **One image fits every store.** The store is chosen at the console, by a person who can be audited,
  and a stolen image claims nothing.
- **Someone must still read the code off the box.** That is the human check option 1 skipped. A store
  with no screen reads it from the local page on a phone, or from the receipt the claim prints when it
  finds a printer.
- **A claimed box is an activated box.** The activation-code path stays for boxes installed with a
  store id, and nothing about it changes.
