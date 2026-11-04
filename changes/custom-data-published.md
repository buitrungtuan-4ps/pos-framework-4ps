---
kind: added
---
- **A package version can declare custom data.** `POST /admin/packages/{package_id}/versions` takes
  a `data` part (ADR-0194): custom fields on an order, line, bill, table or shift, object types with
  their own fields, and managed lists. Every field must say whether it is personal. The cloud and the
  edge both refuse a data part that breaks ADR-0194's rules, naming the field. A field declared
  personal is refused with `PERSONAL_FIELD_UNAVAILABLE` until ADR-0206 is accepted and personal
  records keep their own retention period. A version that removes, moves or retypes what an earlier
  version of its package declared, or makes a personal field not personal, is refused with
  `NOT_ADDITIVE`; deprecate it instead. Nothing is entered on a till yet.
