-- Who submitted a change order for review. Normally the author; an
-- administrator who acts for the author is recorded here so that they cannot
-- also review the change (separation of duties).
ALTER TABLE changes ADD COLUMN submitted_by UUID REFERENCES users (id) ON DELETE SET NULL;
UPDATE changes SET submitted_by = created_by WHERE submitted_at IS NOT NULL;
