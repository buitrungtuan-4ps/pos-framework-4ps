---
kind: added
---
- **An HQ can read another cell's rollups over HTTPS.** The federation's fetcher calls another
  cell's public `GET /v1/stores/{store_id}/rollups/daily` with the `read_rollups` key that cell
  issued (ADR-0179 C10). Each call is vetted against SSRF, limited to 10 seconds and a 1 MiB answer,
  and follows no redirect, so a key is sent only to the cell the signed directory names. Only
  aggregates cross: a store's daily event counts, in total and by event type, and no personal data.
  The HTTPS GET it uses is the release source's, moved unchanged into one shared module; the release
  source behaves exactly as before. Nothing calls the fetcher yet: the console view follows.
