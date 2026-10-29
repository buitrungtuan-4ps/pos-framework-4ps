---
kind: fixed
---
- **The console's OpenAPI document named a permission that does not exist.** Routes behind the
  console's read permission said "The role lacks console.read". The permission's id is
  `console.data.read`, so a fork reading the document could not find the role grant it names. Every
  such 403 now names `console.data.read`. Nothing changes in which role reaches which route.
