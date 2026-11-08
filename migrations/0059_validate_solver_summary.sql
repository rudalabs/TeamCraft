-- 0049 added the CHECK on file_versions.solver_summary as NOT VALID and never
-- validated it, so the constraint did not cover existing rows (tests/migrations.rs
-- requires every constraint to be validated). The column was new in 0049, so
-- every older row is NULL and the validation cannot fail.
ALTER TABLE file_versions VALIDATE CONSTRAINT file_versions_solver_summary_object;
