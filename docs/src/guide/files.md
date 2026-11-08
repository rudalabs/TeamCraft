# Files, versions and 3D preview

## Versions and rollback

Every upload of a file name is a new **version** with an optional comment.
The revision page lists the version history of every file; any version can
be downloaded (`name.v2.ext`), previewed, or **restored** – a rollback that
stores the old content as a new version, so history is never rewritten.

**Large result files.** A file can be set to *keep only the latest version*
(*⋯ → Keep only the latest version*, while you hold the check-out): older
versions are then removed after a grace period (default 7 days) as long as the
revision is in work – released revisions keep all of their versions. See
[Data retention](../admin/operations.md#data-retention).

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

| Previewed           | Formats                                                                                                                       |
|---------------------|-------------------------------------------------------------------------------------------------------------------------------|
| B-rep (tessellated) | STEP (`.step`, `.stp`, `.p21`)                                                                                                |
| Native CAD          | NX parts (`.prt`), SOLIDWORKS parts (`.sldprt`, 2015+) – the tessellation stored in the file; their assemblies from the parts |
| Meshes              | STL, OBJ, PLY, OFF, 3MF (incl. components), AMF, glTF / GLB, 3DS                                                              |
| Point clouds        | PTS, XYZ, LAS (uncompressed), PLY and OBJ without faces                                                                       |
| Simulation results  | VTK (`.vtk`), VTU, VTP, CalculiX FRD – coloured by result fields                                                              |

The server converts a file **once**, in a separate process with a time limit,
and caches the result next to the vault; uploads start the conversion in the
background so the first viewer does not wait. Everything is pure Rust: STEP
is read and tessellated with the [truck](https://github.com/ricosjp/truck)
B-rep kernel (or from the tessellated geometry of AP242 files), the mesh
readers are part of TeamCraft. *Export STL* (in the toolbar on the view) saves the shown triangles, e.g.
for 3D printing. The same conversion computes the shape signature of the
[similar part search](similar-parts.md).

When a STEP preview shows only part of the model, the line below the viewer
says **partial preview: N of M faces** – the missing faces use geometry the
kernel cannot tessellate. Details: [STEP in the 3D preview](../reference/formats.md#step-in-the-3d-preview).

Simulation results show the outer surface of the mesh with a colour scale (viridis), the range of the field above the
canvas and, when the file has
several fields, a selector – see [Simulations](simulation.md#results-in-3d).

## Previews of native CAD files

Native CAD files (Inventor, Solid Edge, SolidWorks, NX, Creo, CATIA, …) are
proprietary; TeamCraft does not read their exact geometry (see
[Research: previews of native CAD files](../research/cad-previews.md)).
The exceptions are **NX** and **SOLIDWORKS**. Both save a tessellation of
every part into the file: NX in a documented format (JT 9), SOLIDWORKS (2015 and later) as the display lists it draws
the part with.

TeamCraft shows that tessellation in the 3D viewer, refined along the
exact surface normals stored with it, so round faces stay round.

An **assembly** of either system is put together from its part files.
Check the parts in, under their file names, either in the assembly's
revision or as items of their own. The assembly then shows every component
in its place, sub-assemblies included (see
[Research: 3D preview of NX parts](../research/nx-jt-preview.md) and
[3D preview of SOLIDWORKS files](../research/solidworks-display-preview.md)).

These components are left out:

* components that are not in TeamCraft;
* NX components on layers the assembly hides;
* SOLIDWORKS components that are suppressed or hidden.

For the rest TeamCraft shows what other PLM systems show:

* **A picture of the 3D preview** for NX and SOLIDWORKS parts and
  assemblies. It replaces the image stored in the file and is a 512 × 512
  PNG with a transparent background, drawn once and cached. An assembly's
  picture is drawn again when one of its parts changes. Drawings, and
  assemblies none of whose parts are in TeamCraft, keep the stored image.
* **The preview image stored in the file.** Inventor (`.ipt`, `.iam`,
  `.idw`, `.ipn`), Solid Edge (`.par`, `.psm`, `.asm`, `.pwd`, `.dft`), NX assemblies and drawings (`.prt`, a JPEG) and
  SolidWorks files (up to 2014 in their OLE container, from 2015 on in a
  compressed section that TeamCraft inflates) carry a picture the CAD program wrote when saving. It
  appears next to the file on the revision page and in the desktop app (`GET /api/v1/files/{id}/thumbnail`). Only the
  image is read; files saved
  without one simply show none.
* **3D from a STEP export.** Attach a STEP export next to the native file.
  The desktop app on Windows creates it with your own Inventor or Solid Edge
  – the *STEP* button in the workspace or the option at check-in, like the
  Inventor add-in of Autodesk Vault or the Solid Edge integration of
  Teamcenter. It uses your licence (student licences included, within their
  terms); TeamCraft contains no Autodesk or Siemens software.

## Full-text search

The item search ranks matches in numbers, names, materials, descriptions and
file names, and in the text inside STEP and IGES files (product names,
authors, organisations), plain-text files (with their decimal numbers), the
text layer of PDF reports and the text of HTML reports (table rows as lines;
scripts, styles and hidden elements skipped). PDFs and HTML files are read in an
isolated child process with size, time and memory limits; scanned PDFs have no
text layer (there is no OCR).
