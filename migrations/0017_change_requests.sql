-- Engineering change requests (ECR): the step before a change order (ECO).
-- Anybody who finds a problem raises a request (shop floor, simulation,
-- test, purchasing); CAD editors of the affected project groups triage and
-- decide. An accepted request is implemented by one or more change orders;
-- when one of them is released the request becomes 'implemented'.
CREATE TABLE change_requests (
    id                UUID PRIMARY KEY,
    number            TEXT        NOT NULL UNIQUE,
    title             TEXT        NOT NULL,
    -- problem and proposed solution
    description       TEXT        NOT NULL DEFAULT '',
    reason            TEXT        NOT NULL DEFAULT 'other'
                      CHECK (reason IN ('defect', 'improvement', 'cost', 'manufacturing', 'supplier',
                                        'safety', 'customer', 'other')),
    priority          TEXT        NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'critical')),
    status            TEXT        NOT NULL DEFAULT 'new'
                      CHECK (status IN ('new', 'in_triage', 'accepted', 'rejected', 'deferred',
                                        'implemented', 'withdrawn')),
    requested_by      UUID        NOT NULL REFERENCES users (id),
    assignee          UUID        REFERENCES users (id),
    decision_comment  TEXT        NOT NULL DEFAULT '',
    decided_by        UUID        REFERENCES users (id),
    decided_at        TIMESTAMPTZ,
    due_date          DATE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at         TIMESTAMPTZ
);
CREATE INDEX change_requests_status_idx ON change_requests (status);
CREATE INDEX change_requests_requested_by_idx ON change_requests (requested_by);
CREATE INDEX change_requests_assignee_idx ON change_requests (assignee) WHERE assignee IS NOT NULL;

-- The affected items, optionally with the revision the problem was found in.
CREATE TABLE change_request_items (
    request_id   UUID        NOT NULL REFERENCES change_requests (id) ON DELETE CASCADE,
    item_id      UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    revision_id  UUID        REFERENCES revisions (id) ON DELETE SET NULL,
    note         TEXT        NOT NULL DEFAULT '',
    added_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, item_id)
);
CREATE INDEX change_request_items_item_idx ON change_request_items (item_id);

-- Discussion thread.
CREATE TABLE change_request_comments (
    id          UUID PRIMARY KEY,
    request_id  UUID        NOT NULL REFERENCES change_requests (id) ON DELETE CASCADE,
    author_id   UUID        NOT NULL REFERENCES users (id),
    body        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX change_request_comments_request_idx ON change_request_comments (request_id, created_at);

-- Screenshots, mark-ups and reports (images and PDF only). The bytes live in
-- the content-addressed vault like every other file.
CREATE TABLE change_request_attachments (
    id            UUID PRIMARY KEY,
    request_id    UUID        NOT NULL REFERENCES change_requests (id) ON DELETE CASCADE,
    filename      TEXT        NOT NULL,
    content_type  TEXT        NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg', 'image/gif',
                                                               'image/webp', 'application/pdf')),
    size_bytes    BIGINT      NOT NULL CHECK (size_bytes >= 0),
    sha256        TEXT        NOT NULL,
    uploaded_by   UUID        NOT NULL REFERENCES users (id),
    uploaded_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX change_request_attachments_request_idx ON change_request_attachments (request_id);

-- Change orders that implement a request.
CREATE TABLE change_request_changes (
    request_id  UUID        NOT NULL REFERENCES change_requests (id) ON DELETE CASCADE,
    change_id   UUID        NOT NULL REFERENCES changes (id) ON DELETE CASCADE,
    linked_by   UUID        NOT NULL REFERENCES users (id),
    linked_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, change_id)
);
CREATE INDEX change_request_changes_change_idx ON change_request_changes (change_id);

-- Notifications and watch lists know change requests.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('review_requested', 'change_update', 'impact', 'item_update', 'unlock',
                    'change_request', 'test'));
ALTER TABLE notifications DROP CONSTRAINT notifications_subject_type_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_subject_type_check
    CHECK (subject_type IN ('', 'change', 'revision', 'change_request'));
ALTER TABLE watches DROP CONSTRAINT watches_entity_type_check;
ALTER TABLE watches ADD CONSTRAINT watches_entity_type_check
    CHECK (entity_type IN ('item', 'change', 'change_request'));
