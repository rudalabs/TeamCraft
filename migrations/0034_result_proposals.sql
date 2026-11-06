-- Result values proposed by the assistant from a report (PDF or text file) of
-- a simulation revision. A proposal is a draft: it becomes a result value only
-- when a CAD editor accepts it while the revision is checked out to them.
-- It keeps the file version and the quoted text it was read from.

CREATE TABLE result_proposals (
    id              UUID PRIMARY KEY,
    revision_id     UUID             NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    name            TEXT             NOT NULL CHECK (name <> ''),
    value           DOUBLE PRECISION NOT NULL,
    unit            TEXT             NOT NULL DEFAULT '',
    lower_limit     DOUBLE PRECISION,
    upper_limit     DOUBLE PRECISION,
    load_case       TEXT             NOT NULL DEFAULT '',
    solver          TEXT             NOT NULL DEFAULT '',
    solver_version  TEXT             NOT NULL DEFAULT '',
    source_file     TEXT             NOT NULL CHECK (source_file <> ''),
    source_version  INTEGER          NOT NULL,
    quote           TEXT             NOT NULL CHECK (quote <> ''),
    status          TEXT             NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'accepted', 'rejected')),
    proposed_by     UUID             NOT NULL REFERENCES users (id),
    created_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    decided_by      UUID             REFERENCES users (id),
    decided_at      TIMESTAMPTZ,
    CHECK ((status = 'pending') = (decided_at IS NULL)),
    CHECK (lower_limit IS NULL OR upper_limit IS NULL OR lower_limit <= upper_limit)
);
-- one pending proposal per value; a newer one replaces it
CREATE UNIQUE INDEX result_proposals_pending ON result_proposals
    (revision_id, lower(name), lower(load_case)) WHERE status = 'pending';
CREATE INDEX result_proposals_revision ON result_proposals (revision_id, status);
