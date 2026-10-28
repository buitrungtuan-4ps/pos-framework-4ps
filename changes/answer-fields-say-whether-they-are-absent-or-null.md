---
kind: changed
upgrade_note: Schema only, no wire change. `docs/openapi-admin.json`, `docs/openapi-edge.json` and the generated `admin.d.ts` and `edge.d.ts` narrow 21 optional `pos-proto` fields from "absent or `null`" to "absent", and make the print job's `station_id` required (`StationId` or `null`). Every value the cloud or the edge sends is as before, `serde` still accepts `null` or absent in a request, and `PROTOCOL_VERSION` is unchanged; a client that wrote `null` into one of these fields by hand should leave it out instead.
---
- **Every answer field says whether it can be absent or `null`, never both.** The `pos-proto` wire
  types the console and the till read (floor and kitchen-station plans, campaigns, fees, the `qr` and
  `retention` nodes) mark each field they leave out when empty as not nullable, and the printer
  port's job marks its always-sent `station_id` as required. The guard tests that listed the
  exceptions now allow none ([ADR-0177](../docs/adr/0177-openapi-is-the-route-map.md) decision 3,
  [ADR-0180](../docs/adr/0180-utoipa-in-pos-cloud-config-and-pos-ports.md)).
