-- Separation of duties for changes that are already in review.
--
-- `four_eyes` says whether the editor and preparer checks apply when a
-- reviewer decides. Changes that were in review before this rule existed keep
-- working (false); they could otherwise be blocked for good, because only a
-- withdraw and resubmit would replace a reviewer. Every (re)submit sets it.
ALTER TABLE changes ADD COLUMN four_eyes BOOLEAN NOT NULL DEFAULT true;
UPDATE changes SET four_eyes = false WHERE status = 'in_review';

-- Migration 0040 recorded the author as submitter. Where the audit log knows
-- who really pressed submit, use that user.
UPDATE changes c SET submitted_by = s.user_id
FROM (
    SELECT DISTINCT ON (entity_id) entity_id, user_id
    FROM audit_log
    WHERE entity_type = 'change' AND action = 'change.submit' AND user_id IS NOT NULL
    ORDER BY entity_id, at DESC
) s
WHERE s.entity_id = c.id AND c.submitted_at IS NOT NULL;
