# Release runbook — building and signing an edge release

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-04
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

3. **Publish the public half — as a build input, and only as a build input.** `minisign.pub` is not a
   secret. Set the repository **variable** `POS_EDGE_TRUSTED_KEYS` to the **second line** of the file,
   verbatim; `release.yml` **fails before it builds** without it, and `pos-edge` bakes it in at compile
   time ([ADR-0092](adr/0092-artifact-trust-chain.md)):

   ```
   POS_EDGE_TRUSTED_KEYS="$(sed -n 2p minisign.pub)"
   ```

   A variable rather than a secret, deliberately: it is visible in the run log, where an operator can
   see which anchor a release was built against.

   **Not the cloud's configuration tree.** An earlier version of this step said to record the key
   "wherever the fleet's trust set is configured (the OTA trust configuration)". That is the config
   tree the cloud publishes — and a key taken from there is a key an attacker who controls the cloud
   can choose, which makes the verifier check *their* artifact against *their* key. A trust anchor
   cannot live inside the channel it protects, so `pos-edge` exposes no runtime way to supply one:
   `trusted_keys()` takes no arguments and its parser is private. Following the old instruction would
   not have produced a weaker fleet so much as one that cannot install an update at all — the binary
   refuses rather than trusting nothing — but it would have looked like a configuration problem
   instead of a build one.

   Keep a copy of `minisign.pub` with the ops keys for manual verification, and comma-separate two
   keys in the variable to keep a retirement path open ([ADR-0047](adr/0047-minisign-verification.md)).

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

5. **Upload the OTA pair to the cloud.** The Release is where a human downloads from; the cloud is
   where a *store* downloads from, and they are separate steps on purpose (the cloud never sees the
   signing key, [D1](roadmap-v3.md#debates-settled)). For each Linux target, send the **bare
   executable** and the signature line beside it:

   ```sh
   TAG=v1.2.3
   TARGET=x86_64-unknown-linux-gnu
   # `release` is the version *without* the tag's `v` — the same string the binary reports and a
   # rollout's `target_version` names (ADR-0088 Amendment 2). One release, one spelling.
   curl -fsS --cookie "$CONSOLE_COOKIE" \
     -H "content-type: application/octet-stream" \
     -H "x-pos-minisig: $(sed -n 2p "pos-edge-${TAG}-${TARGET}.bin.minisig")" \
     --data-binary "@pos-edge-${TAG}-${TARGET}.bin" \
     "https://$DOMAIN/admin/releases?release=${TAG#v}&arch=${TARGET}"
   ```

   Re-running is safe: identical bytes answer `200` instead of `201`. Different bytes for a release
   already hosted are refused with `409` — a version a ring has installed has to keep meaning the
   same thing.

   **Why the `.bin` and not the `.tar.gz`.** `UpdateInstaller::apply` writes the bytes it is handed
   as the next binary, so the signature the edge checks has to cover exactly those bytes. The tarball
   has its own signature and its own consumer; unpacking it server-side would install bytes nobody
   signed.

6. **Check what is hosted, then promote.** `GET /admin/releases/1.2.3` lists the targets the cloud
   holds. Then publish the rollout (`PUT /admin/config/ota`, or the Fleet screen). Promoting a
   version with no hosted artifact is refused — before the guard existed, a typo published fine and
   then every store in the ring fetched a `404`, which means "install nothing", so the fleet sat
   still with nothing in any log saying why.

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
