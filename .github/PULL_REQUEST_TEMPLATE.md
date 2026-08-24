## What

<!-- One or two sentences. What does this change do? -->

## Why

<!-- The problem, and a link to the issue. Cite the spec section if one applies, e.g. pos-spec.md §5.3 -->

## How it was tested

<!-- Unit, contract, integration, simulator, manual on hardware. Name the tests. -->

## Checklist

- [ ] `just preflight` passes
- [ ] Tests cover the new behaviour; new adapters pass their port's contract tests
- [ ] Documentation updated **in this PR** — which file(s): 
- [ ] `CHANGELOG.md` entry added under `[Unreleased]`
- [ ] Snapshots regenerated if a public API, event, or permission changed
- [ ] Schema and protocol changes are additive (nothing renamed or removed)
- [ ] No PII in logs or event payloads; no secrets committed
- [ ] ADR merged first if this changes a port, `pos-proto`, a dependency, or a security boundary

## Impact

- **Protocol version:** unchanged / bumped to N
- **Migrations:** none / additive (`NNNN_name`) — rollback safe: yes/no
- **Defaults or permissions changed:** none / describe
- **Upgrade note for the changelog:** none / describe

## Provenance

- [ ] This pull request was produced with AI assistance (also add the `ai-assisted` label)
