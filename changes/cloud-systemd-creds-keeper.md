---
kind: security
upgrade_note: New cloud.toml settings `key_keeper` ("file", the default, or "systemd-creds") and `key_credentials_dir`. The default is unchanged, and now logs at every boot that the file keeper is for development only. Production should move to "systemd-creds" as deploy/README.md describes (deploy/cloud-keys.sh new, then unseal before each start); keep integration_secret set until the re-seal pass has moved older values.
---
- **Production keeps the cloud's environment key in systemd-creds.** `key_keeper = "systemd-creds"`
  reads versioned keys systemd decrypted from credentials sealed to the host's TPM2, and
  `deploy/cloud-keys.sh` seals and unseals them; the file keeper stays for development only
  (ADR-0217).
