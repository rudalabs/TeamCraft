-- Need-to-know groups: the elevation history outlives the administrator's
-- account. Deleting a user used to delete their elevations (ON DELETE CASCADE),
-- which removed the break-glass evidence; now the row stays, the user id is
-- cleared and the username written at elevation time remains. (Deleting the
-- group itself still deletes its history; audit_log is the durable record.)
ALTER TABLE project_elevations ADD COLUMN username TEXT NOT NULL DEFAULT '';
UPDATE project_elevations e SET username = u.username FROM users u WHERE u.id = e.user_id;
ALTER TABLE project_elevations ALTER COLUMN username DROP DEFAULT;
ALTER TABLE project_elevations ALTER COLUMN user_id DROP NOT NULL;
ALTER TABLE project_elevations DROP CONSTRAINT project_elevations_user_id_fkey;
ALTER TABLE project_elevations
    ADD CONSTRAINT project_elevations_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL;
