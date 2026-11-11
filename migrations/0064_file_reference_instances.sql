-- How often an assembly places a referenced file (#247): the instance count
-- where the format tells it – SOLIDWORKS 2015+ assemblies (component tree,
-- instances excluded from the BOM not counted, 0 = only excluded ones) and
-- NX assemblies (placed components on visible layers). NULL = not known
-- (every other format, references found by name only).
ALTER TABLE file_references ADD COLUMN instances INTEGER
    CHECK (instances IS NULL OR instances >= 0);

-- The current versions of those formats are read again by the startup scan
-- (files::scan_unscanned_references), which fills the counts.
DELETE FROM file_references fr
USING file_versions fv, files f
WHERE fr.file_version_id = fv.id
  AND fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format IN ('nx-part', 'solidworks-assembly');

UPDATE file_versions fv
SET references_scanned = FALSE
FROM files f
WHERE fv.file_id = f.id
  AND fv.version = f.current_version
  AND f.format IN ('nx-part', 'solidworks-assembly');
