-- Mass properties (mass, centre of gravity, inertia) of assemblies from the
-- exported meshes of their parts (see docs/src/research/mass-properties.md).

-- The material library: densities administrators maintain. The material
-- attribute of a revision is looked up here by name (case-insensitive).
CREATE TABLE materials (
    id            UUID PRIMARY KEY,
    name          TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    density_kg_m3 DOUBLE PRECISION NOT NULL CHECK (density_kg_m3 > 0 AND density_kg_m3 <= 100000),
    notes         TEXT NOT NULL DEFAULT '' CHECK (length(notes) <= 2000),
    created_by    UUID NOT NULL REFERENCES users (id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX materials_name_idx ON materials (lower(name));

-- Mass settings of a revision: a density override, the file whose mesh is
-- the geometry (NULL = picked automatically) and measured values, which win
-- over everything computed. CG in metres in the frame of the geometry files,
-- inertia in kg·m² about the measured CG, tensor form
-- [xx, yy, zz, xy, yz, xz] with the off-diagonals = −∫ xy dm.
CREATE TABLE revision_mass (
    revision_id            UUID PRIMARY KEY REFERENCES revisions (id) ON DELETE CASCADE,
    density_kg_m3          DOUBLE PRECISION CHECK (density_kg_m3 IS NULL OR (density_kg_m3 > 0 AND density_kg_m3 <= 100000)),
    geometry_file_id       UUID REFERENCES files (id) ON DELETE SET NULL,
    measured_mass_kg       DOUBLE PRECISION CHECK (measured_mass_kg IS NULL OR measured_mass_kg >= 0),
    measured_cg_m          DOUBLE PRECISION[] CHECK (measured_cg_m IS NULL OR cardinality(measured_cg_m) = 3),
    measured_inertia_kg_m2 DOUBLE PRECISION[] CHECK (measured_inertia_kg_m2 IS NULL OR cardinality(measured_inertia_kg_m2) = 6),
    measured_note          TEXT NOT NULL DEFAULT '' CHECK (length(measured_note) <= 2000),
    updated_by             UUID REFERENCES users (id),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (measured_cg_m IS NULL OR measured_mass_kg IS NOT NULL),
    CHECK (measured_inertia_kg_m2 IS NULL OR measured_cg_m IS NOT NULL)
);
CREATE INDEX revision_mass_geometry_idx ON revision_mass (geometry_file_id) WHERE geometry_file_id IS NOT NULL;

-- Per file: the length unit of its coordinates (NULL = the format's
-- default, mm; glTF m) and how many units of the item the file shows.
CREATE TABLE file_mass_settings (
    file_id     UUID PRIMARY KEY REFERENCES files (id) ON DELETE CASCADE,
    length_unit TEXT CHECK (length_unit IS NULL OR length_unit IN ('mm', 'm', 'in')),
    instances   INTEGER NOT NULL DEFAULT 1 CHECK (instances BETWEEN 1 AND 100000)
);
