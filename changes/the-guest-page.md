---
kind: added
upgrade_note: A guest order through `POST /v1/qr/orders` is now refused `400` when it carries no line or more than 30, a quantity that is not 1 to 99 whole items, more than 16 modifiers on a line, or a note. The guest page sends none of these; an integration that posted guest orders by hand must keep within them.
---
- **A guest orders from the table on a page the cloud serves** (ADR-0224 decision 2). `/qr/#t=<table
  token>` is a small page with no dependencies: the menu in the guest's language, modifier
  choices, an order kept in the browser tab, and a send through the existing intake. It asks for no
  name, phone, email or note, its strings are translated (`en`, `vi`), and it runs under a content
  security policy that allows only its own script, style and requests. Payment stays with staff.
