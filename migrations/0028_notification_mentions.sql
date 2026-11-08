-- Notifications written before 0025 got their subject in 0027, but not the
-- other objects their text names: an impact notice is about the reader's
-- dependent and names the released source ("Check needed: ASM-2 – PRT-5/B
-- was released"), a release notice names the change order. The read check
-- tests the subject and the mention arrays, so such a text stayed visible
-- to readers who may not see what it names. Take the item and change
-- numbers from the title and body, as they were written (fail closed: a
-- word that happens to be a number only hides the row from readers who may
-- not see that item or change).
WITH words AS (
    SELECT n.id, w.word
    FROM notifications n
    CROSS JOIN LATERAL regexp_matches(n.title || ' ' || n.body,
                                      '[A-Za-z0-9][A-Za-z0-9._-]*', 'g') AS m (parts)
    CROSS JOIN LATERAL (VALUES (m.parts[1]), (rtrim(m.parts[1], '._-'))) AS w (word)
    WHERE cardinality(n.mentioned_items) = 0 AND cardinality(n.mentioned_changes) = 0
),
named_items AS (
    SELECT w.id, array_agg(DISTINCT i.id) AS items
    FROM words w JOIN items i ON i.number = w.word
    GROUP BY w.id
),
named_changes AS (
    SELECT w.id, array_agg(DISTINCT c.id) AS changes
    FROM words w JOIN changes c ON c.number = w.word
    GROUP BY w.id
)
UPDATE notifications n
SET mentioned_items = COALESCE(ni.items, '{}'),
    mentioned_changes = COALESCE(nc.changes, '{}')
FROM (SELECT id FROM named_items UNION SELECT id FROM named_changes) AS x
LEFT JOIN named_items ni ON ni.id = x.id
LEFT JOIN named_changes nc ON nc.id = x.id
WHERE n.id = x.id;

-- The groups of the items named now, as 0026 records them for newer rows.
UPDATE notifications n
SET mentioned_projects = ARRAY(
    SELECT DISTINCT p FROM unnest(n.mentioned_projects || ARRAY(
        SELECT i.project_id FROM items i
        WHERE i.id = ANY(n.mentioned_items) AND i.project_id IS NOT NULL)) AS p)
WHERE cardinality(n.mentioned_items) > 0;
