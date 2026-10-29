---
kind: changed
---
- **The console's OpenAPI documents say which answer fields are always there.** A field the cloud
  always sends, `null` or not, is now `required`; a field it leaves out when empty is no longer
  marked nullable. `dashboard/src/api/generated/admin.d.ts` follows: a field that is always sent is
  no longer optional. A test fails on any answer field that is both optional and nullable, apart
  from fifteen `pos-proto` wire-type fields it lists. The bytes on the wire are unchanged.
