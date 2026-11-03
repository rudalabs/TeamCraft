-- Roles: "engineer" is called "CAD editor" now (stored as 'editor').
ALTER TABLE users DROP CONSTRAINT users_role_check;
UPDATE users SET role = 'editor' WHERE role = 'engineer';
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN ('viewer', 'editor', 'admin'));

-- Project groups: CAD editors may only modify items of the projects they are
-- members of. Memberships are maintained by admins ('manual') or synchronised
-- from the IdP group named in projects.oidc_group at every SSO login ('oidc').
ALTER TABLE projects ADD COLUMN oidc_group TEXT;

CREATE TABLE project_members (
    project_id  UUID        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    user_id     UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source      TEXT        NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'oidc')),
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX project_members_user_idx ON project_members (user_id);
