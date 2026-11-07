-- Extraction templates: a third method `table` for whitespace-separated
-- tables with quoted column names (Fluent report files, report-file-*.out).
-- It uses the csv_* columns for the value column and the optional row key.
-- The two CHECK constraints of 0033 that name the method were created
-- without names; they are found by their definition and replaced by named
-- ones.
DO $$
DECLARE
    c record;
BEGIN
    FOR c IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'result_templates'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%method%'
    LOOP
        EXECUTE format('ALTER TABLE result_templates DROP CONSTRAINT %I', c.conname);
    END LOOP;
END $$;

ALTER TABLE result_templates
    ADD CONSTRAINT result_templates_method_known
        CHECK (method IN ('regex', 'csv', 'table')),
    ADD CONSTRAINT result_templates_method_fields
        CHECK ((method = 'regex' AND pattern <> '')
            OR (method IN ('csv', 'table') AND csv_column <> ''));
