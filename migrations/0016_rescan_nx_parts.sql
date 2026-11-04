-- NX parts are now read through their stream container (src/nxpart.rs):
-- only the reference streams count, not the save history in the rest of the
-- file. Drop the references found by the old whole-file scan of the current
-- versions; the startup scan (files::scan_unscanned_references) reads them
-- again.
DELETE FROM file_references fr
USING file_versions fv, files f
WHERE fr.file_version_id = fv.id
  AND fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format = 'nx-part';

UPDATE file_versions fv
SET references_scanned = FALSE
FROM files f
WHERE fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format = 'nx-part';
