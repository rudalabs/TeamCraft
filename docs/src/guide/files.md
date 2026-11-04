# Files, versions and 3D preview

## Versions and rollback

Every upload of a file name is a new **version** with an optional comment.
The revision page lists the version history of every file; any version can
be downloaded (`name.v2.ext`), previewed, or **restored** – a rollback that
stores the old content as a new version, so history is never rewritten.

The vault is content-addressed (SHA-256): identical files are stored once,
and `teamcraft vault check --verify-hashes` proves that nothing was altered.

## File formats

TeamCraft stores files of any type and recognises the native and exchange
formats of the common CAD, BIM, 3D and electronics programs – see
[Supported file formats](../reference/formats.md). Recognised files are
labelled with program and kind, get their registered content type, are
searchable by program (*Items → CAD program*) and, where readable, scanned
for the files they reference.

## 3D preview

Meshes, point clouds and STEP models open in a WebGL viewer in the browser
and in the desktop app. Drag to rotate, wheel or pinch to zoom, arrow keys to
turn, double-click to reset; *Z up / Y up* switches the convention.

| Previewed           | Formats                                                          |
|---------------------|------------------------------------------------------------------|
| B-rep (tessellated) | STEP (`.step`, `.stp`, `.p21`)                                   |
| Meshes              | STL, OBJ, PLY, OFF, 3MF (incl. components), AMF, glTF / GLB, 3DS |
| Point clouds        | PTS, XYZ, LAS (uncompressed), PLY and OBJ without faces          |

The server converts a file **once**, in a separate process with a time limit,
and caches the result next to the vault; uploads start the conversion in the
background so the first viewer does not wait. Everything is pure Rust: STEP
is read and tessellated with the [truck](https://github.com/ricosjp/truck)
B-rep kernel, the mesh readers are part of TeamCraft. *Download STL* saves
the shown triangles, e.g. for 3D printing.

Native CAD files (SolidWorks, NX, Creo, CATIA, …) are proprietary – attach a
STEP export next to them to see them in 3D.

## Full-text search

The item search ranks matches in numbers, names, materials, descriptions and
file names, and in the text inside STEP and IGES files (product names,
authors, organisations) and plain-text files.
