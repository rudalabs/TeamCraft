-- Chunked, resumable uploads of large files (services/uploads.rs). A session
-- reserves an upload of `total_bytes` into a revision; the chunks are
-- appended to <vault>/uploads/<id> and `received_bytes` is the offset the
-- next chunk must start at. When the content is verified and moved into the
-- vault, `stored_sha256` is set; the row is removed when a file version was
-- created from it, when the client aborts, or when it expired.
CREATE TABLE upload_sessions (
    id             UUID PRIMARY KEY,
    user_id        UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    revision_id    UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    filename       TEXT        NOT NULL,
    content_type   TEXT        NOT NULL,
    total_bytes    BIGINT      NOT NULL CHECK (total_bytes >= 0),
    received_bytes BIGINT      NOT NULL DEFAULT 0
                               CHECK (received_bytes >= 0 AND received_bytes <= total_bytes),
    -- SHA-256 the client announced (optional); checked when the upload completes.
    sha256         TEXT        CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    -- The server version the upload is based on (optimistic concurrency):
    -- a version number (0 = new file) or the SHA-256 the client started from.
    base_version   INTEGER     CHECK (base_version >= 0),
    base_sha256    TEXT        CHECK (base_sha256 ~ '^[0-9a-f]{64}$'),
    stored_sha256  TEXT        CHECK (stored_sha256 ~ '^[0-9a-f]{64}$'),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at     TIMESTAMPTZ NOT NULL,
    CHECK (base_version IS NULL OR base_sha256 IS NULL)
);
CREATE INDEX upload_sessions_user_idx ON upload_sessions (user_id);
CREATE INDEX upload_sessions_expires_idx ON upload_sessions (expires_at);
