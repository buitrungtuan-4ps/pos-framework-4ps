---
kind: added
upgrade_note: New optional `config.toml` key `cloud_ca_file` on the store server, and an optional `--cloud-ca-file <path>` on `pos-edge claim`. Nothing changes for a box that does not set them. `POST /api/activate` may now answer `503` with the reason `ACTIVATION_CLOUD_UNTRUSTED` or `ACTIVATION_CLOUD_UNREACHABLE` where it said `ACTIVATION_UNAVAILABLE`; a client that does not know them still has the same status.
---
- **A store server can trust a cloud on a private CA, and a failed activation says why.** Set
  `cloud_ca_file` in the store server's `config.toml` to a PEM file of the CA's root certificates, and
  every client that dials the cloud trusts it as well as the public roots. When activation fails
  before reaching the cloud, `/setup` now says whether the cloud's certificate was not trusted or the
  cloud could not be reached, in English and Vietnamese, and the store server logs the underlying
  error.
