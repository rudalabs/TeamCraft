-- Instance branding: the logos administrators may replace.
--   wide   – the wide logo at the top left (web header, desktop title bar)
--   square – the square logo at the bottom left (web footer and tab icon,
--            desktop navigation rail)
-- One row per kind; no row = the built-in TeamCraft logo.
CREATE TABLE branding_logos (
    kind         TEXT PRIMARY KEY CHECK (kind IN ('wide', 'square')),
    content_type TEXT        NOT NULL CHECK (content_type IN
                     ('image/png', 'image/jpeg', 'image/webp', 'image/svg+xml')),
    data         BYTEA       NOT NULL CHECK (octet_length(data) BETWEEN 1 AND 524288),
    sha256       TEXT        NOT NULL,
    updated_by   UUID REFERENCES users (id) ON DELETE SET NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
