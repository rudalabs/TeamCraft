-- Result values taken from files at check-in (never from big result files):
-- where a value came from, and the extraction templates of a project group.

ALTER TABLE simulation_results ADD COLUMN source TEXT NOT NULL DEFAULT '';

-- A template finds one number in a small text file (solver log, summary
-- .txt/.csv/.html) of a simulation revision of the group. `regex`: the first
-- capture group of the pattern on a line; `csv`: a column (optionally of the
-- row whose key column has a given value).
CREATE TABLE result_templates (
    id              UUID PRIMARY KEY,
    project_id      UUID             NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name            TEXT             NOT NULL CHECK (name <> ''),
    file_pattern    TEXT             NOT NULL CHECK (file_pattern <> ''),
    method          TEXT             NOT NULL CHECK (method IN ('regex', 'csv')),
    pattern         TEXT             NOT NULL DEFAULT '',
    csv_column      TEXT             NOT NULL DEFAULT '',
    csv_key_column  TEXT             NOT NULL DEFAULT '',
    csv_key_value   TEXT             NOT NULL DEFAULT '',
    pick            TEXT             NOT NULL DEFAULT 'last' CHECK (pick IN ('first', 'last')),
    unit            TEXT             NOT NULL DEFAULT '',
    lower_limit     DOUBLE PRECISION,
    upper_limit     DOUBLE PRECISION,
    load_case       TEXT             NOT NULL DEFAULT '',
    solver          TEXT             NOT NULL DEFAULT '',
    enabled         BOOLEAN          NOT NULL DEFAULT true,
    created_by      UUID             REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CHECK ((method = 'regex' AND pattern <> '') OR (method = 'csv' AND csv_column <> '')),
    CHECK (lower_limit IS NULL OR upper_limit IS NULL OR lower_limit <= upper_limit)
);
CREATE UNIQUE INDEX result_templates_unique ON result_templates
    (project_id, lower(name), lower(load_case));
