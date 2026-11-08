-- Need-to-know groups, step 5 (issue #151): the periodic access review of a
-- restricted project group. A manager (or administrator) confirms that the
-- member list and the administrator policy were checked; the row keeps the
-- date, who confirmed, an optional note and the member list and policy as they
-- were at that moment, so an auditor can see what was reviewed. Rows are
-- never changed; the username stays when the account is deleted.
CREATE TABLE project_access_reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    reviewed_by UUID REFERENCES users(id) ON DELETE SET NULL,
    username TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '' CHECK (char_length(note) <= 1000),
    reviewed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    admin_read TEXT NOT NULL CHECK (admin_read IN ('implicit', 'with_reason', 'none')),
    member_count INTEGER NOT NULL,
    snapshot JSONB NOT NULL
);
CREATE INDEX project_access_reviews_project ON project_access_reviews (project_id, reviewed_at DESC);
