-- Data of a TeamCraft v0.1.0 database for the upgrade tests
-- (tests/common/releases.rs). On its own it fits the first v0.1.0 tag
-- (6f68f9a: migrations 0001–0014, 0016–0020, 0022); the published v0.1.0
-- (439724a: 0001–0014, 0016–0031) loads v0.1.0-seed-0021-0031.sql after it.
-- It is loaded into a database at exactly that schema, then the current
-- migrations upgrade it. Every table of the schema gets rows, and every value
-- of the TEXT enum columns (CHECK constraints) occurs at least once (both
-- checked by the test), so a later migration that narrows a CHECK or loses
-- rows makes the upgrade test fail. Invented data only, no organisation names.

-- users: every role and both auth sources
INSERT INTO users (id, username, display_name, email, auth_source, password_hash, oidc_subject, role, active, skin) VALUES
  ('aaaaaaaa-0000-4000-8000-000000000001', 'admin', 'Ada Admin', 'admin@example.org', 'local', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA', NULL, 'admin', TRUE, 'teamcraft'),
  ('aaaaaaaa-0000-4000-8000-000000000002', 'erika', 'Erika Editor', NULL, 'local', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA', NULL, 'editor', TRUE, 'teamcraft'),
  ('aaaaaaaa-0000-4000-8000-000000000003', 'viktor', 'Viktor Viewer', NULL, 'oidc', NULL, 'oidc-subject-3', 'viewer', TRUE, 'teamcraft'),
  ('aaaaaaaa-0000-4000-8000-000000000004', 'max', 'Max Editor', NULL, 'local', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA', NULL, 'editor', FALSE, 'teamcraft');

INSERT INTO sessions (token_hash, user_id, expires_at) VALUES
  ('sessionhash-1', 'aaaaaaaa-0000-4000-8000-000000000002', now() + interval '1 day');
INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix) VALUES
  ('88888888-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002', 'desktop', 'tokenhash-1', 'tc_pat_abcd');
INSERT INTO desktop_auth_codes (code_hash, user_id, challenge, device, expires_at) VALUES
  ('codehash-1', 'aaaaaaaa-0000-4000-8000-000000000002', 'challenge', 'laptop', now() + interval '5 minutes');
INSERT INTO oidc_states (state, nonce, pkce_verifier) VALUES ('state-1', 'nonce-1', 'verifier-1');

-- project groups and members (both sources); one with a numbering scheme
INSERT INTO projects (id, code, name, description, active, oidc_group, created_by, number_scheme) VALUES
  ('bbbbbbbb-0000-4000-8000-000000000001', 'ROCKET', 'Sounding rocket', 'test group', TRUE, 'teamcraft-rocket', 'aaaaaaaa-0000-4000-8000-000000000001', '{PROJECT}-{TYPE}-{SEQ:4}'),
  ('bbbbbbbb-0000-4000-8000-000000000002', 'ROVER', 'Planetary rover', '', TRUE, NULL, NULL, NULL);
INSERT INTO project_members (project_id, user_id, source) VALUES
  ('bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002', 'manual'),
  ('bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000004', 'manual'),
  ('bbbbbbbb-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000003', 'oidc');
INSERT INTO counters (name, value) VALUES ('PRT', 3), ('ASM', 1), ('CHG', 2), ('ECR', 2), ('ROCKET-PRT', 1)
  ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value;

-- items: every item type
INSERT INTO items (id, number, item_type, unit, project_id, created_by) VALUES
  ('cccccccc-0000-4000-8000-000000000001', 'PRT-000001', 'part', 'each', 'bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('cccccccc-0000-4000-8000-000000000002', 'ASM-000001', 'assembly', 'each', 'bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('cccccccc-0000-4000-8000-000000000003', 'DOC-000001', 'document', 'each', 'bbbbbbbb-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('cccccccc-0000-4000-8000-000000000004', 'SW-000001', 'software', 'each', NULL, 'aaaaaaaa-0000-4000-8000-000000000001'),
  ('cccccccc-0000-4000-8000-000000000005', 'SIM-000001', 'simulation', 'each', 'bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('cccccccc-0000-4000-8000-000000000006', 'PRT-000002', 'part', 'kg', 'bbbbbbbb-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002');

-- revisions: every lifecycle state, one checked out
INSERT INTO revisions (id, item_id, seq, code, state, name, description, material, mass_kg, unit_cost, checked_out_by, checked_out_at, created_by, released_at) VALUES
  ('dddddddd-0000-4000-8000-000000000001', 'cccccccc-0000-4000-8000-000000000001', 1, 'A', 'released', 'Bracket', 'first release', 'AlMg3', 0.25, 4.5, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', now()),
  ('dddddddd-0000-4000-8000-000000000002', 'cccccccc-0000-4000-8000-000000000001', 2, 'B', 'in_work', 'Bracket', 'thicker wall', 'AlMg3', 0.3, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', now(), 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000002', 1, 'A', 'in_review', 'Engine mount', '', '', NULL, NULL, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('dddddddd-0000-4000-8000-000000000004', 'cccccccc-0000-4000-8000-000000000003', 1, 'A', 'obsolete', 'Test procedure', '', '', NULL, NULL, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', now()),
  ('dddddddd-0000-4000-8000-000000000005', 'cccccccc-0000-4000-8000-000000000004', 1, 'A', 'released', 'Flight software', '', '', NULL, NULL, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000001', now()),
  ('dddddddd-0000-4000-8000-000000000006', 'cccccccc-0000-4000-8000-000000000005', 1, 'A', 'in_work', 'Bracket static', '', '', NULL, NULL, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('dddddddd-0000-4000-8000-000000000007', 'cccccccc-0000-4000-8000-000000000006', 1, 'A', 'released', 'Bolt M6', '', 'A2-70', 0.01, 0.2, NULL, NULL, 'aaaaaaaa-0000-4000-8000-000000000002', now());

-- BOM (one line remembers the child revision it was released with)
INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes, released_child_revision_id) VALUES
  ('99999999-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000001', 10, 2, 'left and right', NULL),
  ('99999999-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000006', 20, 8, '', 'dddddddd-0000-4000-8000-000000000007');

-- references: every kind
INSERT INTO item_references (id, from_revision_id, to_item_id, kind, note, created_by) VALUES
  ('99999999-0000-4000-8000-000000000011', 'dddddddd-0000-4000-8000-000000000004', 'cccccccc-0000-4000-8000-000000000001', 'drawing', 'sheet 1', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('99999999-0000-4000-8000-000000000012', 'dddddddd-0000-4000-8000-000000000002', 'cccccccc-0000-4000-8000-000000000003', 'specification', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('99999999-0000-4000-8000-000000000013', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000004', 'reference', '', 'aaaaaaaa-0000-4000-8000-000000000002');

-- files with two versions, a folder and CAD file references
INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by, current_version, format, folder) VALUES
  ('eeeeeeee-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000001', 'bracket.step', 'model/step', 120, 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 'aaaaaaaa-0000-4000-8000-000000000002', 2, 'step', 'rocket/brackets'),
  ('eeeeeeee-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003', 'mount.iam', 'application/octet-stream', 300, 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2', 'aaaaaaaa-0000-4000-8000-000000000002', 1, 'inventor-assembly', '');
INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment, uploaded_by, references_scanned) VALUES
  ('ffffffff-0000-4000-8000-000000000001', 'eeeeeeee-0000-4000-8000-000000000001', 1, 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3', 100, 'model/step', 'first', 'aaaaaaaa-0000-4000-8000-000000000002', TRUE),
  ('ffffffff-0000-4000-8000-000000000002', 'eeeeeeee-0000-4000-8000-000000000001', 2, 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 120, 'model/step', 'wall 2.5 mm', 'aaaaaaaa-0000-4000-8000-000000000002', TRUE),
  ('ffffffff-0000-4000-8000-000000000003', 'eeeeeeee-0000-4000-8000-000000000002', 1, 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2', 300, 'application/octet-stream', '', 'aaaaaaaa-0000-4000-8000-000000000002', FALSE);
INSERT INTO file_references (file_version_id, name) VALUES
  ('ffffffff-0000-4000-8000-000000000003', 'bracket.ipt'),
  ('ffffffff-0000-4000-8000-000000000003', 'bolt.ipt');

-- change orders: every status and priority, every review decision
INSERT INTO changes (id, number, title, description, priority, status, created_by, submitted_at, closed_at) VALUES
  ('11111111-0000-4000-8000-000000000001', 'CHG-000001', 'First release', '', 'normal', 'released', 'aaaaaaaa-0000-4000-8000-000000000002', now(), now()),
  ('11111111-0000-4000-8000-000000000002', 'CHG-000002', 'Engine mount', '', 'high', 'in_review', 'aaaaaaaa-0000-4000-8000-000000000002', now(), NULL),
  ('11111111-0000-4000-8000-000000000003', 'CHG-000003', 'Draft', '', 'low', 'draft', 'aaaaaaaa-0000-4000-8000-000000000002', NULL, NULL),
  ('11111111-0000-4000-8000-000000000004', 'CHG-000004', 'Rejected', '', 'critical', 'rejected', 'aaaaaaaa-0000-4000-8000-000000000004', now(), now()),
  ('11111111-0000-4000-8000-000000000005', 'CHG-000005', 'Cancelled', '', 'normal', 'cancelled', 'aaaaaaaa-0000-4000-8000-000000000004', NULL, now());
INSERT INTO change_items (change_id, revision_id) VALUES
  ('11111111-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000001'),
  ('11111111-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003');
INSERT INTO change_reviews (change_id, reviewer_id, decision, comment, decided_at) VALUES
  ('11111111-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000001', 'approved', 'ok', now()),
  ('11111111-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000001', 'pending', '', NULL),
  ('11111111-0000-4000-8000-000000000004', 'aaaaaaaa-0000-4000-8000-000000000001', 'rejected', 'no', now());

-- change requests: every status, reason and priority
INSERT INTO change_requests (id, number, title, description, reason, priority, status, requested_by, assignee) VALUES
  ('22222222-0000-4000-8000-000000000001', 'ECR-000001', 'Crack at the bore', '', 'defect', 'critical', 'new', 'aaaaaaaa-0000-4000-8000-000000000003', NULL),
  ('22222222-0000-4000-8000-000000000002', 'ECR-000002', 'Cheaper bolt', '', 'cost', 'low', 'in_triage', 'aaaaaaaa-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('22222222-0000-4000-8000-000000000003', 'ECR-000003', 'Lighter', '', 'improvement', 'normal', 'accepted', 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('22222222-0000-4000-8000-000000000004', 'ECR-000004', 'Machining', '', 'manufacturing', 'high', 'rejected', 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('22222222-0000-4000-8000-000000000005', 'ECR-000005', 'Supplier change', '', 'supplier', 'normal', 'deferred', 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('22222222-0000-4000-8000-000000000006', 'ECR-000006', 'Edge guard', '', 'safety', 'normal', 'implemented', 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('22222222-0000-4000-8000-000000000007', 'ECR-000007', 'Customer wish', '', 'customer', 'normal', 'withdrawn', 'aaaaaaaa-0000-4000-8000-000000000002', NULL),
  ('22222222-0000-4000-8000-000000000008', 'ECR-000008', 'Other', '', 'other', 'normal', 'new', 'aaaaaaaa-0000-4000-8000-000000000002', NULL);
INSERT INTO change_request_items (request_id, item_id) VALUES
  ('22222222-0000-4000-8000-000000000001', 'cccccccc-0000-4000-8000-000000000001');
INSERT INTO change_request_changes (request_id, change_id, linked_by) VALUES
  ('22222222-0000-4000-8000-000000000003', '11111111-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000002');
INSERT INTO change_request_comments (id, request_id, author_id, body) VALUES
  ('22222222-0000-4000-8000-000000000101', '22222222-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000002', 'Seen it, checking.');
INSERT INTO change_request_attachments (id, request_id, filename, content_type, size_bytes, sha256, uploaded_by) VALUES
  ('22222222-0000-4000-8000-000000000201', '22222222-0000-4000-8000-000000000001', 'crack.png', 'image/png', 10, 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4', 'aaaaaaaa-0000-4000-8000-000000000003'),
  ('22222222-0000-4000-8000-000000000202', '22222222-0000-4000-8000-000000000001', 'b.jpg', 'image/jpeg', 10, 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4', 'aaaaaaaa-0000-4000-8000-000000000003'),
  ('22222222-0000-4000-8000-000000000203', '22222222-0000-4000-8000-000000000001', 'c.gif', 'image/gif', 10, 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4', 'aaaaaaaa-0000-4000-8000-000000000003'),
  ('22222222-0000-4000-8000-000000000204', '22222222-0000-4000-8000-000000000001', 'd.webp', 'image/webp', 10, 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4', 'aaaaaaaa-0000-4000-8000-000000000003'),
  ('22222222-0000-4000-8000-000000000205', '22222222-0000-4000-8000-000000000001', 'e.pdf', 'application/pdf', 10, 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4', 'aaaaaaaa-0000-4000-8000-000000000003');

-- impacts: every event, relation and resolution
INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event, relation, detail, change_id, resolved_at, resolved_by, resolution, note) VALUES
  ('33333333-0000-4000-8000-000000000001', 'cccccccc-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000001', 'released', 'bom', 'find no. 10', '11111111-0000-4000-8000-000000000001', NULL, NULL, NULL, ''),
  ('33333333-0000-4000-8000-000000000002', 'cccccccc-0000-4000-8000-000000000003', 'dddddddd-0000-4000-8000-000000000004', 'cccccccc-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000001', 'released', 'drawing', '', NULL, now(), 'aaaaaaaa-0000-4000-8000-000000000002', 'acknowledged', 'fit unchanged'),
  ('33333333-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000002', 'cccccccc-0000-4000-8000-000000000003', 'dddddddd-0000-4000-8000-000000000004', 'obsolete', 'specification', '', NULL, now(), NULL, 'superseded', ''),
  ('33333333-0000-4000-8000-000000000004', 'cccccccc-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000004', 'dddddddd-0000-4000-8000-000000000005', 'released', 'reference', '', NULL, now(), NULL, 'obsolete', ''),
  ('33333333-0000-4000-8000-000000000005', 'cccccccc-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000003', 'cccccccc-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000007', 'released', 'file', 'mount.iam references bolt.ipt', NULL, NULL, NULL, NULL, ''),
  ('33333333-0000-4000-8000-000000000006', 'cccccccc-0000-4000-8000-000000000005', 'dddddddd-0000-4000-8000-000000000006', 'cccccccc-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000001', 'released', 'simulation', '', NULL, NULL, NULL, NULL, '');

-- notifications: every kind, subject type and delivery state
INSERT INTO notifications (id, user_id, kind, title, body, link, subject_type, subject_ref, actor_name, email_state, chat_state, read_at) VALUES
  ('44444444-0000-4000-8000-000000000001', 'aaaaaaaa-0000-4000-8000-000000000001', 'review_requested', 'Review CHG-000002', '', '/changes/x', 'change', 'CHG-000002', 'Erika', 'pending', 'none', NULL),
  ('44444444-0000-4000-8000-000000000002', 'aaaaaaaa-0000-4000-8000-000000000002', 'change_update', 'CHG-000001 released', '', '', 'change', 'CHG-000001', 'Ada', 'sent', 'sent', now()),
  ('44444444-0000-4000-8000-000000000003', 'aaaaaaaa-0000-4000-8000-000000000002', 'impact', 'Check ASM-000001', '', '', 'revision', 'ASM-000001/A', 'Ada', 'failed', 'failed', NULL),
  ('44444444-0000-4000-8000-000000000004', 'aaaaaaaa-0000-4000-8000-000000000002', 'item_update', 'PRT-000001/B', '', '', 'revision', 'PRT-000001/B', 'Max', 'skipped', 'skipped', NULL),
  ('44444444-0000-4000-8000-000000000005', 'aaaaaaaa-0000-4000-8000-000000000002', 'unlock', 'Unlocked', '', '', '', '', 'Ada', 'none', 'pending', NULL),
  ('44444444-0000-4000-8000-000000000006', 'aaaaaaaa-0000-4000-8000-000000000002', 'change_request', 'ECR-000001', '', '', 'change_request', 'ECR-000001', 'Viktor', 'none', 'none', NULL),
  ('44444444-0000-4000-8000-000000000007', 'aaaaaaaa-0000-4000-8000-000000000002', 'test', 'Test', '', '', '', '', '', 'none', 'none', NULL);
INSERT INTO watches (user_id, entity_type, entity_id) VALUES
  ('aaaaaaaa-0000-4000-8000-000000000002', 'item', 'cccccccc-0000-4000-8000-000000000001'),
  ('aaaaaaaa-0000-4000-8000-000000000002', 'change', '11111111-0000-4000-8000-000000000002'),
  ('aaaaaaaa-0000-4000-8000-000000000003', 'change_request', '22222222-0000-4000-8000-000000000001');

-- favourites, saved searches, staged imports (both kinds)
INSERT INTO favorites (user_id, item_id) VALUES ('aaaaaaaa-0000-4000-8000-000000000002', 'cccccccc-0000-4000-8000-000000000001');
INSERT INTO saved_searches (id, user_id, name, query) VALUES
  ('88888888-0000-4000-8000-000000000011', 'aaaaaaaa-0000-4000-8000-000000000002', 'My brackets', 'q=bracket&type=part');
INSERT INTO staged_imports (id, user_id, kind, filename, size_bytes, content, sha256) VALUES
  ('88888888-0000-4000-8000-000000000021', 'aaaaaaaa-0000-4000-8000-000000000002', 'bom_table', 'bom.csv', 5, '\x612c620a31'::bytea, NULL),
  ('88888888-0000-4000-8000-000000000022', 'aaaaaaaa-0000-4000-8000-000000000002', 'step', 'mount.step', 300, NULL, 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2');

-- simulation: every basis role, results with limits
INSERT INTO simulation_bases (id, revision_id, basis_revision_id, file_version_id, file_name, file_version, role, note, created_by) VALUES
  ('55555555-0000-4000-8000-000000000001', 'dddddddd-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000001', 'ffffffff-0000-4000-8000-000000000002', 'bracket.step', 2, 'geometry', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('55555555-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000001', NULL, '', NULL, 'mesh', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('55555555-0000-4000-8000-000000000003', 'dddddddd-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000007', NULL, '', NULL, 'material', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('55555555-0000-4000-8000-000000000004', 'dddddddd-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000004', NULL, '', NULL, 'load_case', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('55555555-0000-4000-8000-000000000005', 'dddddddd-0000-4000-8000-000000000006', 'dddddddd-0000-4000-8000-000000000005', NULL, '', NULL, 'other', '', 'aaaaaaaa-0000-4000-8000-000000000002');
INSERT INTO simulation_results (id, revision_id, position, name, value, unit, lower_limit, upper_limit, load_case, solver, solver_version, note, created_by) VALUES
  ('55555555-0000-4000-8000-000000000011', 'dddddddd-0000-4000-8000-000000000006', 1, 'Max. stress', 180, 'MPa', NULL, 250, 'LC1', 'CalculiX', '2.21', '', 'aaaaaaaa-0000-4000-8000-000000000002'),
  ('55555555-0000-4000-8000-000000000012', 'dddddddd-0000-4000-8000-000000000006', 2, 'Safety factor', 1.4, '', 1.5, NULL, 'LC1', '', '', '', 'aaaaaaaa-0000-4000-8000-000000000002');

-- a chunked upload session in progress
INSERT INTO upload_sessions (id, user_id, revision_id, filename, content_type, total_bytes, received_bytes, sha256, base_version, folder, expires_at) VALUES
  ('88888888-0000-4000-8000-000000000031', 'aaaaaaaa-0000-4000-8000-000000000002', 'dddddddd-0000-4000-8000-000000000002', 'bracket.step', 'model/step', 1000, 400, 'e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5', 2, 'rocket/brackets', now() + interval '1 day');

-- webhooks: every event, deliveries in every state
INSERT INTO webhooks (id, name, url, secret, secret_hint, events, project_id, active, created_by) VALUES
  ('66666666-0000-4000-8000-000000000001', 'CI', 'https://hooks.example.org/teamcraft', 'abcdefghijklmnopqrstuvwxyz0123456789', '…6789',
   ARRAY['revision.released', 'revision.obsoleted', 'change.submitted', 'change.approved', 'change.rejected', 'change.released',
         'change_request.created', 'change_request.decided', 'simulation.result_added'],
   'bbbbbbbb-0000-4000-8000-000000000001', TRUE, 'aaaaaaaa-0000-4000-8000-000000000001');
INSERT INTO webhook_deliveries (id, webhook_id, event_id, event, payload, status, attempts, last_status_code, last_error) VALUES
  ('77777777-0000-4000-8000-000000000001', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000101', 'revision.released', '{"item":"PRT-000001"}', 'sent', 1, 200, ''),
  ('77777777-0000-4000-8000-000000000002', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000102', 'change.submitted', '{}', 'pending', 0, NULL, ''),
  ('77777777-0000-4000-8000-000000000003', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000103', 'change_request.created', '{}', 'failed', 8, 500, 'server error'),
  -- the other events of the delivery CHECK (found missing by the enum check of the test)
  ('77777777-0000-4000-8000-000000000004', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000104', 'revision.obsoleted', '{}', 'sent', 1, 204, ''),
  ('77777777-0000-4000-8000-000000000005', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000105', 'change.approved', '{}', 'sent', 1, 200, ''),
  ('77777777-0000-4000-8000-000000000006', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000106', 'change.rejected', '{}', 'sent', 2, 200, ''),
  ('77777777-0000-4000-8000-000000000007', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000107', 'change.released', '{}', 'pending', 0, NULL, ''),
  ('77777777-0000-4000-8000-000000000008', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000108', 'change_request.decided', '{}', 'sent', 1, 200, ''),
  ('77777777-0000-4000-8000-000000000009', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000109', 'simulation.result_added', '{}', 'failed', 3, 410, 'gone'),
  ('77777777-0000-4000-8000-000000000010', '66666666-0000-4000-8000-000000000001', '77777777-0000-4000-8000-000000000110', 'ping', '{}', 'sent', 1, 200, '');

-- audit trail
INSERT INTO audit_log (user_id, username, action, entity_type, entity_id, summary, details) VALUES
  ('aaaaaaaa-0000-4000-8000-000000000002', 'erika', 'item.create', 'item', 'cccccccc-0000-4000-8000-000000000001', 'Created PRT-000001', '{}'),
  ('aaaaaaaa-0000-4000-8000-000000000001', 'admin', 'change.release', 'change', '11111111-0000-4000-8000-000000000001', 'Released CHG-000001', '{"revisions":1}');
