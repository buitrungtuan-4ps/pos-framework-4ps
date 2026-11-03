---
kind: security
---
- **Add maximum password length limit to prevent password-hashing DoS.** `pos-cloud` admin enrolment (`/admin/setup`) and invite acceptance (`/admin/invites/accept`) now enforce a maximum password length of 128 characters (`MAX_PASSWORD_LEN`), mitigating denial-of-service attacks that exhaust server CPU and memory via oversized payloads submitted to Argon2id hashing.
