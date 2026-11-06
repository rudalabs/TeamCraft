-- E-mail digest of the change requests that wait for triage, per project
-- group (src/services/request_digests.rs).

-- The digest is a notification kind of its own: users mute it on their
-- account page like every other kind.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('review_requested', 'change_update', 'impact', 'item_update', 'unlock',
                    'change_request', 'request_digest', 'test'));

-- Up to when the digests of a user for a project group covered the new
-- requests. The row is updated in the transaction that queues the digest,
-- so a restart (or a second instance) never sends a period twice.
CREATE TABLE request_digests (
    user_id       UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id    UUID        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    covered_until TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, project_id)
);
