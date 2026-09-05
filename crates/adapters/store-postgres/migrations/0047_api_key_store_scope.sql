-- Bind a store's API key to that store (production-readiness S1).
--
-- `api_keys` carried only `tenant_id`, and the `/sync/stores/{store_id}` routes checked the tenant
-- from the verified grant against the store id in the *path* — which within one tenant is no check
-- at all. Any store's key could read a sibling store's `permissions` node: employee names and PIN
-- hashes. Nullable on purpose: NULL is a tenant-wide key (an integration reading a tenant's
-- rollups), which is what every existing key is; a store's own credential is issued with this set,
-- and the `/sync` routes require it.
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS store_id text;

-- Listing a store's keys — the console's "which key does this store hold" question, and the
-- revoke-on-decommission path. Partial, because most rows are tenant-wide and index nothing useful.
CREATE INDEX IF NOT EXISTS api_keys_store ON api_keys (store_id) WHERE store_id IS NOT NULL;
