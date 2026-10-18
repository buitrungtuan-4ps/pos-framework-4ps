---
kind: changed
---
- **`pos-render` moves to harfrust 0.14 and skrifa 0.48 together**, which share `read-fonts` 0.45.
  harfrust 0.14 shapes with `read-fonts`' own font model, so each loaded face now holds one,
  built once over the bytes it already shares and keeping its prepared layout tables between
  lines. Printed output is unchanged: the print preview of Latin, Vietnamese, Japanese, Chinese,
  Korean, Devanagari, Thai and Arabic lines is byte-identical before and after. Dependabot
  now proposes the two crates in one pull request. (#654)
