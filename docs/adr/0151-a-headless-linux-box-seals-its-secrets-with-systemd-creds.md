# ADR-0151 — A headless Linux box seals its secrets under a key systemd-creds keeps

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0086](0086-edge-keyvault-and-activation.md) (whose flagged hardware follow-up this
is), [ADR-0143](0143-the-device-credential-syncs-and-events-travel-over-https.md),
[ADR-0150](0150-the-appliance-is-a-linux-image-that-claims-itself.md), [ADR-0001](0001-offline-first-store-autonomy.md)

## The problem

On Linux the edge keeps its device credential in the kernel keyring (ADR-0086), and the kernel
keyring is empty after a reboot. A headless store box that lost power came back unactivated: no
cloud sync until someone issued a new activation code and typed it on `/setup`. Since ADR-0143 that
credential is the only one a box needs, so losing it stops config sync, the order relay and event
publishing. `docs/gate-register.md` row P2 names the answer, `systemd-creds`, and leaves it open.

Two constraints shape it. The service runs as the unprivileged `pos` user with `NoNewPrivileges`,
and `systemd-creds` needs root to encrypt (the unprivileged interface arrives in systemd 256; Debian
12 ships 252 and Ubuntu 24.04 ships 255). And the till treats "not activated" as a reason to send
the counter to `/setup`, while a vault that answers an error leaves the counter trading (ADR-0001).

## Options considered

| | Option | For | Against |
|---|---|---|---|
| a | Keep the keyring; document re-activation after a reboot | Nothing to build | Every power cut costs a store its sync until someone with console access comes |
| b | Seal the credential itself: a root path unit encrypts what the edge drops in `/run` | The credential is a systemd credential end to end | A root process reading a file the `pos` user wrote (symlink and swap attacks to defend); a race between the edge's restart and the seal |
| c | **Seal one vault key at install; the edge encrypts its own secrets with it** | No root process at runtime reads anything the service wrote; the edge's store and delete take effect at once | One more cipher use in the adapter (the one the backups already use) |
| d | A TPM-resident key and device attestation | Nothing leaves the chip, even for root | Every box needs a TPM2, and the protocol and the cloud change; a separate, larger decision |

## Decision

Option **c**.

- **At install**, as root, `deploy/edge/pos-edge-vault seal` seals 32 random bytes with
  `systemd-creds encrypt --tpm2-pcrs=`: with the TPM2 and systemd's host key together when the
  machine has a usable TPM2, with the host key alone otherwise, including when the TPM2 will not
  seal. The key is bound to the chip and not to the boot measurements, so a firmware update does not
  cost the store its activation; the threat answered is a copied disk. A key that still unseals is
  kept, so re-running an installer changes nothing. A key that no longer unseals is replaced, and it
  and the secrets sealed under it are moved aside: nothing can open them now, and if the failure was
  one that passes, moving both back undoes it. A seal that fails changes nothing, and the drop-in
  below goes in only after a seal has succeeded, so a box where no key can be sealed stays on the
  keyring rather than answering vault errors.
- **At every start**, `ExecStartPre=-+/usr/local/libexec/pos-edge/pos-edge-vault unseal` decrypts
  the key into `/run/pos-edge-vault/vault-key`, mode `0440 root:pos` in a `0750 root:pos` directory,
  so the service can read it and nothing but root can write there. The unit drop-in
  `pos-edge.service.d/vault.conf` sets `POS_EDGE_VAULT_KEY` and `POS_EDGE_VAULT_DIR`. On a machine
  with no sealed key yet, `unseal` seals one first. That is how a box made from a generic image
  (ADR-0150) gets its own key at its first boot: sealing while the image is built would give every
  box the same host key, or bind the key to the build machine's TPM2. A key that exists and does
  not unseal is reported and left for a person to replace with `seal`, because the failure may pass.
- **The helper is the only code that runs as root, and it trusts nothing the service wrote.** It
  never reads a file under the service's directory. The one path of the service's it touches, the
  secrets it moves aside, it renames as an entry (`mv -T`), so a link the `pos` user planted cannot
  make root move them into a directory of its choosing.
- **The edge** (`key-vault-keyring`'s `OsVault`) seals each secret with XChaCha20-Poly1305 into
  `/var/lib/pos-edge/vault/<secret>.sealed`, bound by its associated data to the secret's name,
  written atomically and privately, and read on the blocking pool. A secret still in the keyring
  from before the box had a vault key is moved into the sealed store on first read, so re-running the
  installer on an activated box keeps the activation.
- **A configured key that cannot be read makes every vault call fail.** It never falls back to the
  keyring: an empty keyring would read as "never activated" and take the counter to `/setup`. An
  error leaves the counter trading and cloud sync paused, and the start-up log says to run the
  installer again. No key configured at all is the keyring, as before, which is what Windows and
  every box installed before this decision keep.

This does not change the `KeyVault` port. Its contract §4 forbids writing a secret to a path and
falling back to a file; a sealed file holds ciphertext under a key only this machine recovers, and
nothing here is a fallback.

## Consequences accepted

- **Without a TPM2 a copied disk yields the credential**, because the host key is on the same
  disk. The installer says so when it seals. That is the default level of the common edge platforms,
  and the credential is scoped to one store and revoked by archiving the device.
- **Anyone who runs code as `pos` or root on the live box can read the key**, as they could read the
  credential from the process. Option d is the answer to that, and stays open.
- **A box installed before this decision gains nothing until its installer runs again**; an OTA
  update does not install units. The upgrade note says so.
- **A cleared TPM or a moved disk means activating again**, after `pos-edge-vault seal` (or the
  installer) replaces the key. Until then the box trades and does not sync.
- **A box built from an image seals its key at its first boot, and if that fails it answers vault
  errors** until `systemd-creds` works or the drop-in is removed; the start-up log says so. An image
  cannot seal in advance without every box sharing its key.
- **The proof on real hardware is still owed**: a box with and without a TPM2, power cut, comes back
  activated (`docs/gate-register.md` row P2). The container test covers the helper against a real
  `systemd-creds` and the adapter against real files, not a real boot.
