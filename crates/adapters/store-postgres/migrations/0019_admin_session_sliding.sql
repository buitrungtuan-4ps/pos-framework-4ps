-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Accountable, revocable admin sessions with a sliding idle TTL bounded by an absolute cap ([ADR-0067]
-- slice 4, superseding the fixed-TTL session of ADR-0034). Like `admin_sessions` itself (migration
-- 0003) this is not tenant data: it authorises the privileged console, not a tenant's `/v1` data, so
-- no tenant_id, no RLS, no `app_tenant` grant — only the trusted pool owner touches it. Forward-only
-- and additive, applied idempotently on every boot (ADR-0017); `ADD COLUMN IF NOT EXISTS` keeps it a
-- no-op on re-boot.

-- Two nullable bigints turn the fixed 8-hour session into a sliding one with an idle timeout:
--
--   * `absolute_expires_at` — the hard ceiling (Unix ms). A session can never live past this instant
--     however active it is, preserving the ADR-0034 "one working day" bound. Set at login to
--     `now + admin_session_ttl_secs`.
--   * `idle_ttl_ms` — the idle window (a duration in ms). On each *real* guarded request the guard
--     slides `expires_at = LEAST(now + idle_ttl_ms, absolute_expires_at)`, so a session used within
--     the window stays alive up to the cap, and one left idle past the window expires. The
--     lightweight `/admin/session` liveness poll deliberately does not slide, so a genuinely idle
--     admin still times out even with the console tab left open.
--
-- Both are nullable so sessions minted before this migration stay valid and keep their original fixed
-- expiry: with either column NULL the guard leaves `expires_at` untouched (no sliding), and the row
-- ages out at its old TTL. `admin_id`, `ip` and `user_agent` — the accountability columns an admin's
-- session list shows — already exist from migration 0018.
ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS absolute_expires_at bigint;
ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS idle_ttl_ms         bigint;
