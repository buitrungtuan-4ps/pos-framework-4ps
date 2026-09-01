-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The OTA release registry ([ADR-0088](../../../../docs/adr/0088-ota-artifact-hosting.md), roadmap-v3
-- slice R2). R1's workflow signs an artifact per target and publishes it; the edge decides it should
-- update, asks for the bytes, and verifies the minisign signature before staging anything. Nothing
-- joined the two: `/internal/ota/artifact` had no release to serve. This table is the join's small
-- half — the row that says a release exists for a target, and how big and what digest the bytes are.
--
--   * `release`     — the tag the workflow cut (e.g. `v1.2.3`), validated as one path segment before
--                     it ever becomes an object-store key.
--   * `target`      — the Rust target triple the binary was compiled for; `x86_64-unknown-linux-gnu`
--                     and `aarch64-unknown-linux-gnu` are what the release workflow builds today.
--   * `size_bytes`  — the executable's size, so a truncated upload is visible without fetching it.
--   * `sha256`      — lowercase hex digest of the executable. An **integrity** check against a
--                     truncated upload or a corrupted blob, and deliberately not a trust boundary:
--                     only the detached minisign signature makes an artifact safe to install, and
--                     only the edge verifies it (ADR-0047). The cloud never signs and never verifies.
--   * `recorded_at` — Unix ms, stamped from the server clock.
--
-- The bytes are **not** here. They live in the object store at `releases/{release}/{target}/pos-edge`,
-- with the signature beside them as `pos-edge.minisig` — derived from this row rather than stored, so
-- a row and its blobs cannot disagree about where the bytes are. A 30 MB binary per release per target
-- in the transactional database would ride along in every WAL archive, for data that is immutable and
-- content-addressable; that is the opposite call from `media_assets` (0030), whose renditions are
-- capped at 150 KB and are cheaper in `bytea` than behind a second service.
--
-- **Immutable.** The primary key is `(release, target)` and nothing here updates it: re-recording a
-- release with different bytes is refused in the adapter (`ota::admit_artifact`), not overwritten. An
-- artifact a ring has already installed must keep meaning the same thing, or a rollback target stops
-- being a known quantity. Re-recording the *identical* digest is a no-op, so re-running a release step
-- is not an error.
--
-- **Not tenant-scoped, and so no row-level security.** A release is fleet-wide: the same signed binary
-- serves every tenant, and the row carries no tenant data — a tag, a triple, a digest, a size. There
-- is no `tenant_id` to isolate by, and no grant to `app_tenant`: the trusted admin connection the
-- server runs as is the only reader, which is why this table deliberately breaks the schema's
-- otherwise-universal RLS pattern. Forward-only and additive, applied idempotently on every boot
-- (ADR-0017). Greenfield — no backfill.
CREATE TABLE IF NOT EXISTS ota_releases (
    release     text   NOT NULL,
    target      text   NOT NULL,
    size_bytes  bigint NOT NULL,
    sha256      text   NOT NULL,
    recorded_at bigint NOT NULL,
    PRIMARY KEY (release, target)
);

CREATE INDEX IF NOT EXISTS ota_releases_by_recorded_at
    ON ota_releases (recorded_at DESC);
