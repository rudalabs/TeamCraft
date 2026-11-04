-- Simulation management (FEA/CFD): the item type `simulation`, the
-- revisions a simulation is based on and its key result values.

ALTER TABLE items DROP CONSTRAINT items_item_type_check;
ALTER TABLE items ADD CONSTRAINT items_item_type_check
    CHECK (item_type IN ('part', 'assembly', 'document', 'software', 'simulation'));

-- Change propagation: a simulation is flagged when the revision it is based
-- on is superseded by a new release or becomes obsolete.
ALTER TABLE impacts DROP CONSTRAINT impacts_relation_check;
ALTER TABLE impacts ADD CONSTRAINT impacts_relation_check
    CHECK (relation IN ('bom', 'drawing', 'specification', 'reference', 'file', 'simulation'));

-- What a simulation revision was computed with: an item revision and,
-- optionally, the exact file version. Like BOM lines the links belong to
-- the simulation revision: changed only while it is checked out, copied by
-- Revise and frozen at release. `file_name` / `file_version` keep what was
-- simulated even if the file version is deleted later (in-work revisions).
CREATE TABLE simulation_bases (
    id                 UUID PRIMARY KEY,
    revision_id        UUID        NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    basis_revision_id  UUID        NOT NULL REFERENCES revisions (id),
    file_version_id    UUID        REFERENCES file_versions (id) ON DELETE SET NULL,
    file_name          TEXT        NOT NULL DEFAULT '',
    file_version       INTEGER,
    role               TEXT        NOT NULL CHECK (role IN ('geometry', 'mesh', 'material', 'load_case', 'other')),
    note               TEXT        NOT NULL DEFAULT '',
    created_by         UUID        NOT NULL REFERENCES users (id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((file_name = '') = (file_version IS NULL))
);
CREATE INDEX simulation_bases_basis_idx ON simulation_bases (basis_revision_id);
CREATE UNIQUE INDEX simulation_bases_unique ON simulation_bases
    (revision_id, basis_revision_id, role, lower(file_name));

-- Key result values of a simulation revision (max. stress, safety factor,
-- pressure drop, …). Pass/fail follows from the optional limits.
CREATE TABLE simulation_results (
    id              UUID PRIMARY KEY,
    revision_id     UUID             NOT NULL REFERENCES revisions (id) ON DELETE CASCADE,
    position        INTEGER          NOT NULL DEFAULT 0,
    name            TEXT             NOT NULL CHECK (name <> ''),
    value           DOUBLE PRECISION NOT NULL,
    unit            TEXT             NOT NULL DEFAULT '',
    lower_limit     DOUBLE PRECISION,
    upper_limit     DOUBLE PRECISION,
    load_case       TEXT             NOT NULL DEFAULT '',
    solver          TEXT             NOT NULL DEFAULT '',
    solver_version  TEXT             NOT NULL DEFAULT '',
    note            TEXT             NOT NULL DEFAULT '',
    created_by      UUID             NOT NULL REFERENCES users (id),
    created_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CHECK (lower_limit IS NULL OR upper_limit IS NULL OR lower_limit <= upper_limit)
);
CREATE UNIQUE INDEX simulation_results_unique ON simulation_results
    (revision_id, lower(name), lower(load_case));

-- Result file formats are new in the format registry: classify files that
-- were unknown so far, and `.sim` files again (NX or STAR-CCM+).
UPDATE files SET format = NULL
WHERE (format = '' AND lower(filename) ~ '\.(vtk|vtu|vtp|vts|vtr|pvd|vtm|frd|op2|f06|bdf|nas|rst|rth|db|odb|sta|msg|cgns|exo|e|cas|dat|h5|k|d3plot|foam)$')
   OR (format = '' AND (filename = 'controlDict' OR lower(filename) ~ '^d3plot[0-9]*$'))
   OR (format = 'nx-simulation' AND lower(filename) LIKE '%.sim');
