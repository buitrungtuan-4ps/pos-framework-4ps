---
kind: added
---
- **A cell redirects a tenant homed elsewhere to its home cell.** A request whose hostname's first
  label is a tenant slug that the signed directory homes in another cell is answered with a redirect
  to that cell's base URL, keeping the path and query (ADR-0179 C5): `302` for a browser navigation,
  `307` for an API call, which keeps its method and body, and never `301` or a proxy. The redirect
  sets a host-only `cell_id` cookie naming the home cell, with no personal data and no `Domain`. A
  request to the cell's own host, an unlisted tenant, a tenant homed here and every request to a
  single cell pass through untouched.
