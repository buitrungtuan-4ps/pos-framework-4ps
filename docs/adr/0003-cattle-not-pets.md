# ADR-0003 — Machines are replaceable

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** In-store hardware dies during service. Nobody on site can restore a backup or edit a configuration file, and a stalled till costs money by the minute.

**Decision.** A store is provisioned with a one-time activation code that is exchanged for credentials stored in the OS key store (TPM/DPAPI, keyring). The cloud issues a **lease** naming the single active server for a store; a revived old machine finds its lease gone and comes up read-only. Replacement is: install, enter code, restore from cloud — target 5–10 minutes.

**Consequences.**
- No split-brain and no duplicate receipt numbers, because the lease is a single-writer token.
- Restore quality depends on continuous WAL shipping; verifying it on Windows is the top pilot risk.
- Provisioning must be idempotent and safe to repeat, since operators will retry under stress.
