-- Self-service project groups: CAD editors create their own groups; the
-- creator (and every member marked as manager) maintains the group.
ALTER TABLE projects ADD COLUMN created_by UUID REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE project_members ADD COLUMN manager BOOLEAN NOT NULL DEFAULT false;

-- The default skin is "teamcraft".
ALTER TABLE users ALTER COLUMN skin SET DEFAULT 'teamcraft';
