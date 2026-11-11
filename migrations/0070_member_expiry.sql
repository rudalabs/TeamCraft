-- Season handover for student teams (issue #173, step 8): "member until",
-- e.g. the end of term. Only flags the member on the season page once the
-- date has passed; nothing removes a member automatically.
ALTER TABLE project_members ADD COLUMN expires_at DATE;
