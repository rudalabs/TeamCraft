-- The project groups of the items an audit entry, a notification or a
-- webhook delivery talks about, captured when the row is written. Read
-- access is re-checked against these as well as against the current items,
-- so a text that names an item of a restricted group stays redacted after
-- that item is deleted (fail closed), and a queued delivery is withheld
-- when one of its groups is restricted before it is sent.
ALTER TABLE audit_log ADD COLUMN mentioned_projects UUID[] NOT NULL DEFAULT '{}';
ALTER TABLE notifications ADD COLUMN mentioned_projects UUID[] NOT NULL DEFAULT '{}';
ALTER TABLE webhook_deliveries ADD COLUMN projects UUID[] NOT NULL DEFAULT '{}';

-- 0025 took the items of older change and request entries from the current
-- membership of the change or request; an item removed since is named in
-- the entry's details (labels "NUMBER/REV", skipped "NUMBER: reason") but
-- was not marked. Mark every item those lists name.
UPDATE audit_log a
SET mentioned_items = ARRAY(
    SELECT DISTINCT x FROM unnest(a.mentioned_items || ARRAY(
        SELECT i.id
        FROM jsonb_each(a.details) AS d (key, value)
        CROSS JOIN LATERAL jsonb_array_elements(
            CASE WHEN jsonb_typeof(d.value) = 'array' THEN d.value ELSE '[]'::jsonb END) AS e (v)
        JOIN items i
          ON i.number = regexp_replace(split_part(e.v #>> '{}', ': ', 1), '/[^/]*$', '')
        WHERE d.key IN ('revisions', 'added', 'skipped', 'items')
          AND jsonb_typeof(e.v) = 'string')) AS x)
WHERE a.entity_type IN ('change', 'change_request') AND jsonb_typeof(a.details) = 'object';

-- Rows written before: the groups of the items that still exist.
UPDATE audit_log a
SET mentioned_projects = ARRAY(SELECT DISTINCT i.project_id FROM items i
                               WHERE i.project_id IS NOT NULL
                                 AND (i.id = a.related_item_id OR i.id = ANY(a.mentioned_items)))
WHERE a.related_item_id IS NOT NULL OR cardinality(a.mentioned_items) > 0;

UPDATE notifications n
SET mentioned_projects = ARRAY(SELECT DISTINCT i.project_id FROM items i
                               WHERE i.project_id IS NOT NULL
                                 AND (i.id = ANY(n.mentioned_items)
                                      OR (n.subject_kind = 'item' AND i.id = n.subject_id)))
WHERE cardinality(n.mentioned_items) > 0 OR n.subject_kind = 'item';
