-- Issue #152 step 7: every item has an owner (default: its creator) who can
-- be changed by the managers of the project group and by administrators.
ALTER TABLE items ADD COLUMN owner_id UUID REFERENCES users (id);
UPDATE items SET owner_id = created_by;

-- Inserts that do not name an owner (imports, fixtures, older code paths)
-- get the creator, so that the column can be NOT NULL.
CREATE FUNCTION items_default_owner() RETURNS trigger AS $$
BEGIN
    IF NEW.owner_id IS NULL THEN
        NEW.owner_id := NEW.created_by;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER items_default_owner BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION items_default_owner();

ALTER TABLE items ALTER COLUMN owner_id SET NOT NULL;
CREATE INDEX items_owner_idx ON items (owner_id);
