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

-- The WARR project groups. Existing projects with the same code are kept.
INSERT INTO projects (id, code, name, description, oidc_group) VALUES
    (gen_random_uuid(), 'NIXUS',      'WARR Rocketry Project NIXUS',        'Rocketry',        'teamcraft-nixus'),
    (gen_random_uuid(), 'WESP',       'WARR Rocketry Project WESP',         'Rocketry',        'teamcraft-wesp'),
    (gen_random_uuid(), 'RETROFIRE',  'WARR Rocketry Project Retrofire',    'Rocketry',        'teamcraft-retrofire'),
    (gen_random_uuid(), 'MOVE-III',   'WARR MOVE III',                      'MOVE',            'teamcraft-move-iii'),
    (gen_random_uuid(), 'MOVE-REXUS', 'WARR MOVE Rexus',                    'MOVE',            'teamcraft-move-rexus'),
    (gen_random_uuid(), 'MOVE-BEXUS', 'WARR MOVE Bexus',                    'MOVE',            'teamcraft-move-bexus'),
    (gen_random_uuid(), 'CRATER',     'WARR Space Robotics Project CRATER', 'Space Robotics',  'teamcraft-crater'),
    (gen_random_uuid(), 'GRAKSLER',   'WARR Space Robotics Project GRAKSLER','Space Robotics', 'teamcraft-graksler'),
    (gen_random_uuid(), 'SPACE-LABS', 'WARR Space Labs',                    'Space Labs',      'teamcraft-space-labs'),
    (gen_random_uuid(), 'GENERAL',    'WARR General',                       'General',         'teamcraft-general'),
    (gen_random_uuid(), 'PR',         'WARR Public Relations',              'Public Relations','teamcraft-pr'),
    (gen_random_uuid(), 'WORKSHOP',   'WARR Workshop',                      'Workshop',        'teamcraft-workshop')
ON CONFLICT (code) DO NOTHING;
