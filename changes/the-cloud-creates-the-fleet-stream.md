---
kind: fixed
upgrade_note: The cloud's `[nats]` section takes an optional `subject` (default `pos.fleet.events`), the subject the fleet stream captures when the cloud creates it. Set it only if a fork renamed the fleet subject, and to the same value every store's `config.toml` carries.
---
- **A cloud with `[nats]` starts on a broker no store has used yet.** Its ingest cursor used to bind
  only an existing fleet stream, which the first store to connect creates, so a cloud started first
  refused to start (`the cloud consumer is unavailable`). It now creates the stream itself, as a
  store would, and binds an existing one as it stands.
