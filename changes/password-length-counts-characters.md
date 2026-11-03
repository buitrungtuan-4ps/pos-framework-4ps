---
kind: fixed
---
- **An admin password's length is counted in characters, not bytes.** `/admin/setup` and
  `/admin/invites/accept` checked the 12 to 128 character limits against the password's UTF-8 byte
  length, so a Vietnamese passphrase of about 43 or more characters was refused as too long, and one
  of fewer than 12 characters could pass the minimum. Both limits now count characters.
