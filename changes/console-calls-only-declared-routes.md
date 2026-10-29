---
kind: added
---
- **The console's client calls only the routes the OpenAPI document declares.**
  `dashboard/src/api/paths.ts` derives `AdminPath`, `AdminUrl` and `AdminCall` from the generated
  `/admin` types, and every request helper in `dashboard/src/api/client.ts` takes its method and
  URL through them, so a route or method the cloud does not declare fails the console's build.
  Types only: nothing changes at run time.
