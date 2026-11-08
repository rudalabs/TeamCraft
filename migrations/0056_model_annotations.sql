-- 3D markup: notes reviewers pin to a point of a file version's 3D preview
-- (model coordinates). Removed with the version.
CREATE TABLE model_annotations (
    id              UUID PRIMARY KEY,
    file_version_id UUID NOT NULL REFERENCES file_versions (id) ON DELETE CASCADE,
    x               DOUBLE PRECISION NOT NULL,
    y               DOUBLE PRECISION NOT NULL,
    z               DOUBLE PRECISION NOT NULL,
    body            TEXT NOT NULL CHECK (length(body) BETWEEN 1 AND 2000),
    author_id       UUID NOT NULL REFERENCES users (id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX model_annotations_version_idx ON model_annotations (file_version_id, created_at);
