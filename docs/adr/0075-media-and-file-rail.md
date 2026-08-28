# ADR-0075 — Media & file rail: images in Postgres `bytea`, and a CSV import/export rail with dry-run validation

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-28
**Relates to** [ADR-0042](0042-image-pipeline.md) · [ADR-0031](0031-cloud-adapter-transports.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0066](0066-cloud-catalog.md) · [ADR-0069](0069-audit-trail.md) · [ADR-0007](0007-in-house-vs-dependency.md) · `docs/cloud-admin-ux-plan.md` Track M5

**Context.** Track M5 is the *media & file rail*. Two of its three surfaces are decided here (the third,
PDPD subject-request tooling, is PII-sensitive and gets its own record, ADR-0076 (subject-request tooling, this track's final slice)).
The gaps this closes:

- The [ADR-0042](0042-image-pipeline.md) image pipeline (`pos_cloud::images::render`) is built, pure, and
  heavily tested — **but it has no caller and no storage**. ADR-0042 explicitly deferred both: *"Where
  renditions live — a Postgres `bytea` table rather than the `blob-garage` port, which ADR-0031
  schedules for deletion — and the admin route that accepts an upload, calls `render`, and stores the
  output, build on this pure pipeline behind a seam."* This ADR is that follow-on.
- A catalog item and a brand have no image; the operator cannot attach a photo or a logo.
- There is no bulk data path. An operator authors items, prices, translations, and employees one row at
  a time in the console; there is no export for reporting or backup, and no import for onboarding a menu
  from a spreadsheet.

**Decision.**

1. **Image renditions live in a Postgres `bytea` table, behind a `MediaStore` seam — not `blob-garage`.**
   Per [ADR-0042](0042-image-pipeline.md) and [ADR-0031](0031-cloud-adapter-transports.md) (object
   storage is scheduled for deletion once WAL shipping is in-house), renditions are stored in a new
   tenant-scoped table `media_assets` (migration 0030): a minted `MediaId` (ULID), the `content_type`
   (`image/jpeg` today), the two JPEG renditions the pipeline produces (`thumbnail`, `detail`) as
   `bytea`, and the detail byte size for listing without shipping the bytes. RLS-isolated on
   `app.tenant_id` exactly like every other cloud table, `GRANT SELECT, INSERT, DELETE` (media is
   immutable — a change is a new upload plus a delete, never an UPDATE). A `MediaStore` seam
   (`put` / `get` a single rendition / `list` summaries / `delete`) lives in `pos-cloud`; `store-postgres`
   implements a `PostgresMedia` adapter and `pos-cloud`'s `persistence.rs` bridges it, the same
   direction every other seam follows.

2. **The upload route re-encodes; it never stores raw bytes.** The admin upload route (slice 2) reads
   the multipart body under a hard size limit, calls `images::render` (which bounds the output to the
   ≤30 KB / ≤150 KB budgets and rejects a non-image with a clean `Decode` error), and stores **only the
   two renditions** — the original upload is never persisted. Serving is two authenticated routes that
   stream the thumbnail or detail with `image/jpeg` and a long immutable cache header (renditions are
   content-addressed by an immutable id). Behind a new `console.media.manage` permission
   (deny-by-default, Owner/Admin), audited (`media.upload`, `media.delete`).

3. **A catalog item gains an optional `image_ref` (a brand logo follows with receipts).** `CatalogItem`
   gains an `image_ref: Option<MediaId>` (an additive column, slice 3), authored in the console and
   displayed in the dashboard. This is an **authoring/display** concern: the compiled `MenuBook` the
   edge reprices from is unchanged — images do not cross to the edge in this track (flagged below).
   Deleting a media asset an item still references is allowed; a dangling ref serves a placeholder,
   never an error (the never-blank posture). `BrandRecord` gains the same shape with the receipt /
   branding work, whose renderer is the brand logo's actual consumer — so it lands there rather than
   ahead of a caller (flagged below).

4. **A CSV import/export rail, dry-run-first.** Buy the `csv` crate ([ADR-0007](0007-in-house-vs-dependency.md):
   RFC-4180 quoting/escaping is fiddly-but-bounded, the wrong thing to hand-roll). **Export** (slice 5)
   ships as a reusable serialiser (`pos_cloud::export`, pure and unit-tested) behind two authenticated
   routes — **catalog items** (`ManageCatalog`) and the **translation grid** (`ManageTranslations`) — and
   the dashboard downloads each with a `Blob` download. Each is audited (who exported which domain and how
   many rows, never the contents). The **employee** (T1) and per-channel **price/placement** (T2-verbatim)
   exports, and the rollup **report** export, are **deferred to a human-reviewed slice** — see decision 5.
   **Import** (slice 6, `pos_cloud::import`): the operator uploads a CSV; the server parses and validates
   it and returns a **dry-run report** (row-by-row: would-create / would-update / rejected-with-reason)
   **without writing**; a second, explicit confirm applies it (the valid rows are merged, rejected rows
   skipped). It ships for the **translation grid** — the clean round-trip with the translation export.
   **Item import** (upsert with FK validation and id minting) is deferred to its own slice, beside the
   T1/T2 export domains. XLSX is **deferred** — CSV is the interoperable floor; XLSX pulls a heavier
   dependency and buys little over CSV.

5. **Data-classification guardrails are part of the design, not an afterthought.** Employee and subject
   data are T1; prices are T2. Therefore the export rail ships **only the non-personal domains** (items,
   translations) autonomously: every export is permission-gated and audited (the audit entry records who
   exported which domain and how many rows, never the row contents), and the rail is *tenant-scoped by
   construction* (RLS), so it cannot become a cross-tenant profiling path. A **T1 export (the employee
   roster) is a bulk-T1-export path the organisation's data-classification policy escalates**, and a
   **verbatim T2 export (per-channel prices) reproduces a protected pricing model** — so both, and any
   subject-data export, wait on a human-approved design/DPIA rather than shipping here. An unbounded
   cross-tenant T1 export is out of scope and remains an escalation, never a feature.

**Rejected.**

- **`blob-garage` / S3 for renditions** — rejected by [ADR-0042](0042-image-pipeline.md) /
  [ADR-0031](0031-cloud-adapter-transports.md): the object-storage port exists only for Litestream and
  is scheduled for deletion; adding a product dependency on it now would be building on a condemned
  seam. Postgres `bytea` keeps renditions in the one durable store the cloud already backs up.
- **Storing the original upload** — rejected: it is unbounded, is the attack surface `render` exists to
  contain, and nothing reads it. Only the two bounded renditions are kept.
- **XLSX in this track** — deferred, not rejected: CSV first; XLSX can be an additive reader/writer later.
- **A generic "import anything" engine** — rejected for now: each domain's columns and validation differ
  enough that a per-domain typed import (with a shared dry-run/report shape) is clearer and safer than a
  configurable mapper.

**Consequences.**

- `image` already joined `pos-cloud` (ADR-0042); `csv` joins it for the file rail. `cargo-deny` passes
  with permissive-licensed, advisory-clean additions.
- The image pipeline gains its first and only caller; renditions are bounded and tenant-isolated.
- Migrations 0030 (media_assets) and the additive `image_ref` columns are forward-only and additive; no
  `PROTOCOL_VERSION` bump (nothing here crosses the edge wire).
- Every write and every export is audited; the rail is RLS-tenant-scoped, so it is not a profiling path.

**Deferred / flagged follow-ups.**

- Images to the edge (item thumbnails in the compiled `MenuBook` / POS UI) — a wire change deferred to a
  later track; this track keeps images to cloud authoring/display.
- **T1/T2 CSV export & import — the employee roster (T1), per-channel prices/placements (T2 verbatim),
  and the rollup report.** Deferred to a human-approved design/DPIA per decision 5, not shipped in the
  autonomous slices; the rail's serialiser and dry-run shape are built so adding them later is additive.
- **Item CSV import** (upsert by id with tax-class/category FK validation and id minting) — the natural
  round-trip with the item export, deferred to its own slice; the dry-run report shape carries over.
- XLSX import/export.
- Receipt templates + brand logo/footer rendering — depend on this track's media rail and land with the
  receipt work (M4 flagged them here); the `BrandRecord.image_ref` column lands there, beside its
  renderer.
- PDPD subject-request tooling — ADR-0076 (subject-request tooling, this track's final slice), the PII-sensitive surface,
  delivered as this track's final slice.
