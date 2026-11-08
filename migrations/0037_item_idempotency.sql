-- Idempotency keys of item creation (POST /api/v1/items): a client that lost
-- the answer repeats the request with the same key and gets the original item
-- instead of a second one. The row is written in the transaction that creates
-- the item, so the key and the item exist together or not at all.
CREATE TABLE item_idempotency (
    user_id      UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    key          TEXT        NOT NULL CHECK (char_length(key) BETWEEN 1 AND 255),
    -- SHA-256 of the request's fields: the same key with another payload is a conflict
    payload_hash TEXT        NOT NULL,
    revision_id  UUID        REFERENCES revisions (id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, key)
);
