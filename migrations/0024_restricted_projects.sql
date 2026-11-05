-- Read access control for project groups: in a restricted group only its
-- members (of any role) and administrators can read anything.
ALTER TABLE projects
    ADD COLUMN visibility TEXT NOT NULL DEFAULT 'internal'
        CHECK (visibility IN ('internal', 'restricted'));

-- Notifications about restricted subjects are not posted as text to shared chat channels.
ALTER TABLE notifications
    ADD COLUMN confidential BOOLEAN NOT NULL DEFAULT false;

-- Second item an audit entry talks about (BOM child, reference target, basis item, ...),
-- so the entry can be redacted at read time when that item is hidden.
-- No foreign key: audit rows are append-only and must not be touched when an item is deleted.
ALTER TABLE audit_log ADD COLUMN related_item_id UUID;
