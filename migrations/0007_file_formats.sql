-- File formats of the CAD programs (see src/cadformat.rs): SolidWorks,
-- NX / Teamcenter, Solid Edge, Inventor, Fusion, AutoCAD, Creo, CATIA, the
-- neutral formats (STEP, IGES, Parasolid, JT, ACIS, STL) and more.
--
-- NULL = not classified yet: TeamCraft classifies such files at startup
-- (from the name and, for ambiguous extensions such as .prt and .asm, the
-- first bytes of the content). '' = unknown format. A later release that
-- learns new formats re-classifies the unknown ones with
-- `UPDATE files SET format = NULL WHERE format = ''`.
ALTER TABLE files ADD COLUMN format TEXT;
CREATE INDEX files_format_idx ON files (format);
