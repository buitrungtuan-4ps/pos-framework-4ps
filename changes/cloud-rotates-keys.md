---
kind: added
---
- **The cloud rotates its keys without stopping.** `pos-cloud keys rotate` makes a tenant's next key
  version, or re-wraps every tenant key under the newest environment key; a background pass
  re-seals older values and retires a version once nothing is under it; and
  `pos-cloud keys erase` makes a tenant's secrets unreadable at once, with the tenant id typed
  twice and the owner's approval reference, audited (ADR-0217).
