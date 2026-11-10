-- Two optional rules of a project group's review policy (#152 step 9), both
-- off by default so existing groups behave as before.
--
-- requester_cannot_decide: the requester of a change request on items of the
-- group does not accept, reject or defer it.
-- concessions_need_review: a "use as is" deviation of an as-built revision is
-- confirmed on its own by a reviewer of the change order that releases it
-- (not the person who recorded it) before that reviewer approves.
ALTER TABLE project_policies ADD COLUMN requester_cannot_decide BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE project_policies ADD COLUMN concessions_need_review BOOLEAN NOT NULL DEFAULT false;

-- The confirmation of a "use as is" deviation; cleared whenever the
-- deviation changes.
ALTER TABLE as_built_deviations ADD COLUMN concession_confirmed_by UUID REFERENCES users (id);
ALTER TABLE as_built_deviations ADD COLUMN concession_confirmed_at TIMESTAMPTZ;
