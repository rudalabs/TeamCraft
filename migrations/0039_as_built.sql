-- As-built records: the item type `as_built` is one physical unit (or lot)
-- as it was really built. Each as-built revision records the design
-- revision the unit was built to and the deviations from that design.

ALTER TABLE items DROP CONSTRAINT items_item_type_check;
ALTER TABLE items ADD CONSTRAINT items_item_type_check
    CHECK (item_type IN ('part', 'assembly', 'document', 'software', 'simulation', 'as_built'));

-- The build record of an as-built revision. Like BOM lines it belongs to the
-- revision: changed only while it is checked out, copied by Revise and
-- frozen at release. `design_item_id` repeats the item of the design
-- revision for the serial-number lookup.
CREATE TABLE as_built_records (
    revision_id         UUID        PRIMARY KEY REFERENCES revisions (id) ON DELETE CASCADE,
    design_revision_id  UUID        NOT NULL REFERENCES revisions (id),
    design_item_id      UUID        NOT NULL REFERENCES items (id),
    serial_number       TEXT        NOT NULL DEFAULT '',
    built_on            DATE,
    note                TEXT        NOT NULL DEFAULT '',
    updated_by          UUID        NOT NULL REFERENCES users (id),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX as_built_records_design_item_idx ON as_built_records (design_item_id);
CREATE INDEX as_built_records_design_revision_idx ON as_built_records (design_revision_id);

-- How the built unit departs from its design, and what was decided.
CREATE TABLE as_built_deviations (
    id           UUID        PRIMARY KEY,
    revision_id  UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    position     INTEGER     NOT NULL DEFAULT 0,
    kind         TEXT        NOT NULL CHECK (kind IN ('dimension', 'material', 'component', 'process', 'other')),
    feature      TEXT        NOT NULL CHECK (feature <> ''),
    designed     TEXT        NOT NULL DEFAULT '',
    built        TEXT        NOT NULL DEFAULT '',
    disposition  TEXT        NOT NULL DEFAULT 'open'
                 CHECK (disposition IN ('open', 'use_as_is', 'rework', 'repair', 'scrap')),
    note         TEXT        NOT NULL DEFAULT '',
    created_by   UUID        NOT NULL REFERENCES users (id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX as_built_deviations_revision_idx ON as_built_deviations (revision_id, position);
