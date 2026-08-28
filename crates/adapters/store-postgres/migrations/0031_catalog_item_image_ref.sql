-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An image reference on a catalog item (Track M5, ADR-0075). An item can carry a photo that points at
-- a `media_assets` row (0030) by its `MediaId`. Application-enforced, no foreign key (the no-FK posture
-- every cloud table keeps): a dangling reference — the media asset was deleted — serves a placeholder,
-- never an error, so an FK cascade is neither needed nor wanted.
--
-- `image_ref` is a nullable `text` (a ULID string), `NULL` meaning "no image". This is an
-- authoring/display concern only: the compiled `MenuBook` the edge reprices from is unchanged, so
-- nothing here crosses the edge wire. Forward-only and additive, applied idempotently on every boot
-- (ADR-0017): `ADD COLUMN IF NOT EXISTS` so re-running is a no-op. Tenant isolation, RLS, and grants
-- are inherited from `catalog_items` (0012). (Brand logos will gain the same shape with the receipt /
-- branding work, whose renderer is their consumer — see ADR-0075.)

ALTER TABLE catalog_items ADD COLUMN IF NOT EXISTS image_ref text;
