-- Data of the published TeamCraft v0.1.0 (tag v0.1.0 at 439724a) beyond the
-- first v0.1.0 tag: what migrations 0021 and 0023–0031 added. Loaded after
-- v0.1.0-seed.sql into a database at exactly that schema (see
-- tests/common/releases.rs); together they fill every table and every value
-- of every TEXT enum column (CHECK) of the published v0.1.0, which the
-- upgrade test checks. New columns of existing tables get non-default values
-- on the rows of the first seed. Invented data only, no organisation names.

-- 0021: a shape signature of the bracket's current file content
INSERT INTO shape_signatures (sha256, version, extent_1, extent_2, extent_3, ratio_2, ratio_3, area, volume, closed, triangles, d2) VALUES
  ('a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 1, 40, 20, 5, 0.5, 0.125, 2050.5, 3999.75, TRUE, 12,
   ARRAY[0.05, 0.1, 0.2, 0.3, 0.2, 0.1, 0.05]::REAL[]);

-- 0024: a restricted project group (both visibilities), a confidential
-- notification, an audit entry with its related item
UPDATE projects SET visibility = 'restricted' WHERE id = 'bbbbbbbb-0000-4000-8000-000000000002';
UPDATE notifications SET confidential = TRUE WHERE id = '44444444-0000-4000-8000-000000000003';
UPDATE audit_log SET related_item_id = 'cccccccc-0000-4000-8000-000000000001' WHERE action = 'item.create';

-- 0025, 0026: what audit entries, notifications and webhook deliveries name
UPDATE audit_log
SET mentioned_items = ARRAY['cccccccc-0000-4000-8000-000000000001'::uuid],
    mentioned_changes = ARRAY['11111111-0000-4000-8000-000000000001'::uuid],
    mentioned_projects = ARRAY['bbbbbbbb-0000-4000-8000-000000000001'::uuid]
WHERE action = 'change.release';
UPDATE webhook_deliveries SET projects = ARRAY['bbbbbbbb-0000-4000-8000-000000000001'::uuid]
WHERE id = '77777777-0000-4000-8000-000000000001';

-- 0025, 0027: the subject of a notification, every kind
UPDATE notifications
SET subject_kind = 'change', subject_id = '11111111-0000-4000-8000-000000000002',
    mentioned_changes = ARRAY['11111111-0000-4000-8000-000000000002'::uuid]
WHERE id = '44444444-0000-4000-8000-000000000001';
UPDATE notifications
SET subject_kind = 'item', subject_id = 'cccccccc-0000-4000-8000-000000000002',
    mentioned_items = ARRAY['cccccccc-0000-4000-8000-000000000002'::uuid],
    mentioned_projects = ARRAY['bbbbbbbb-0000-4000-8000-000000000001'::uuid]
WHERE id = '44444444-0000-4000-8000-000000000003';
UPDATE notifications
SET subject_kind = 'request', subject_id = '22222222-0000-4000-8000-000000000001'
WHERE id = '44444444-0000-4000-8000-000000000006';

-- 0029: custom attributes of every data type, values on a revision, every
-- classification
INSERT INTO attribute_defs (id, key, label, data_type, unit, choices, item_types, position, active, created_by) VALUES
  ('12121212-0000-4000-8000-000000000001', 'vendor', 'Vendor', 'text', '', '{}', '{part}', 1, TRUE, 'aaaaaaaa-0000-4000-8000-000000000001'),
  ('12121212-0000-4000-8000-000000000002', 'order_qty', 'Order quantity', 'number', 'pcs', '{}', '{part,assembly}', 2, TRUE, 'aaaaaaaa-0000-4000-8000-000000000001'),
  ('12121212-0000-4000-8000-000000000003', 'rohs', 'RoHS', 'boolean', '', '{}', '{}', 3, TRUE, 'aaaaaaaa-0000-4000-8000-000000000001'),
  ('12121212-0000-4000-8000-000000000004', 'finish', 'Finish', 'choice', '', '{anodised,painted,raw}', '{part}', 4, FALSE, NULL);
UPDATE revisions
SET attributes = '{"vendor": "Bolt Works", "order_qty": 500, "rohs": true, "finish": "raw"}'::jsonb
WHERE id = 'dddddddd-0000-4000-8000-000000000007';
UPDATE items SET classification = 'standard' WHERE id = 'cccccccc-0000-4000-8000-000000000006';
UPDATE items SET classification = 'purchased' WHERE id = 'cccccccc-0000-4000-8000-000000000004';

-- 0030: an alternate and a supersession
INSERT INTO item_alternates (item_id, alternate_item_id, note, created_by) VALUES
  ('cccccccc-0000-4000-8000-000000000006', 'cccccccc-0000-4000-8000-000000000001', 'either fits', 'aaaaaaaa-0000-4000-8000-000000000002');
INSERT INTO item_supersessions (item_id, successor_item_id, note, created_by) VALUES
  ('cccccccc-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000004', 'procedure moved into the software', 'aaaaaaaa-0000-4000-8000-000000000002');
