-- Need-to-know project groups, step 1 (issue #151): how administrators read a
-- restricted group. Without a row (or with 'implicit') nothing changes:
-- administrators read every restricted group.
--   with_reason  an administrator who is not a member sees the group as hidden
--                until they open a time-limited elevation with a written reason
--   none         administrators manage the group but read nothing in it unless
--                they are members
CREATE TABLE project_policies (
    project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    admin_read TEXT NOT NULL DEFAULT 'implicit'
        CHECK (admin_read IN ('implicit', 'with_reason', 'none')),
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Time-limited admin access ("break glass"). Rows are never deleted, so the
-- review report can list every elevation; `ended_at` closes one early.
CREATE TABLE project_elevations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 1000),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ
);
CREATE INDEX project_elevations_active ON project_elevations (user_id, project_id, expires_at);
CREATE INDEX project_elevations_project ON project_elevations (project_id, started_at DESC);

-- Notices to the group managers about elevations and admin self-adds.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('review_requested', 'change_update', 'impact', 'item_update', 'unlock',
                    'change_request', 'request_digest', 'access_alert', 'test'));
