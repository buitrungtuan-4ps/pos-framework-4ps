# ADR-0142 — Windows code signing is the fork's choice, and the build says which it made

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0047](0047-minisign-verification.md), [ADR-0092](0092-artifact-trust-chain.md),
[ADR-0140](0140-a-store-pc-installs-itself-from-one-file.md), [ADR-0141](0141-the-console-hands-out-the-installer-by-name.md)

## The problem

A store PC now installs itself when its file is double-clicked (ADR-0140). Windows puts two warnings
in front of that double-click: SmartScreen for a downloaded file with no reputation, and a UAC prompt
naming an "Unknown publisher". Both are about **Authenticode**, which nothing in the release signs.
Minisign signs every artifact, but only the edge's own updater reads that signature. Windows never
does.

A single answer does not fit every fork. Pizza 4P's can buy a certificate. A fork running ten shops
may never have one, and a fork with an EV certificate cannot export the key at all, because it lives
on a hardware token or in a cloud HSM.

## Options considered

1. **Require a certificate.** This locks out every fork that has none, for a warning that minisign
   already makes harmless for updates.
2. **Hard-code one signer**, for example signtool with a PFX. This fails for the forks whose keys
   cannot leave hardware.
3. **One script that reads the fork's choice from the environment.**

## Decision

**Option 3.** `deploy/release/sign-windows.ps1` signs with whatever the fork configured, and says so
in the build summary:

| Mode | Input | For |
|---|---|---|
| `none` | nothing configured | a fork with no certificate. The build prints a notice and ships unsigned; `POS_SIGN_REQUIRED=true` turns that into a failure for a build that must be signed |
| `pfx` | `POS_SIGN_PFX_BASE64`, `POS_SIGN_PFX_PASSWORD` | a certificate from a public CA, or an **internal** one from `new-internal-signing-cert.ps1` |
| `command` | `POS_SIGN_COMMAND`, containing `{file}` | a key that cannot leave its hardware: an EV token, Azure Trusted Signing, a cloud KMS through `jsign` |

`auto`, the default, picks the first mode whose inputs are present. The same script is the Station
app's `signCommand`, so the two cannot drift.

**Order is part of the decision.** Authenticode writes into the executable, so it runs **before**
minisign. Signed after minisign, every store refuses the update: the OTA signature covers bytes that
no longer exist. The file name is not signed, so a binary signed here keeps its signature when the
console renames it for a store (ADR-0141).

**A fork with no certificate still has a path.** `new-internal-signing-cert.ps1` makes a
code-signing-only certificate, and `deploy/edge/trust-internal-signing-cert.ps1` (or Group Policy or
Intune) makes the fleet trust it. The UAC prompt then names the fork, and AppLocker or WDAC can allow
it by publisher.

## Consequences accepted

- **Wiring the step into `release.yml` is the owner's change.** `AGENTS.md` §8 bars an agent from
  editing a release workflow. The exact step is in `docs/release-runbook.md`. Until it is added,
  releases ship unsigned, as they do today.
- **An internal certificate buys no SmartScreen reputation.** Reputation comes only from a public
  CA's certificate and accrues with downloads. The runbook says so rather than promising otherwise.
- **Minisign stays the only trust anchor for updates.** Authenticode is about the first double-click
  and about Windows policy. It does not replace ADR-0047/0092, and nothing reads it at update time.
- **The scripts are parsed on every pull request.** `installer-syntax.mjs --emit` hands them to both
  PowerShell editions on the Windows runner, like the generated installers.
