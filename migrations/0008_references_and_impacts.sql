-- Assembly and reference tracking with change propagation to dependents.

-- As-released configuration: when the parent revision is released, every
-- BOM line remembers the child revision it resolved to (the latest released
-- one), so a released assembly always knows exactly what it was released
-- with (revision rule "as released").
ALTER TABLE bom_lines ADD COLUMN released_child_revision_id UUID REFERENCES revisions (id);

-- References between items besides the BOM: a drawing documents a part, a
-- specification applies to an item, a model references another one
-- (skeleton, interface, mating part). Like BOM lines they belong to the
-- revision of the referencing item, are copied by Revise and frozen at release.
CREATE TABLE item_references (
    id                       UUID PRIMARY KEY,
    from_revision_id         UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    to_item_id               UUID        NOT NULL REFERENCES items (id),
    kind                     TEXT        NOT NULL CHECK (kind IN ('drawing', 'specification', 'reference')),
    note                     TEXT        NOT NULL DEFAULT '',
    released_to_revision_id  UUID        REFERENCES revisions (id),
    created_by               UUID        NOT NULL REFERENCES users (id),
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_revision_id, to_item_id, kind)
);
CREATE INDEX item_references_to_idx ON item_references (to_item_id);

-- File names found inside CAD files when a version is uploaded: an assembly
-- references its part files, a drawing its model (see src/cadrefs.rs).
CREATE TABLE file_references (
    file_version_id  UUID NOT NULL REFERENCES file_versions (id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    PRIMARY KEY (file_version_id, name)
);
CREATE INDEX file_references_name_idx ON file_references (lower(name));
-- FALSE = not scanned yet (versions from before this migration are scanned
-- at startup).
ALTER TABLE file_versions ADD COLUMN references_scanned BOOLEAN NOT NULL DEFAULT FALSE;

-- Change propagation: a dependent item has to be checked because an item it
-- uses (BOM), references (drawing, specification, reference) or whose files
-- its CAD files reference was released in a new revision or made obsolete.
-- An impact is resolved by an acknowledgement, by a newer released revision
-- of the dependent or when the dependent becomes obsolete.
CREATE TABLE impacts (
    id                  UUID PRIMARY KEY,
    item_id             UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    revision_id         UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    source_item_id      UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    source_revision_id  UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    event               TEXT        NOT NULL CHECK (event IN ('released', 'obsolete')),
    relation            TEXT        NOT NULL CHECK (relation IN ('bom', 'drawing', 'specification', 'reference', 'file')),
    detail              TEXT        NOT NULL DEFAULT '',
    change_id           UUID        REFERENCES changes (id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at         TIMESTAMPTZ,
    resolved_by         UUID        REFERENCES users (id),
    resolution          TEXT        CHECK (resolution IN ('acknowledged', 'superseded', 'obsolete')),
    note                TEXT        NOT NULL DEFAULT '',
    CHECK ((resolved_at IS NULL) = (resolution IS NULL))
);
CREATE INDEX impacts_item_idx ON impacts (item_id);
CREATE INDEX impacts_open_idx ON impacts (created_at DESC) WHERE resolved_at IS NULL;
-- One open impact per dependent and source; a newer event updates it.
CREATE UNIQUE INDEX impacts_one_open ON impacts (item_id, source_item_id) WHERE resolved_at IS NULL;
