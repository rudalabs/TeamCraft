-- File versions: a file of a revision keeps every uploaded version. The
-- columns of `files` (sha256, size_bytes, content_type, uploaded_*) describe
-- the current version; `file_versions` holds the complete history.
CREATE TABLE file_versions (
    id            UUID PRIMARY KEY,
    file_id       UUID        NOT NULL REFERENCES files (id) ON DELETE CASCADE,
    version       INTEGER     NOT NULL CHECK (version > 0),
    sha256        TEXT        NOT NULL,
    size_bytes    BIGINT      NOT NULL CHECK (size_bytes >= 0),
    content_type  TEXT        NOT NULL,
    comment       TEXT        NOT NULL DEFAULT '',
    uploaded_by   UUID        NOT NULL REFERENCES users (id),
    uploaded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (file_id, version)
);
CREATE INDEX file_versions_sha256_idx ON file_versions (sha256);

ALTER TABLE files ADD COLUMN current_version INTEGER NOT NULL DEFAULT 1;

-- Existing files become version 1.
INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, uploaded_by, uploaded_at)
SELECT gen_random_uuid(), id, 1, sha256, size_bytes, content_type, uploaded_by, uploaded_at FROM files;

CREATE INDEX files_revision_name_idx ON files (revision_id, lower(filename));
