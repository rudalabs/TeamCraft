-- STEP assemblies with external references now also store how often they
-- place each referenced file (file_references.instances, see 0064 and
-- stepbom::external_instance_counts). The current versions of STEP files
-- that reference other files are read again by the startup scan
-- (files::scan_unscanned_references), which fills the counts.
UPDATE file_versions fv
SET references_scanned = FALSE
FROM files f
WHERE fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format = 'step'
  AND EXISTS (SELECT 1 FROM file_references fr WHERE fr.file_version_id = fv.id);

DELETE FROM file_references fr
USING file_versions fv, files f
WHERE fr.file_version_id = fv.id
  AND fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format = 'step';
