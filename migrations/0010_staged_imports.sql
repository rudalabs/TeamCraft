-- Uploaded files waiting for the user to confirm an import preview: BOM
-- tables from CAD programs (content kept here, small) and STEP assemblies
-- (stored in the vault, referenced by SHA-256). A row is removed when its
-- import ran, when it is discarded, or after a day.
CREATE TABLE staged_imports (
    id          UUID PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind        TEXT        NOT NULL CHECK (kind IN ('bom_table', 'step')),
    filename    TEXT        NOT NULL,
    size_bytes  BIGINT      NOT NULL CHECK (size_bytes >= 0),
    content     BYTEA,
    sha256      TEXT,
    options     JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((content IS NULL) <> (sha256 IS NULL))
);
CREATE INDEX staged_imports_user_idx ON staged_imports (user_id, created_at);
CREATE INDEX staged_imports_created_idx ON staged_imports (created_at);
