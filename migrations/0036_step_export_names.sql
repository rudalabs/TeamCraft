-- The name a STEP file was exported under (the FILE_NAME entity of its
-- header), when it differs from the stored file name: a second name of the
-- file for the CAD file references of other files, used only where no file
-- has the referenced name itself (src/cadrefs.rs, services::references).
ALTER TABLE file_versions ADD COLUMN export_name TEXT;
CREATE INDEX file_versions_export_name_idx ON file_versions (lower(export_name))
    WHERE export_name IS NOT NULL;

-- FALSE = the header was not read yet: the startup scan
-- (files::scan_unscanned_references) reads the current versions of the
-- stored STEP files and fills in their export names.
ALTER TABLE file_versions ADD COLUMN export_name_scanned BOOLEAN NOT NULL DEFAULT FALSE;
