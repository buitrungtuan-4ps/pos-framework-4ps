-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0006 — the store's OTA self-test result (ADR-0048's rollback rule, ADR-0055's updater).
--
-- `decide_rollout` puts one rule above every other, above even the kill switch: a device whose
-- running version failed its self-test must revert. The fact that rule reads — `last_self_test` —
-- was process memory, and an install *deliberately restarts the edge* (ADR-0055). So the highest
-- precedence safety rule in the fleet depended on the one fact a reboot destroys, which is the
-- reboot the rule exists to recover from. A box that installed a bad build, failed its self-test
-- and restarted came back with no memory of failing, weighed itself against the same rollout, and
-- was eligible to install the same bad build again.
--
-- One row per store, not a history. The decision reads only the most recent result, and a trail of
-- past self-tests is the cloud's job: the store reports each outcome through `CloudSync::report`
-- (ADR-0078) and the cloud keeps the fleet's history. Keeping a local log too would mean two
-- answers to "did it pass" with no rule for which wins, and unbounded growth on a box that is
-- deliberately treated as cattle (ADR-0003).
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- The store's last self-test. `version` is the release that was tested — stored, not assumed to be
-- whatever the binary now reports, because that is exactly the comparison the rollback rule makes:
-- a failure recorded against a version the box is no longer running is history, not a reason to
-- revert. `passed` is the verdict. `recorded_time` is for an operator reading the file, never for
-- the decision, which is a pure comparison of version and verdict.
CREATE TABLE ota_self_test (
    store_id      TEXT NOT NULL,
    version       TEXT NOT NULL,
    passed        INTEGER NOT NULL,
    recorded_time TEXT NOT NULL,
    PRIMARY KEY (store_id)
) WITHOUT ROWID;
