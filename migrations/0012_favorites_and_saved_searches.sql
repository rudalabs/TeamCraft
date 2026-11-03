-- Personal working sets, as in Teamcenter (Favorites, Saved Queries) and
-- VistaPoint (my documents, stored searches).

-- Items a user keeps at hand ("★ Favorites" folder of the vault explorer).
CREATE TABLE favorites (
    user_id  UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    item_id  UUID        NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, item_id)
);

-- Named item searches: the filter of /items as a normalised query string
-- (q, type, project, state, system).
CREATE TABLE saved_searches (
    id         UUID PRIMARY KEY,
    user_id    UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       TEXT        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    query      TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);
CREATE INDEX saved_searches_user_idx ON saved_searches (user_id, name);
