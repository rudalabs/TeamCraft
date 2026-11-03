-- TeamCraft is generic open-source software.

-- 1. Migration 0003 seeded the project groups of its first installation.
--    Remove them unless they are in use, so new installations start empty.
DELETE FROM projects p
WHERE p.code IN ('NIXUS', 'WESP', 'RETROFIRE', 'MOVE-III', 'MOVE-REXUS', 'MOVE-BEXUS',
                 'CRATER', 'GRAKSLER', 'SPACE-LABS', 'GENERAL', 'PR', 'WORKSHOP')
  AND p.name LIKE 'WARR %'
  AND NOT EXISTS (SELECT 1 FROM items i WHERE i.project_id = p.id);

-- 2. Self-service project groups: CAD editors create their own groups; the
--    creator (and every member marked as manager) maintains the group.
ALTER TABLE projects ADD COLUMN created_by UUID REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE project_members ADD COLUMN manager BOOLEAN NOT NULL DEFAULT false;

-- 3. The house skins are called "teamcraft" and "teamcraft-day".
ALTER TABLE users ALTER COLUMN skin SET DEFAULT 'teamcraft';
UPDATE users SET skin = 'teamcraft' WHERE skin = 'warr';
UPDATE users SET skin = 'teamcraft-day' WHERE skin = 'warr-day';
