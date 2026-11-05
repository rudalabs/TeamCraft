-- Indexes found by the scale check (tests/scale.rs, see
-- docs/src/admin/operations.md → Scale check): ~100k items, ~1M BOM lines,
-- 2M audit rows.
--
-- Plain CREATE INDEX: it blocks writes to the table while it is built
-- (reads go on). On the scale check's data that takes a few seconds for
-- audit_log and well under a second for the others; the migration runs at
-- start-up before the server accepts requests, so nobody waits for it.

-- File names are matched case-insensitively across the vault: CAD file
-- references (readiness check, reference suggestions on the revision page,
-- dependents through CAD files). Without it every lookup read all files.
CREATE INDEX files_lower_filename_idx ON files (lower(filename));

-- The history of an item, revision or change (revision and change pages,
-- item history): newest first by entity id. The old (entity_type, entity_id)
-- index only served it through the skip scan of PostgreSQL 18.
CREATE INDEX audit_log_entity_at_idx ON audit_log (entity_id, at DESC);

-- The vault explorer lists a project group by item number; the new index
-- also serves every lookup by project group, so it replaces items_project_idx.
CREATE INDEX items_project_number_idx ON items (project_id, number);
DROP INDEX items_project_idx;
