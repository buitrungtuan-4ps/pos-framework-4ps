# Documentation index

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-13

Every document in this repository, filed by what you want from it ([Diátaxis](https://diataxis.fr/)):
to **learn** by doing, to **get a task done**, to **look something up**, or to **understand why**.
The English documents are the single source of truth.

To find the files a change touches, start from the **[repository map](repository-map.md)**.

## Tutorials: learn by doing

| Document | Purpose |
|---|---|
| [guides/start-from-zero.md](guides/start-from-zero.md) | From a fresh clone to a sale on your laptop, then the cloud on a real server |
| [guides/bring-a-store-online.md](guides/bring-a-store-online.md) | From an empty store machine to a till taking money, driven from the console |

## How-to guides: get a task done

| Document | Purpose |
|---|---|
| [repository-map.md](repository-map.md) | To make a kind of change: the files to edit and the gate that checks each |
| [guides/README.md](guides/README.md) | Index of the task-shaped guides, and the two tiers they assume |
| [guides/write-an-adapter.md](guides/write-an-adapter.md) | Connect an external system to a port, proven by the contract suite |
| [guides/add-a-country-module.md](guides/add-a-country-module.md) | Add a country's tax-invoice rules and locale pack in three edits |
| [guides/run-the-simulator.md](guides/run-the-simulator.md) | Drive a virtual fleet through the real decisions |
| [guides/integrate-with-the-api.md](guides/integrate-with-the-api.md) | For a third party: get a credential, call `/v1`, receive webhooks |
| [guides/appliance.md](guides/appliance.md) | Turn a stock Debian or Ubuntu PC into a store appliance |
| [guides/pos-station.md](guides/pos-station.md) | Install and run POS Station, the store's desktop shell |
| [guides/replace-a-store-box.md](guides/replace-a-store-box.md) | Replace a dead store PC, or take over with a spare |
| [guides/data-subject-requests.md](guides/data-subject-requests.md) | Fulfil an access or erasure request, including in the backups |
| [deploy-runbook.md](deploy-runbook.md) | Fork, set secrets, and reach a live console with no command typed on the server |
| [go-live.md](go-live.md) | Fork to a trading store, in order, with each stop that needs a person |
| [fork-checklist.md](fork-checklist.md) | Every value a fork must configure, and where each one belongs |
| [release-runbook.md](release-runbook.md) | Build and sign an edge release, and what never touches a runner |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | From zero to a reviewable pull request |
| [../changes/README.md](../changes/README.md) | Write a pull request's change file |
| [../deploy/README.md](../deploy/README.md) · [../deploy/edge/README.md](../deploy/edge/README.md) · [../k8s/README.md](../k8s/README.md) | Run a cloud cell on one VPS, run the edge as a service, or use the optional Kubernetes lane |
| [../templates/adapter-template/README.md](../templates/adapter-template/README.md) · [../countries/zz/README.md](../countries/zz/README.md) | The scaffolds to copy for an adapter or a country |

## Reference: look something up

| Document | Purpose |
|---|---|
| [pos-spec.md](pos-spec.md) | What the product does: business behaviour, and the event catalogue (§18) |
| [naming-and-api.md](naming-and-api.md) | How to name anything: JSON, URLs, database, events, permissions, code and files |
| [ui-ux.md](ui-ux.md) | How every screen must behave |
| [glossary.md](glossary.md) | Restaurant and framework vocabulary |
| [configuration.md](configuration.md) | Every setting a store may run differently (generated) |
| [permissions.md](permissions.md) | The permission matrix and its default roles (generated) |
| [state-machines.md](state-machines.md) | Every state machine's transition table (generated) |
| [openapi.json](openapi.json) · [openapi-admin.json](openapi-admin.json) | The `/v1` integrator and `/admin` console APIs (generated) |
| [generated/crate-graph.md](generated/crate-graph.md) | The workspace crates by layer, what each depends on, and what depends on it (generated) |
| [generated/adr-index.md](generated/adr-index.md) | The ADRs nothing in the code cites, and any citation of an ADR that does not exist (generated). `cargo xtask gen-docs --adr-citations [ADR-NNNN]` prints which files cite each ADR |
| [snapshots/](snapshots/) | The published contracts: [events](snapshots/events.txt), [permissions](snapshots/permissions.txt), [settings](snapshots/settings.txt), [capabilities](snapshots/capabilities.txt), [edge routes](snapshots/routes.txt). Each is generated, and CI refuses a removal |
| [capacity-and-reliability.md](capacity-and-reliability.md) | Sizing numbers, load limits, and the failure and recovery matrix |
| [gate-register.md](gate-register.md) | The gates only a person, a credential, hardware or an outside body can clear |
| [production-readiness.md](production-readiness.md) | The open list between the tree and a shop that takes money |
| [security-review.md](security-review.md) · [security-review-ws-d.md](security-review-ws-d.md) | The pre-production security review, and the ops-hardening pass |
| [wcag-contrast-audit.md](wcag-contrast-audit.md) | The measured contrast ratio of every design token |
| [metrics/baseline-2026-10.md](metrics/baseline-2026-10.md) | The size and shape of the code before the standardisation work |
| [../CHANGELOG.md](../CHANGELOG.md) | What changed, when, and the upgrade notes |
| [../SECURITY.md](../SECURITY.md) · [../MAINTAINERS.md](../MAINTAINERS.md) | Reporting a vulnerability; who owns which area |
| `README.md` in every workspace crate, for example [pos-core](../crates/pos-core/README.md), [pos-edge](../crates/pos-edge/README.md), [store-sqlite](../crates/adapters/store-sqlite/README.md) | One crate each: what it is, what it owns, its workspace dependencies and dependents, entry points, governing ADRs, how to test it |
| [../ui/README.md](../ui/README.md) · [../dashboard/public/fonts/README.md](../dashboard/public/fonts/README.md) · [../countries/vn/README.md](../countries/vn/README.md) · [../countries/jp/README.md](../countries/jp/README.md) · [../countries/in/README.md](../countries/in/README.md) | The till's front end, the console's self-hosted typeface, and each shipped country module |

## Explanation: understand why

| Document | Purpose |
|---|---|
| [architecture.md](architecture.md) | How the system is built, and why each choice was made |
| [design-principles.md](design-principles.md) | SOLID, KISS, DRY and YAGNI as checkable rules for this codebase |
| [engineering-guide.md](engineering-guide.md) | The machinery behind the rules: branching, CI, releases, testing, governance |
| [adr/README.md](adr/README.md) | Every architecture decision with its reasoning, numbered in the order taken, and the status rules |
| [roadmap-v3.md](roadmap-v3.md) | The current roadmap: what is left, in what order |
| [roadmap.md](roadmap.md) | The original phase plan (P1–P13) and each phase's exit criterion |
| [cloud-admin-ux-plan.md](cloud-admin-ux-plan.md) | The console overhaul plan (roadmap v2) and what it found |
| [dependency-review-2026-09.md](dependency-review-2026-09.md) | The September 2026 assessment of every open dependency update |
| [../AGENTS.md](../AGENTS.md) | The rules every change follows, human or AI |
| [../vietnamese-design-archive/README.md](../vietnamese-design-archive/README.md) | The original Vietnamese design documents: frozen, not maintained |
