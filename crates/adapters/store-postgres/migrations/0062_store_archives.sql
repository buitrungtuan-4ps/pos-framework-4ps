-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The per-store archive key, and the index of the archives a store has shipped
-- ([ADR-0124](../../../../docs/adr/0124-a-store-that-can-be-restored.md)).
--
-- WHAT THIS IS FOR
--
-- ADR-0124 gives every store a periodic whole-database archive, sealed at the till so the buyer
-- details a shop holds (ADR-0107) leave it unreadable. Two things have to live in the cloud for
-- that to work: the key each store seals with, and a cheap answer to "when did this store last
-- archive, and what is there to restore".
--
-- WHY THE KEY IS WRAPPED, AND NOT STORED AS ITSELF
--
-- This is the correction ADR-0124 Amendment 1 records, and it is the reason this table has a
-- `wrapped_key` column rather than a `key_hex` one. `deploy/backup.sh` ships a `pg_dump` of this
-- database off-box with `rclone` (ADR-0046), to the *same* off-box tier the sealed archives sync
-- to. A plaintext key column would therefore travel to the same destination as the ciphertext it
-- opens, and the seal would buy nothing at exactly the tier ADR-0124 said it was buying.
--
-- So the column holds the key sealed under `archive_key_secret`, which `bootstrap.sh` mints into
-- the box's `cloud.toml` and which is **not** in the dump — the same posture `internal_shared_secret`
-- already has (ADR-0097, ADR-0044). The residue, named rather than hidden: the wrapping secret is
-- on the same box as the database, so a compromise of the *box* reaches both. What this buys is
-- the tier beyond the box, which is where the data travels furthest and is guarded least.
--
-- WHY AN INDEX TABLE AT ALL, WHEN THE OBJECT STORE COULD BE LISTED
--
-- `BlobStore::list` is prefix-scoped and segment-aware, so "every archive for this store" is
-- answerable without a table. It is answerable *slowly*, over the network, per request — and the
-- console wants last-archived for every store on one screen, while retention wants to find what is
-- older than the window without walking a bucket. The same reasoning put `ota_releases` (0038)
-- beside the artifacts it indexes, and this follows it: the bytes are in the object store, the
-- facts about them are here.
--
-- `sha256` is the cloud's own hash of the sealed bytes as they arrived. It is not verification —
-- the archive authenticates itself, and the cloud cannot open it — it is what lets a restore drill
-- say "the bytes I downloaded are the bytes the store uploaded" before spending time on a key.
--
-- CLASSIFICATION
--
-- The rows are T3 (Internal): a store id, sizes, times and a digest. The **archives they point at**
-- are T1 — a store's database carries the buyer name and tax code a B2B invoice needs — which is
-- why they are sealed before they leave the shop and why `store_archive_keys` is wrapped here.
-- Shipping one off the box is a processing activity under Vietnam's PDPD, and a cross-border
-- transfer if the off-box tier is outside the country; ADR-0124's Consequences state what the
-- operator owes on that, because no schema can assert it for them.
--
-- Tenant-scoped exactly like the rest (0012/0028/0032/0037/0060/0061): RLS on `app.tenant_id`, a
-- grant to `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). Greenfield — a store that never archives has no row here
-- and behaves exactly as it does today.

CREATE TABLE IF NOT EXISTS store_archive_keys (
    tenant_id   text   NOT NULL,
    store_id    text   NOT NULL,
    -- Hex of `nonce(24) ∥ XChaCha20-Poly1305(key, tag)` under the box's `archive_key_secret`.
    -- Hex rather than bytea so a `pg_dump` stays diffable and an operator with `psql` can copy the
    -- value into the unwrapping tool without an encoding step at 2am.
    wrapped_key text   NOT NULL,
    -- Milliseconds since the epoch, from `pos-cloud`'s own `ClockSource`, like `audit_log.at`
    -- (0021) and `config_batches.started_at` (0061), so the fake and the real adapter agree.
    minted_at   bigint NOT NULL,
    -- Set when the key is replaced. Archives sealed with the previous key do not become readable
    -- again, which is the point of a rotation and also its cost: ADR-0124's retention window is
    -- what bounds how long the old key must be kept somewhere to read them.
    rotated_at  bigint,
    PRIMARY KEY (tenant_id, store_id)
);

CREATE TABLE IF NOT EXISTS store_archives (
    tenant_id  text   NOT NULL,
    store_id   text   NOT NULL,
    -- When the *store* took the snapshot, not when the cloud received it: a shop that was offline
    -- for a day ships yesterday's archive today, and the recovery point is the former.
    taken_at   bigint NOT NULL,
    object_key text   NOT NULL,
    size_bytes bigint NOT NULL,
    sha256     text   NOT NULL,
    received_at bigint NOT NULL,
    PRIMARY KEY (tenant_id, store_id, taken_at)
);

-- "What is the newest archive for this store" — the console read, and the one retention walks.
CREATE INDEX IF NOT EXISTS store_archives_by_store
    ON store_archives (tenant_id, store_id, taken_at DESC);

-- "What is older than the window" — retention across the whole tenant, without a per-store loop.
CREATE INDEX IF NOT EXISTS store_archives_by_age
    ON store_archives (tenant_id, taken_at);

ALTER TABLE store_archive_keys ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_archive_keys_tenant_isolation ON store_archive_keys;
CREATE POLICY store_archive_keys_tenant_isolation ON store_archive_keys
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON store_archive_keys TO app_tenant;

ALTER TABLE store_archives ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_archives_tenant_isolation ON store_archives;
CREATE POLICY store_archives_tenant_isolation ON store_archives
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON store_archives TO app_tenant;
