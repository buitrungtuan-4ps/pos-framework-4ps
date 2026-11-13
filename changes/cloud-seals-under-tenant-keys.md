---
kind: security
upgrade_note: Migration 0100 adds `tenant_keys`. A vendor credential or gateway header saved from this release on is sealed under its tenant's own key, which the cloud generates on first need and stores wrapped by the environment key (the file keeper over `integration_secret` until a production keeper is configured), and is stored with a `t<version>.` prefix. Values saved before keep opening under `integration_secret` as version 0, so keep that key configured; the re-seal pass that moves them follows. Nothing to do on upgrade.
---
- **Each tenant's secrets are sealed under its own key.** Vendor credentials and gateway secret
  headers are sealed under a per-tenant key that the environment key wraps, and every sealed value
  names its key's version, so keys can be rotated and a tenant's secrets erased by key (ADR-0217).
