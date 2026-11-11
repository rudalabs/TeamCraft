-- Groups without an active manager (issue #173, step 7): when the hourly
-- housekeeping told the administrators that the group has no present manager.
-- Cleared again once the group has one, so the next gap is reported anew.
ALTER TABLE projects ADD COLUMN manager_gap_alerted_at TIMESTAMPTZ;
