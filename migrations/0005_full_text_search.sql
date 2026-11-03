-- Full-text search. 'simple' configuration: no language-specific stemming,
-- because engineering data mixes languages and part names are not words.

-- Metadata of revisions (name > material > description).
ALTER TABLE revisions ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('simple', coalesce(name, '')), 'A') ||
    setweight(to_tsvector('simple', coalesce(material, '')), 'B') ||
    setweight(to_tsvector('simple', coalesce(description, '')), 'C')
) STORED;
CREATE INDEX revisions_search_idx ON revisions USING GIN (search_vector);

-- Text extracted from file versions (STEP/IGES entity names, text files).
ALTER TABLE file_versions ADD COLUMN content_text TEXT NOT NULL DEFAULT '';
ALTER TABLE file_versions ADD COLUMN content_vector tsvector GENERATED ALWAYS AS (
    to_tsvector('simple', content_text)
) STORED;
CREATE INDEX file_versions_content_idx ON file_versions USING GIN (content_vector);
