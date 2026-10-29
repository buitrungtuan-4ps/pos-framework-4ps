---
kind: changed
---
- **The till's OpenAPI document says which answer fields are always there.** A field the edge always
  sends, `null` or not, is now `required`; a field it leaves out when empty is no longer marked
  nullable, and a `drawer_open` is one of `OPENED`, `NO_DRAWER` or `DRAWER_UNAVAILABLE` rather than
  any string. `ui/src/api/generated/edge.d.ts` follows. A test fails on any till answer field that is
  both optional and nullable, apart from seven `pos-proto` and printer-port fields it lists. The
  bytes on the wire are unchanged.
