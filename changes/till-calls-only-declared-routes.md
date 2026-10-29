---
kind: added
---
- **The till's client calls only the routes the OpenAPI document declares.**
  `ui/src/api/paths.ts` derives `EdgePath`, `EdgeUrl` and `EdgeCall` from the generated edge types,
  and the request helper, the revalidating read, the store data engine's entries and the sign-in and
  sign-out calls take their URL through them, so a route or method the edge does not declare fails
  the till's build. Types only: nothing changes at run time.
