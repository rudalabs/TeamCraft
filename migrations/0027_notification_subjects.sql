-- Notifications written before 0025 have no subject_kind/subject_id, so the
-- read-time check let them through for good, also after their subject's
-- group was restricted. Their link and subject_ref name the subject: take
-- it from there. (Rows whose subject is gone stay as they are: they were
-- written before any group could be restricted, and a deleted item cannot
-- be restricted any more.) The CASE keeps the cast away from other links.
UPDATE notifications n
SET subject_kind = 'item', subject_id = r.item_id
FROM revisions r
WHERE n.subject_kind IS NULL AND n.subject_type = 'revision'
  AND r.id = CASE WHEN n.link ~ '^/revisions/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                  THEN substr(n.link, 12)::uuid END;

UPDATE notifications n
SET subject_kind = 'item', subject_id = i.id
FROM items i
WHERE n.subject_kind IS NULL AND n.subject_type = 'revision'
  AND i.number = regexp_replace(n.subject_ref, '/[^/]*$', '');

UPDATE notifications n
SET subject_kind = 'change', subject_id = c.id
FROM changes c
WHERE n.subject_kind IS NULL AND n.subject_type = 'change'
  AND (c.number = n.subject_ref
       OR c.id = CASE WHEN n.link ~ '^/changes/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                      THEN substr(n.link, 10)::uuid END);

UPDATE notifications n
SET subject_kind = 'request', subject_id = q.id
FROM change_requests q
WHERE n.subject_kind IS NULL AND n.subject_type = 'change_request'
  AND (q.number = n.subject_ref
       OR q.id = CASE WHEN n.link ~ '^/requests/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                      THEN substr(n.link, 11)::uuid END);

-- The group of an item subject, as 0026 records it for newer rows.
UPDATE notifications n
SET mentioned_projects = n.mentioned_projects || i.project_id
FROM items i
WHERE n.subject_kind = 'item' AND i.id = n.subject_id AND i.project_id IS NOT NULL
  AND NOT (i.project_id = ANY(n.mentioned_projects));
