# Release runbook — building and signing an edge release

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-01
**Relates to** [ADR-0047](adr/0047-minisign-verification.md) (how the edge verifies a signature) ·
[ADR-0052](adr/0052-ota-rollout-config.md) (how the cloud advertises a release) ·
[deploy-runbook.md](deploy-runbook.md) (the cloud cell it is distributed from) ·
[`docs/roadmap-v3.md`](roadmap-v3.md) (R1)

CI builds and signs; the cloud only distributes, and the signing key never touches a VPS or a store
(debate D1). The [`release`](../.github/workflows/release.yml) workflow cross-compiles `pos-edge`,
signs each artifact with **minisign**, and attaches the artifacts and their `.minisig` signatures to a
GitHub Release. The edge verifies that signature before it installs an update (ADR-0047), so an
unsigned or tampered artifact is inert even if an attacker owns the distribution host.

This is a **human/security gate**: the workflow references a signing key it cannot create, and a
person decides which key the fleet trusts. Do the one-time setup below before the first release.

## One-time: generate the signing key and tell the fleet to trust it

1. **Generate a keypair** on a trusted machine (not a CI runner, not a VPS). A passwordless key is the
   simplest fit for CI — the GitHub Actions secret store is the thing protecting it:

   ```
   minisign -G -W -p minisign.pub -s minisign.key
   ```

   Use a password instead (`minisign -G` without `-W`) only if you will also store
   `MINISIGN_PASSWORD`; the workflow pipes it to `minisign` on stdin.

2. **Store the secret half** as a GitHub Actions secret named `MINISIGN_SECRET_KEY` — the full
   contents of `minisign.key` (both lines). Put it on the **`production` Environment** so cutting a
   release inherits the same required-reviewer protection as a deploy (ADR-0044/ADR-0045). If you used
   a password, add `MINISIGN_PASSWORD` the same way. **Never commit the secret key**; the
   secret-scanning gate (`gitleaks`) will reject it if you try.

3. **Publish the public half.** `minisign.pub` is not a secret. Record it wherever the fleet's trust
   set is configured (the OTA trust configuration, ADR-0052) so the edge's minisign verifier
   (ADR-0047) accepts signatures from this key. Keep a copy with the ops keys for manual verification.

4. **Custody.** Keep `minisign.key` offline (a password manager or an HSM/hardware key). Rotating it
   means generating a new pair, adding the new public key to the trust set *before* it signs a
   release, then removing the old one once nothing signed by it is still in a rollout.

## Cutting a release

1. Make sure `main` is green and the `CHANGELOG.md` `[Unreleased]` section describes what ships.
2. Tag and push:

   ```
   git tag v1.2.0
   git push origin v1.2.0
   ```

   The push triggers the `release` workflow. (To rebuild an existing tag — e.g. after a transient
   runner failure — run the workflow manually from the Actions tab with that tag; publishing is
   idempotent and re-uploads onto the same Release.)
3. Approve the `production` Environment prompt (the second human) when GitHub asks.
4. When the run finishes, the Release for the tag carries, for each Linux target
   (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`):
   - `pos-edge-vX.Y.Z-<target>.tar.gz` — the binary (with the real embedded UI),
   - `pos-edge-vX.Y.Z-<target>.tar.gz.minisig` — its minisign signature,
   plus `SHA256SUMS` and its `.minisig`.

## Verifying an artifact by hand

```
minisign -Vm pos-edge-v1.2.0-x86_64-unknown-linux-gnu.tar.gz -P "$(cat minisign.pub)"
```

`Signature and comment signature verified` means the artifact is authentic and unmodified — the same
check the edge makes automatically (ADR-0047).

## Deferred (flagged)

- **Windows.** `x86_64-pc-windows-msvc` is a real edge target but needs the MSVC toolchain on a
  Windows runner; it joins the release matrix with the Windows service wrapper (roadmap-v3 E4).
- **Serving the artifacts.** Publishing to a GitHub Release is R1. The OTA artifact server that mirrors
  these to the fleet's own store (Garage) and drives the rollout is R2; the cloud stays a dumb host and
  never holds the signing key.
