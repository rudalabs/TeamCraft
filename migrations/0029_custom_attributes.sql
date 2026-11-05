-- Custom attributes (feature 5): definitions maintained by administrators,
-- values per revision, and a classification per item.

CREATE TABLE attribute_defs (
    id          UUID PRIMARY KEY,
    -- Stable name used in the API and in revisions.attributes.
    key         TEXT        NOT NULL UNIQUE CHECK (key ~ '^[a-z][a-z0-9_]{0,39}$'),
    label       TEXT        NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    data_type   TEXT        NOT NULL CHECK (data_type IN ('text', 'number', 'boolean', 'choice')),
    unit        TEXT        NOT NULL DEFAULT '' CHECK (length(unit) <= 16),
    -- Allowed values of a 'choice' attribute.
    choices     TEXT[]      NOT NULL DEFAULT '{}',
    -- Item types the attribute applies to; empty = all.
    item_types  TEXT[]      NOT NULL DEFAULT '{}',
    position    INTEGER     NOT NULL DEFAULT 0,
    -- Inactive attributes are no longer offered; stored values stay visible.
    active      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_by  UUID        REFERENCES users (id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Values per revision: {"vendor": "Bosch", "length_mm": 120, "rohs": true}.
ALTER TABLE revisions ADD COLUMN attributes JSONB NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(attributes) = 'object');
CREATE INDEX revisions_attributes_idx ON revisions USING GIN (attributes jsonb_path_ops);

-- Make, buy or standard part – master data of the item.
ALTER TABLE items ADD COLUMN classification TEXT NOT NULL DEFAULT 'make'
    CHECK (classification IN ('make', 'purchased', 'standard'));
CREATE INDEX items_classification_idx ON items (classification) WHERE classification <> 'make';

-- The full-text search also finds attribute values (weight B, like the material).
DROP INDEX revisions_search_idx;
ALTER TABLE revisions DROP COLUMN search_vector;
ALTER TABLE revisions ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('simple', coalesce(name, '')), 'A') ||
    setweight(to_tsvector('simple', coalesce(material, '')), 'B') ||
    setweight(to_tsvector('simple', jsonb_path_query_array(attributes, '$.*')::text), 'B') ||
    setweight(to_tsvector('simple', coalesce(description, '')), 'C')
) STORED;
CREATE INDEX revisions_search_idx ON revisions USING GIN (search_vector);
