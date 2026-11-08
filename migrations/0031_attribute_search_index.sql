-- Substring search on custom attribute values (item search, feature 5):
-- a trigram index on the same expression the search matches, so that the
-- candidate lookup of a search text keeps using indexes (see
-- 0023_performance_indexes.sql). Without pg_trgm the search stays correct
-- and reads the table instead.
DO $$
BEGIN
    CREATE EXTENSION IF NOT EXISTS pg_trgm;
    CREATE INDEX revisions_attribute_values_trgm_idx ON revisions
        USING gin ((jsonb_path_query_array(attributes, '$.*')::text) gin_trgm_ops);
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'pg_trgm not available (%): attribute search without trigram index', SQLERRM;
END $$;
