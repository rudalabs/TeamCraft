-- Shape signatures for the similar part search (see src/shape.rs).
--
-- A signature depends only on the file content, so it is stored once per
-- blob (SHA-256), whichever files share it. It is derived data: computed
-- from the 3D preview mesh in the isolated preview process, replaced when
-- the computation changes (`version`), never edited.
CREATE TABLE shape_signatures (
    sha256       TEXT             PRIMARY KEY,
    version      SMALLINT         NOT NULL,
    -- oriented bounding box edge lengths, largest first (model units)
    extent_1     DOUBLE PRECISION NOT NULL CHECK (extent_1 > 0),
    extent_2     DOUBLE PRECISION NOT NULL CHECK (extent_2 >= 0),
    extent_3     DOUBLE PRECISION NOT NULL CHECK (extent_3 >= 0),
    -- extent_2 / extent_1 and extent_3 / extent_1 for the search that ignores scale
    ratio_2      REAL             NOT NULL,
    ratio_3      REAL             NOT NULL,
    area         DOUBLE PRECISION NOT NULL,
    volume       DOUBLE PRECISION NOT NULL,
    -- watertight and consistently oriented: only then is the volume reliable
    closed       BOOLEAN          NOT NULL,
    triangles    BIGINT           NOT NULL,
    -- D2 shape distribution (32 bins that sum to 1)
    d2           REAL[]           NOT NULL,
    computed_at  TIMESTAMPTZ      NOT NULL DEFAULT now()
);
-- Prefilters of the search: size windows and proportion windows.
CREATE INDEX shape_signatures_extent_idx ON shape_signatures (extent_1, extent_2);
CREATE INDEX shape_signatures_ratio_idx ON shape_signatures (ratio_2, ratio_3);
