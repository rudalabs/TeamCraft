-- Synthetic large vault for the scale check (tests/scale.rs). Not a
-- feature: it is run by the ignored test `scale_check` only, against a
-- database of its own (teamcraft_scale), never against a real instance.
--
-- Everything is generated set-based (generate_series) in a few large
-- INSERT … SELECT statements; nothing goes through the services, so the
-- seed takes minutes, not hours. The data is deterministic (no random()
-- except for the UUIDs), so every run measures the same structure:
--
--   * 500 users, 40 project groups (memberships), one busy user
--     `scale.busy` (created by the harness before this script) who is a
--     member of every group, has ~200 check-outs, ~100 pending reviews,
--     ~30k notifications, 5k watched items and is assigned change requests;
--   * 100k items: 30k assemblies (750 per group on 9 levels), 50 standard
--     parts (STD-0001 is used in every assembly revision), ~65k parts,
--     5k documents – with 1–3 revisions each (~200k revisions);
--   * ~1M BOM lines: every assembly revision has 2 sub-assemblies of the
--     next level (ASM-000001 has 10) and 15 parts, so the deepest structure
--     has 10 levels and ASM-000001 expands to ~41k lines;
--   * ~200k files with ~240k file versions, file references, 100k item
--     references (documents → parts);
--   * 20k changes (ECO-000001 has 2000 affected revisions) with affected
--     items and reviewers, 5k change requests;
--   * 2M audit rows, ~500k notifications, 50k impacts, watches, favorites.
--
-- The harness runs the steps (separated by `-- step:` lines) on one
-- connection; temporary tables live until the end of the seed.

-- step: helpers
SET synchronous_commit = off;
CREATE TEMP TABLE seed_busy AS SELECT id FROM users WHERE username = 'scale.busy';
CREATE TEMP TABLE seed_levels (level int PRIMARY KEY, start int NOT NULL, size int NOT NULL);
-- assemblies per project group and level (750 per group)
INSERT INTO seed_levels VALUES
    (1, 0, 5), (2, 5, 10), (3, 15, 20), (4, 35, 40), (5, 75, 60),
    (6, 135, 80), (7, 215, 100), (8, 315, 140), (9, 455, 295);

-- step: users and project groups
INSERT INTO users (id, username, display_name, email, auth_source, password_hash, role, created_at)
SELECT gen_random_uuid(), 'scale.u' || lpad(n::text, 4, '0'), 'Scale User ' || n,
       'scale.u' || n || '@example.invalid', 'local', '!scale-seed-no-login',
       CASE WHEN n % 10 = 0 THEN 'viewer' WHEN n % 50 = 1 THEN 'admin' ELSE 'editor' END,
       now() - interval '400 days'
FROM generate_series(1, 500) n;
CREATE TEMP TABLE seed_users AS
SELECT row_number() OVER (ORDER BY username)::int AS un, id, username
FROM users WHERE username LIKE 'scale.u%';
CREATE UNIQUE INDEX ON seed_users (un);

INSERT INTO projects (id, code, name, description, created_at)
SELECT gen_random_uuid(), 'P' || lpad(n::text, 2, '0'), 'Product line ' || n,
       'Synthetic project group of the scale check', now() - interval '400 days'
FROM generate_series(1, 40) n;
CREATE TEMP TABLE seed_projects AS
SELECT (row_number() OVER (ORDER BY code) - 1)::int AS n, id
FROM projects WHERE code ~ '^P[0-9]{2}$';
CREATE UNIQUE INDEX ON seed_projects (n);

INSERT INTO project_members (project_id, user_id)
SELECT DISTINCT p.id, u.id
FROM seed_users u
CROSS JOIN LATERAL (VALUES (u.un % 40), ((u.un * 7) % 40), ((u.un * 13) % 40)) m(project)
JOIN seed_projects p ON p.n = m.project;
INSERT INTO project_members (project_id, user_id, manager)
SELECT p.id, b.id, true FROM seed_projects p CROSS JOIN seed_busy b;

-- step: items
CREATE TEMP TABLE seed_items (
    g       int PRIMARY KEY,
    id      uuid NOT NULL,
    kind    text NOT NULL,
    number  text NOT NULL,
    project int  NOT NULL,
    idx     int  NOT NULL,
    level   int,
    nrev    int  NOT NULL
);
INSERT INTO seed_items
SELECT g, gen_random_uuid(), 'assembly', 'ASM-' || lpad(g::text, 6, '0'),
       (g - 1) % 40, (g - 1) / 40, l.level, 1 + (g * 7) % 3
FROM generate_series(1, 30000) g
JOIN seed_levels l ON (g - 1) / 40 >= l.start AND (g - 1) / 40 < l.start + l.size;
INSERT INTO seed_items
SELECT g, gen_random_uuid(), 'standard', 'STD-' || lpad((g - 30000)::text, 4, '0'),
       0, g - 30001, NULL, 1 + (g * 7) % 3
FROM generate_series(30001, 30050) g;
INSERT INTO seed_items
SELECT g, gen_random_uuid(), 'part', 'PRT-' || lpad(g::text, 6, '0'),
       (g - 30051) % 40, (g - 30051) / 40, NULL, 1 + (g * 7) % 3
FROM generate_series(30051, 95000) g;
INSERT INTO seed_items
SELECT g, gen_random_uuid(), 'document', 'DOC-' || lpad(g::text, 6, '0'),
       (g - 95001) % 40, (g - 95001) / 40, NULL, 1 + (g * 7) % 3
FROM generate_series(95001, 100000) g;
CREATE INDEX ON seed_items (kind, project, idx);
ANALYZE seed_items;

INSERT INTO items (id, number, item_type, unit, project_id, created_by, created_at)
SELECT s.id, s.number, CASE s.kind WHEN 'standard' THEN 'part' ELSE s.kind END,
       'each', p.id, u.id, now() - interval '300 days' + s.g * interval '1 minute'
FROM seed_items s
JOIN seed_projects p ON p.n = s.project
JOIN seed_users u ON u.un = s.g % 500 + 1;

-- step: revisions
-- Older revisions are released; the latest one is released, in work
-- (checked out) or in review by the item number.
CREATE TEMP TABLE seed_revs AS
SELECT row_number() OVER (ORDER BY s.g, v.seq)::int AS rn, s.g, v.seq, gen_random_uuid() AS id,
       v.seq = s.nrev AS latest,
       CASE WHEN v.seq < s.nrev THEN 'released'
            WHEN s.g % 10 IN (6, 7) THEN 'in_work'
            WHEN s.g % 10 = 8 THEN 'in_review'
            ELSE 'released' END AS state
FROM seed_items s CROSS JOIN LATERAL generate_series(1, s.nrev) v(seq);
CREATE UNIQUE INDEX ON seed_revs (rn);
CREATE UNIQUE INDEX ON seed_revs (g, seq);
ANALYZE seed_revs;

INSERT INTO revisions (id, item_id, seq, code, state, name, description, material, mass_kg,
                       unit_cost, checked_out_by, checked_out_at, created_by, created_at,
                       updated_at, released_at)
SELECT r.id, s.id, r.seq, chr(64 + r.seq), r.state,
       (ARRAY['Bracket', 'Housing', 'Shaft', 'Gear', 'Bearing seat', 'Cover', 'Flange', 'Spacer',
              'Motor mount', 'Frame', 'Plate', 'Bolt', 'Nut', 'Washer', 'Pin', 'Spring', 'Sensor',
              'Cable harness', 'Pulley', 'Coupling', 'Gearbox', 'Drive unit', 'Chassis',
              'Control box'])[1 + s.g % 24] || ' ' || s.g,
       'Synthetic ' || s.kind || ' of the scale check, revision ' || chr(64 + r.seq)
           || CASE WHEN s.g % 7 = 0 THEN ', machined from bar stock' ELSE '' END,
       (ARRAY['Steel S235', 'Aluminium 6061', 'Stainless 1.4301', 'POM', 'PA6', 'Brass'])[1 + s.g % 6],
       0.01 * (1 + s.g % 50), 0.5 * (1 + s.g % 40),
       CASE WHEN r.state = 'in_work' THEN CASE WHEN s.g % 500 = 6 THEN b.id ELSE u.id END END,
       CASE WHEN r.state = 'in_work' THEN now() - (s.g % 90) * interval '1 day' END,
       u.id,
       now() - interval '300 days' + s.g * interval '1 minute' + r.seq * interval '20 days',
       now() - interval '300 days' + s.g * interval '1 minute' + r.seq * interval '21 days',
       CASE WHEN r.state = 'released'
            THEN now() - interval '300 days' + s.g * interval '1 minute' + r.seq * interval '21 days' END
FROM seed_revs r
JOIN seed_items s USING (g)
JOIN seed_users u ON u.un = s.g % 500 + 1
CROSS JOIN seed_busy b;

-- step: bom lines
-- Assembly revisions: sub-assemblies of the next level of the same group
-- (2, or 10 for ASM-000001) followed by 15 parts: STD-0001 everywhere,
-- two more standard parts and 12 parts of the group.
CREATE TEMP TABLE seed_lines AS
SELECT r.id AS parent_rev, r.state AS parent_state, c.g AS child_g,
       10 * (j + 1) AS find_number, 1.0::float8 AS quantity, j AS k
FROM seed_revs r
JOIN seed_items s ON s.g = r.g AND s.kind = 'assembly' AND s.level < 9
CROSS JOIN LATERAL generate_series(0, CASE WHEN s.g = 1 THEN 9 ELSE 1 END) j
JOIN seed_levels lv ON lv.level = s.level + 1
JOIN seed_items c ON c.kind = 'assembly' AND c.project = s.project
                 AND c.idx = lv.start + (s.idx * 31 + j * 17 + r.seq) % lv.size;
INSERT INTO seed_lines
SELECT r.id, r.state,
       CASE WHEN k = 0 THEN 30001
            WHEN k < 3 THEN 30002 + (s.idx * 3 + k + r.seq + s.project) % 49
            ELSE p.g END,
       10 * (CASE WHEN s.g = 1 THEN 10 WHEN s.level < 9 THEN 2 ELSE 0 END + k + 1),
       CASE k WHEN 0 THEN 4 WHEN 1 THEN 8 ELSE 1 + k % 3 END,
       100 + k
FROM seed_revs r
JOIN seed_items s ON s.g = r.g AND s.kind = 'assembly'
CROSS JOIN LATERAL generate_series(0, 14) k
LEFT JOIN seed_items p ON k >= 3 AND p.kind = 'part' AND p.project = s.project
                      AND p.idx = (s.idx * 13 + k * 101 + r.seq * 7) % 1623;

INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes,
                       released_child_revision_id)
SELECT gen_random_uuid(), l.parent_rev, c.id, l.find_number, l.quantity,
       CASE WHEN l.k = 100 THEN 'Standard fastener' ELSE '' END,
       CASE WHEN l.parent_state = 'released' THEN fr.id END
FROM seed_lines l
JOIN seed_items c ON c.g = l.child_g
LEFT JOIN seed_revs fr ON fr.g = l.child_g AND fr.seq = 1 AND fr.state = 'released';

-- step: files
-- One file per revision (the same name in every revision of an item); every
-- fifth file has two versions. All files of group P01 sit in one folder.
CREATE TEMP TABLE seed_files AS
SELECT r.rn, gen_random_uuid() AS id, r.id AS revision_id, s.g, s.kind,
       s.number || CASE WHEN s.kind = 'assembly' THEN '.SLDASM'
                        WHEN s.kind = 'document' THEN '.pdf'
                        WHEN s.g % 3 = 0 THEN '.ipt'
                        ELSE '.SLDPRT' END AS filename,
       CASE WHEN s.kind = 'assembly' THEN 'solidworks-assembly'
            WHEN s.kind = 'document' THEN 'pdf'
            WHEN s.g % 3 = 0 THEN 'inventor-part'
            ELSE 'solidworks-part' END AS format,
       CASE WHEN s.project = 0 THEN 'product/main' ELSE 'cad/sub' || (s.g % 25) END AS folder,
       1 + (r.rn % 5 = 0)::int AS versions,
       u.id AS uploaded_by,
       now() - interval '300 days' + s.g * interval '1 minute' + r.seq * interval '20 days' AS at
FROM seed_revs r
JOIN seed_items s USING (g)
JOIN seed_users u ON u.un = r.rn % 500 + 1;

INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by,
                   uploaded_at, current_version, format, folder)
SELECT f.id, f.revision_id, f.filename,
       CASE f.format WHEN 'pdf' THEN 'application/pdf' ELSE 'application/octet-stream' END,
       10000 + (f.rn * 7919) % 5000000,
       encode(sha256(convert_to(f.filename || '/' || f.rn || '/' || f.versions, 'UTF8')), 'hex'),
       f.uploaded_by, f.at, f.versions, f.format, f.folder
FROM seed_files f;

INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                           uploaded_by, uploaded_at, content_text, references_scanned)
SELECT gen_random_uuid(), f.id, v.version,
       encode(sha256(convert_to(f.filename || '/' || f.rn || '/' || v.version, 'UTF8')), 'hex'),
       10000 + (f.rn * 7919) % 5000000,
       CASE f.format WHEN 'pdf' THEN 'application/pdf' ELSE 'application/octet-stream' END,
       CASE WHEN v.version > 1 THEN 'Updated model' ELSE '' END,
       f.uploaded_by, f.at + (v.version - 1) * interval '1 day',
       CASE WHEN f.kind = 'document' THEN 'Specification drawing tolerances surface finish ' || f.filename
            ELSE '' END,
       true
FROM seed_files f CROSS JOIN LATERAL generate_series(1, f.versions) v(version);

-- The assembly files reference the files of their first sub-assemblies and parts.
INSERT INTO file_references (file_version_id, name)
SELECT DISTINCT fv.id, cf.filename
FROM seed_files f
JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.versions
JOIN seed_lines l ON l.parent_rev = f.revision_id AND (l.k < 2 OR l.k BETWEEN 103 AND 104)
JOIN seed_files cf ON cf.g = l.child_g
JOIN seed_revs cr ON cr.id = cf.revision_id AND cr.latest
WHERE f.kind = 'assembly';

-- step: item references
-- Every document revision is the drawing of 10 parts of its group.
INSERT INTO item_references (id, from_revision_id, to_item_id, kind, note, created_by, created_at)
SELECT gen_random_uuid(), r.id, p.id, 'drawing', '', u.id, now() - interval '100 days'
FROM seed_revs r
JOIN seed_items d ON d.g = r.g AND d.kind = 'document'
CROSS JOIN LATERAL generate_series(0, 9) m
JOIN seed_items p ON p.kind = 'part' AND p.project = d.project AND p.idx = (d.idx * 10 + m) % 1623
JOIN seed_users u ON u.un = r.rn % 500 + 1
ON CONFLICT DO NOTHING;

-- step: changes
CREATE TEMP TABLE seed_changes AS
SELECT n AS cn, gen_random_uuid() AS id,
       CASE WHEN n = 1 THEN 'in_review'
            WHEN n % 10 < 6 THEN 'released'
            WHEN n % 10 = 6 THEN 'draft'
            WHEN n % 10 IN (7, 8) THEN 'in_review'
            WHEN n % 20 = 9 THEN 'rejected'
            ELSE 'cancelled' END AS status
FROM generate_series(1, 20000) n;
CREATE UNIQUE INDEX ON seed_changes (cn);
ANALYZE seed_changes;

INSERT INTO changes (id, number, title, description, priority, status, created_by, created_at,
                     updated_at, submitted_at, closed_at)
SELECT c.id, 'ECO-' || lpad(c.cn::text, 6, '0'),
       CASE WHEN c.cn = 1 THEN 'Product line 1: switch every fastener to the new standard'
            ELSE 'Change ' || c.cn || ': ' || (ARRAY['update tolerances', 'new supplier', 'cost down',
                 'fix interference', 'material change', 'drawing update'])[1 + c.cn % 6] END,
       'Synthetic change of the scale check',
       (ARRAY['low', 'normal', 'normal', 'high', 'critical'])[1 + c.cn % 5],
       c.status,
       CASE WHEN c.cn % 250 = 3 THEN b.id ELSE u.id END,
       now() - interval '200 days' + c.cn * interval '10 minutes',
       now() - interval '200 days' + c.cn * interval '11 minutes',
       CASE WHEN c.status <> 'draft' THEN now() - interval '200 days' + c.cn * interval '11 minutes' END,
       CASE WHEN c.status IN ('released', 'cancelled')
            THEN now() - interval '200 days' + c.cn * interval '12 minutes' END
FROM seed_changes c
JOIN seed_users u ON u.un = c.cn % 500 + 1
CROSS JOIN seed_busy b;

-- ECO-000001: the latest revisions of 2000 parts of group P01; every other
-- change: 5 revisions spread over the vault.
INSERT INTO change_items (change_id, revision_id, added_at)
SELECT c.id, r.id, now() - interval '1 day'
FROM seed_changes c
JOIN seed_items s ON s.kind = 'part' AND s.project = 0 AND s.idx < 2000
JOIN seed_revs r ON r.g = s.g AND r.latest
WHERE c.cn = 1;
INSERT INTO change_items (change_id, revision_id, added_at)
SELECT c.id, r.id, now() - interval '200 days' + c.cn * interval '10 minutes'
FROM seed_changes c
CROSS JOIN LATERAL generate_series(0, 4) k
JOIN seed_revs r ON r.rn = 1 + ((c.cn * 5 + k)::bigint * 7919) % (SELECT count(*) FROM seed_revs)
WHERE c.cn > 1
ON CONFLICT DO NOTHING;

-- Two reviewers per change; the busy user also reviews ~100 changes in
-- review (and ECO-000001).
INSERT INTO change_reviews (change_id, reviewer_id, decision, comment, decided_at)
SELECT c.id, u.id,
       CASE c.status WHEN 'released' THEN 'approved' WHEN 'rejected' THEN 'rejected' ELSE 'pending' END,
       '', CASE WHEN c.status IN ('released', 'rejected') THEN now() - interval '10 days' END
FROM seed_changes c
CROSS JOIN LATERAL (VALUES (c.cn % 500 + 1), ((c.cn * 7 + 3) % 500 + 1)) v(user_n)
JOIN seed_users u ON u.un = v.user_n
ON CONFLICT DO NOTHING;
INSERT INTO change_reviews (change_id, reviewer_id, decision)
SELECT c.id, b.id, 'pending'
FROM seed_changes c CROSS JOIN seed_busy b
WHERE c.cn = 1 OR c.cn % 200 = 7
ON CONFLICT DO NOTHING;

-- step: change requests
INSERT INTO change_requests (id, number, title, description, reason, priority, status,
                             requested_by, assignee, created_at, updated_at)
SELECT gen_random_uuid(), 'ECR-' || lpad(n::text, 6, '0'), 'Request ' || n || ': part does not fit',
       'Synthetic change request of the scale check',
       (ARRAY['defect', 'improvement', 'cost', 'manufacturing', 'supplier'])[1 + n % 5],
       (ARRAY['low', 'normal', 'high'])[1 + n % 3],
       (ARRAY['new', 'in_triage', 'accepted', 'rejected', 'implemented', 'withdrawn'])[1 + n % 6],
       u.id, CASE WHEN n % 50 = 1 THEN b.id WHEN n % 3 = 0 THEN u.id END,
       now() - interval '100 days' + n * interval '20 minutes',
       now() - interval '100 days' + n * interval '21 minutes'
FROM generate_series(1, 5000) n
JOIN seed_users u ON u.un = n % 500 + 1
CROSS JOIN seed_busy b;
INSERT INTO change_request_items (request_id, item_id)
SELECT cr.id, s.id
FROM change_requests cr
CROSS JOIN LATERAL generate_series(0, 2) k
JOIN seed_items s ON s.g = 1 + ((substr(cr.number, 5)::int * 3 + k) * 7919) % 100000
WHERE cr.number LIKE 'ECR-%'
ON CONFLICT DO NOTHING;

-- step: audit log
-- 2M rows: 1.2M about revisions, 500k about items, 300k about changes.
INSERT INTO audit_log (at, user_id, username, action, entity_type, entity_id, summary, details)
SELECT now() - n * interval '13 seconds', u.id, u.username,
       (ARRAY['revision.update', 'revision.checkout', 'revision.checkin', 'file.upload',
              'bom.add', 'revision.release'])[1 + n % 6],
       'revision', r.id, 'Synthetic revision event ' || n, '{}'::jsonb
FROM generate_series(1, 1200000) n
JOIN seed_revs r ON r.rn = 1 + (n * 7) % (SELECT count(*) FROM seed_revs)
JOIN seed_users u ON u.un = n % 500 + 1;
INSERT INTO audit_log (at, user_id, username, action, entity_type, entity_id, summary, details)
SELECT now() - n * interval '31 seconds', u.id, u.username,
       (ARRAY['item.create', 'item.update'])[1 + n % 2],
       'item', s.id, 'Synthetic item event ' || n, '{}'::jsonb
FROM generate_series(1, 500000) n
JOIN seed_items s ON s.g = 1 + (n * 3) % 100000
JOIN seed_users u ON u.un = n % 500 + 1;
INSERT INTO audit_log (at, user_id, username, action, entity_type, entity_id, summary, details)
SELECT now() - n * interval '53 seconds', u.id, u.username,
       (ARRAY['change.create', 'change.submit', 'change.review', 'change.release'])[1 + n % 4],
       'change', c.id, 'Synthetic change event ' || n, '{}'::jsonb
FROM generate_series(1, 300000) n
JOIN seed_changes c ON c.cn = 1 + n % 20000
JOIN seed_users u ON u.un = n % 500 + 1;

-- step: notifications, impacts, watches
INSERT INTO notifications (id, user_id, kind, title, body, link, actor_name, created_at, read_at)
SELECT gen_random_uuid(), u.id,
       (ARRAY['item_update', 'change_update', 'impact', 'review_requested'])[1 + n % 4],
       'Synthetic notification ' || n, 'Something happened to an item you follow.',
       '/changes', 'Scale User', now() - n * interval '1 minute',
       CASE WHEN n % 10 <> 0 THEN now() - n * interval '1 minute' END
FROM generate_series(1, 470000) n
JOIN seed_users u ON u.un = n % 500 + 1;
INSERT INTO notifications (id, user_id, kind, title, body, link, actor_name, created_at, read_at)
SELECT gen_random_uuid(), b.id,
       (ARRAY['item_update', 'change_update', 'impact', 'review_requested'])[1 + n % 4],
       'Synthetic notification for the busy user ' || n, 'Something happened to an item you follow.',
       '/changes', 'Scale User', now() - n * interval '2 minutes',
       CASE WHEN n % 10 <> 0 THEN now() - n * interval '2 minutes' END
FROM generate_series(1, 30000) n CROSS JOIN seed_busy b;

-- 50k impacts on assemblies from released parts; every third one is open.
INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event,
                     relation, detail, created_at, resolved_at, resolved_by, resolution)
SELECT gen_random_uuid(), a.id, ar.id, p.id, pr.id, 'released', 'bom', 'uses the part',
       now() - n * interval '5 minutes',
       CASE WHEN n % 3 <> 0 THEN now() - n * interval '4 minutes' END,
       CASE WHEN n % 3 <> 0 THEN u.id END,
       CASE WHEN n % 3 <> 0 THEN 'acknowledged' END
FROM generate_series(1, 50000) n
JOIN seed_items a ON a.g = 1 + n % 30000
JOIN seed_revs ar ON ar.g = a.g AND ar.latest
JOIN seed_items p ON p.g = 30051 + (n * 13) % 64950
JOIN seed_revs pr ON pr.g = p.g AND pr.latest
JOIN seed_users u ON u.un = n % 500 + 1
ON CONFLICT DO NOTHING;

INSERT INTO watches (user_id, entity_type, entity_id)
SELECT b.id, 'item', s.id FROM seed_items s CROSS JOIN seed_busy b WHERE s.g <= 5000;
INSERT INTO watches (user_id, entity_type, entity_id)
SELECT b.id, 'change', c.id FROM seed_changes c CROSS JOIN seed_busy b WHERE c.cn % 100 = 1;
INSERT INTO watches (user_id, entity_type, entity_id)
SELECT u.id, 'item', s.id
FROM generate_series(1, 100000) n
JOIN seed_users u ON u.un = n % 500 + 1
JOIN seed_items s ON s.g = 1 + (n * 7) % 100000
ON CONFLICT DO NOTHING;
INSERT INTO favorites (user_id, item_id)
SELECT b.id, s.id FROM seed_items s CROSS JOIN seed_busy b WHERE s.g % 500 = 1;

-- step: counters
INSERT INTO counters (name, value) VALUES ('change', 20000), ('change_request', 5000), ('scale:seeded', 1)
ON CONFLICT (name) DO UPDATE SET value = GREATEST(counters.value, EXCLUDED.value);
