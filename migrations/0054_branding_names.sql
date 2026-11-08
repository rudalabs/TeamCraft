-- Instance name in the header: administrators rename what the top left shows
-- ("TeamCraft PLM"). Key/value: no row = the default (TEAMCRAFT_INSTANCE_NAME,
-- then "TeamCraft"; the subtitle defaults to "PLM"). A subtitle row may hold
-- the empty string (= no subtitle); a display_name row never does.
CREATE TABLE branding_text (
    key        TEXT PRIMARY KEY CHECK (key IN ('display_name', 'subtitle')),
    value      TEXT        NOT NULL CHECK (char_length(value) <= 40),
    updated_by UUID REFERENCES users (id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
