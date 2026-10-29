# ADR-0176 — The adapter and country standard

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-20
· Carries out [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md) decision 5 for
adapters and countries · Relates to [ADR-0026](0026-port-shapes.md),
[ADR-0027](0027-country-modules.md), [ADR-0105](0105-a-country-pack-is-values.md),
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md)

Approved by the orchestrator under the owner's standing approval of 2026-10-13 and 2026-10-14
([plan](../plans/standardisation.md), Decisions taken), with the phase 8 plan on 2026-10-20. What
needs the owner is left out of this record and listed under its consequences.

## Context

The owner put the adapter standard ahead of choosing vendors (2026-10-14).

- **Adapters.** Today a new adapter is a hand copy of `templates/adapter-template`: six `.tmpl`
  files and a twelve-step checklist. The template has the shape of a network adapter, so it does not
  fit a device driver (printer, card terminal) or storage.
- **Countries.** A new country is a hand copy of `countries/zz`, then edits in about ten files.
  `xtask countries` catches two of the edits a person can forget.
- **Ports.** A port and its shared contract suite already exist for four kinds of adapter:
  - printer: `PrinterDriver`;
  - storage: `BlobStore`;
  - card payment: `PaymentTerminal`;
  - e-invoice: `Fiscalization`.

  QR payment has no port: [ADR-0155](0155-a-qr-payment-is-confirmed-by-its-gateway-through-the-cloud.md)
  is proposed.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep copying templates by hand, with better guides | Every step stays a step a person can miss; the template still fits one shape |
| B | An external generator such as `cargo-generate` | A new tool dependency, outside the checks `xtask` already runs |
| C | **`cargo xtask new`, with its templates inside `xtask`** | `xtask` carries one template per kind, which must keep compiling against its port |

## Decision

Option **C**.

1. **Two commands.**
   - `cargo xtask new adapter --kind printer|storage|card|fiscal`.
   - `cargo xtask new country <cc>`.
   - Both are built on `cargo xtask new crate`.
   - The templates live in `xtask/templates/`, embedded with `include_str!` and filled in by plain
     `{{placeholder}}` substitution. `xtask` gains no dependency.
2. **A generated crate is complete from its first commit.** It has:
   - a README and contract card that pass `xtask crates`;
   - a place in the workspace `members`;
   - an implementation of its port;
   - its port's shared suite from `pos-contract-tests`, wired in `tests/contract.rs` and passing
     against a stub, with no socket and no device.
3. **No vendor.** A generator emits a `<family>.sandbox` provider ([ADR-0153]) and takes no vendor
   name. An adapter for a real vendor is written from the sandbox once the owner has chosen that
   vendor.
4. **A country's law is an input, never a default.** Each of these is a required flag, and the
   command refuses to run without it:
   - the currency, read with `CurrencyCode::parse`;
   - its exponent;
   - whether prices include tax;
   - the cash rounding;
   - the tax rates.

   The fiscal half starts as `OfflineFiscalization`, as `zz`, `vn`, `jp` and `in` do, and passes the
   fiscalization suite. The command also writes the module's wiring:
   - its line in the workspace `members`;
   - a `country-<cc>` feature and optional dependency in `pos-edge` and in `pos-cloud`;
   - the `country_registry!` arm in each binary.

   A new country is in no binary's default features.
5. **A generator writes only what it is for.**
   - It writes the new directory and the wiring lines above.
   - It refuses to overwrite an existing file, and `--dry-run` prints the result instead of writing it.
   - It never edits `pos-core`, `pos-ports`, `pos-proto`, `.github/`, `CODEOWNERS` or any `AGENTS.md`.
6. **`templates/adapter-template` is removed** once `new adapter` covers its kinds. The guides then
   point at the commands.

## Consequences accepted

- **Templates and suites change together.** A change to a port's contract suite updates that kind's
  template in the same pull request, and `xtask`'s golden tests hold every template.
- **Built-crate check.** No CI job builds a freshly generated crate until the owner approves one
  under `.github/`. Until then, the golden tests and `just preflight` are the only checks.
- **Held for the owner, outside this record:**
  - QR payment, until a `QrGateway` port is accepted ([ADR-0155]);
  - registering card drivers on the edge ([ADR-0154](0154-a-card-is-taken-through-the-stores-terminal.md));
  - the cloud submitting e-invoices ([ADR-0156](0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md)).

  Each of these becomes another generator kind, or a change to one, once its record is accepted.

[ADR-0153]: 0153-a-vendor-is-a-provider-the-cloud-chooses.md
[ADR-0155]: 0155-a-qr-payment-is-confirmed-by-its-gateway-through-the-cloud.md
