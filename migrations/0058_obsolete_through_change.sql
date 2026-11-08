-- Ownership and approvals, step 3 (issue #152): obsolete and supersede
-- through a change order.
--
-- A group can require that released revisions are obsoleted or superseded
-- only through a reviewed change order (default off: the direct action stays).
ALTER TABLE project_policies
    ADD COLUMN obsolete_requires_change BOOLEAN NOT NULL DEFAULT FALSE;

-- A change order line is either the release of an in-work revision (the
-- classic line) or an action on a released revision, applied when the change
-- is released: 'obsolete' marks it obsolete, 'supersede' marks it obsolete and
-- declares the item superseded by successor_item_id.
ALTER TABLE change_items
    ADD COLUMN action TEXT NOT NULL DEFAULT 'release'
        CHECK (action IN ('release', 'obsolete', 'supersede')),
    ADD COLUMN successor_item_id UUID REFERENCES items(id) ON DELETE CASCADE,
    ADD CONSTRAINT change_items_successor_check
        CHECK ((action = 'supersede') = (successor_item_id IS NOT NULL));
