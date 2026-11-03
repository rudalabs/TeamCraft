-- Visual skin chosen by the user (see domain::SKINS). Unknown values fall back to 'warr'.
ALTER TABLE users ADD COLUMN skin TEXT NOT NULL DEFAULT 'warr';
