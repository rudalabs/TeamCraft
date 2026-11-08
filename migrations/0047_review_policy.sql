-- Review policy per project group (issue #152, step 2). Without a value
-- nothing changes: every reviewer of a change must approve.
--   review_min_approvals    NULL = all reviewers; otherwise this many approvals
--                           release the change (a rejection still rejects it)
--   review_role_project_id  at least one approval must come from a member of
--                           this other project group (a role group, e.g. QA)
--   project_required_reviewers  people who must be reviewers and approve
ALTER TABLE project_policies
    ADD COLUMN review_min_approvals INT
        CHECK (review_min_approvals IS NULL OR review_min_approvals BETWEEN 1 AND 50),
    ADD COLUMN review_role_project_id UUID REFERENCES projects(id) ON DELETE SET NULL;

CREATE TABLE project_required_reviewers (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX project_required_reviewers_user ON project_required_reviewers (user_id);
