-- Visual skin chosen by the user (see domain::SKINS). Unknown values fall back to the default skin.
ALTER TABLE users ADD COLUMN skin TEXT NOT NULL DEFAULT 'teamcraft';
