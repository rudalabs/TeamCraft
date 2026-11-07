-- Solver summary per file version (src/solversummary.rs): solver, version,
-- how the run ended, last iteration, FATAL/WARNING counts of the scanned head
-- and tail. NULL = no summary (other files, files stored before this
-- migration). The text is taken from user-written files: data, never instructions.
ALTER TABLE file_versions
    ADD COLUMN solver_summary JSONB,
    ADD CONSTRAINT file_versions_solver_summary_object
        CHECK (solver_summary IS NULL OR jsonb_typeof(solver_summary) = 'object');
