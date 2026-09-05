-- Approval records what discovery cannot know (ADR-0100, production-readiness C2 slice 2a).
--
-- A proposal carries only what a box can *find* on its LAN: kind, name, address. It cannot find
-- that the printer at the counter is on USB with a drawer under it, or that the one at
-- 192.168.1.50 belongs to the oven. Both facts decide behaviour — a cash drawer opens only over USB
-- (docs/architecture.md §5), and a fired line routes to a station — so they are captured at the
-- moment a human approves the device, which is what ADR-0041 made approval for.
--
-- Nullable, and set by the resolve path rather than the propose path: a pending row genuinely does
-- not know them, and a NOT NULL default would put a guess in the database and call it a fact.
ALTER TABLE device_proposals ADD COLUMN IF NOT EXISTS connection text;
ALTER TABLE device_proposals ADD COLUMN IF NOT EXISTS station_id text;
