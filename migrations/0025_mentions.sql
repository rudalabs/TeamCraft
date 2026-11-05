-- Audit entries and notifications whose text names items or change orders
-- besides their subject: the reader must be allowed to see all of them, so
-- the text is redacted (audit) or hidden (notifications) at read time, also
-- after a group was restricted or the reader lost a membership.
-- No foreign keys: audit rows are append-only and outlive deleted objects.
ALTER TABLE audit_log
    ADD COLUMN mentioned_items UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN mentioned_changes UUID[] NOT NULL DEFAULT '{}';

-- Entries written before: impact flags name their dependents; change entries
-- name the revisions of the change; request entries name the linked change.
UPDATE audit_log
SET mentioned_items = ARRAY(SELECT jsonb_array_elements_text(details -> 'dependents')::uuid)
WHERE action = 'impact.flag' AND jsonb_typeof(details -> 'dependents') = 'array';

UPDATE audit_log a
SET mentioned_items = ARRAY(SELECT r.item_id FROM change_items ci
                            JOIN revisions r ON r.id = ci.revision_id
                            WHERE ci.change_id = a.entity_id)
WHERE a.entity_type = 'change'
  AND a.action IN ('change.submit', 'change.rejected', 'change.released', 'change.withdraw');

UPDATE audit_log
SET mentioned_changes = ARRAY[(details ->> 'change_id')::uuid]
WHERE entity_type = 'change_request'
  AND action IN ('change_request.create_change', 'change_request.link_change',
                 'change_request.implemented')
  AND details ->> 'change_id' ~ '^[0-9a-fA-F-]{36}$';

UPDATE audit_log a
SET mentioned_items = ARRAY(SELECT cri.item_id FROM change_request_items cri
                            WHERE cri.request_id = a.entity_id)
WHERE a.entity_type = 'change_request' AND a.action = 'change_request.create';

-- What a notification is about and what else its text names, so the list
-- re-checks at read time (and before e-mail or chat delivery) whether the
-- recipient may still see it. NULL for rows written before.
ALTER TABLE notifications
    ADD COLUMN subject_kind TEXT CHECK (subject_kind IN ('item', 'change', 'request')),
    ADD COLUMN subject_id UUID,
    ADD COLUMN mentioned_items UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN mentioned_changes UUID[] NOT NULL DEFAULT '{}';
