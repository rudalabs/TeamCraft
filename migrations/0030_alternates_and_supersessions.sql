-- Alternate parts and supersession (feature 6, see
-- docs/src/research/supersede-alternates.md). Both are item-level relations,
-- not revisioned: "B can be built in instead of A" stays true for old
-- assemblies too.

-- B (alternate_item_id) may be used instead of A (item_id) in every
-- assembly – form, fit and function are the same. A two-way alternate is
-- stored as two rows.
CREATE TABLE item_alternates (
    item_id            UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    alternate_item_id  UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    note               TEXT        NOT NULL DEFAULT '',
    created_by         UUID        NOT NULL REFERENCES users (id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (item_id, alternate_item_id),
    CHECK (item_id <> alternate_item_id)
);
CREATE INDEX item_alternates_alternate_idx ON item_alternates (alternate_item_id);

-- A (item_id) is superseded by B (successor_item_id): do not use A for new
-- designs any more, use B. At most one successor per item.
CREATE TABLE item_supersessions (
    item_id            UUID        PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
    successor_item_id  UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    note               TEXT        NOT NULL DEFAULT '',
    created_by         UUID        NOT NULL REFERENCES users (id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (item_id <> successor_item_id)
);
CREATE INDEX item_supersessions_successor_idx ON item_supersessions (successor_item_id);
