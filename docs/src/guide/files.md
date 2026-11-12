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

### Preferred CAD program

When a part is kept in several CAD programs – an NX model next to a
SOLIDWORKS and an Inventor copy, say – choose your program under *Account →
Preferred CAD program*. On the revision page, in the detail pane of the vault
explorer and in the Files tab of the desktop app, the native files of the **other** programs then move into a collapsed
group below the list, e.g. *2 files in other CAD formats (SolidWorks, Inventor)*; one click opens it.

* A family is everything one program writes natively: for NX its parts,
  assemblies, drawings and simulation files (`.prt`, `.sim`, `.fem`, `.afm`);
  for Creo also its neutral and Creo View files; for CATIA also CGR and 3D
  XML. The families: SolidWorks, NX, Solid Edge, Inventor, Fusion, AutoCAD (DWG), Creo, CATIA, SpaceClaim, FreeCAD,
  SolidCraft, Rhino, Alias,
  OpenSCAD, Revit, Navisworks, MicroStation, Archicad, SketchUp, Blender,
  Maya, 3ds Max, Cinema 4D, Modo, Houdini, KiCad, Altium and Eagle.
* Neutral and exchange formats are never collapsed: STEP, IGES, Parasolid,
  JT and PLM XML, DXF, meshes (STL, 3MF, 3DS, …), visualisation formats,
  point clouds, IFC, Gerber, simulation decks and results, documents and
  images.
* Only a revision that has at least one file of your program is grouped;
  any other revision lists its files as before.
* It is display only and personal: nothing is hidden from the search,
  downloads, *Pack and Go*, the REST API or other users. The default, *All
  CAD programs alike*, keeps the plain list. The REST API returns the setting
  as `preferred_cad` on `GET /api/v1/me` and each file's family as
  `format_info.family`.

## 3D preview

Meshes, point clouds and STEP models open in a WebGL viewer in the browser
and in the desktop app. Drag to rotate, wheel or pinch to zoom, arrow keys to
turn, double-click to reset; *Z up / Y up* switches the convention. Below the
model, *Section X / Y / Z* cuts it open along a plane: the slider moves the
plane through the model (its position is shown in model units), *flip* keeps
the other side – for a look inside housings and assemblies. *Measure* picks
two points on the model (click, don't drag) and shows their distance and its
X, Y and Z parts in model units; the decimal places (0–6) are remembered in
the browser and apply to the section position too. Measured points are on the
visible surface, so a section shows inner distances such as wall thickness. *Compare with …* (on the preview page, when
there is something to compare)
lays another version of the file – or the file of the same name in another
revision of the item – over the model as a translucent red ghost: red shows
where the other model sticks out, so added and removed material is visible at
a glance; sections and measuring work on both.

*Add note* pins a note to a point of the model for a design review: click
the point, write the note. Notes belong to the shown version of the file,
appear as blue points with a list below the model, and everyone who may read
the file sees them and adds their own – also viewers. A note is removed by
its author or an administrator; adding and removing are recorded in the
revision's history (`annotation.add` / `annotation.remove`). The REST API
has the same notes under `/api/v1/file-versions/{id}/annotations`.

| Previewed           | Formats                                                                                                                       |
|---------------------|-------------------------------------------------------------------------------------------------------------------------------|
| B-rep (tessellated) | STEP (`.step`, `.stp`, `.p21`)                                                                                                |
| Native CAD          | NX parts (`.prt`), SOLIDWORKS parts (`.sldprt`, 2015+), Inventor parts (`.ipt`) – the tessellation stored in the file; their assemblies from the parts |
| Native CAD          | CATIA V5 parts (`.CATPart`) – the visualisation data stored in the file                                                       |
| Native CAD          | Rhino models (`.3dm`) – the meshes stored in the file, SubD objects refined from their control net                            |
| Native CAD          | SketchUp models (`.skp`, SketchUp 3 to today) – their faces, components and groups in place                                   |
| Native CAD          | Creo parts (`.prt.N`) – tessellated from the exact faces stored in the file                                                   |
|                     | Fusion designs (`.f3d`) – the display mesh Fusion stores                                                                      |
| Visualisation       | JT (`.jt`): JT 10 parts and whole assemblies, placed; JT 9 as NX writes it                                                    |
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
The exceptions are **NX**, **SOLIDWORKS**, **Inventor**, **CATIA V5** and
**Creo**. The first four save a tessellation of every part into the file: NX
in a documented format (JT 9), SOLIDWORKS (2015 and later) as the display
lists it draws the part with, Inventor as the triangle lists of its graphics
segment, and CATIA as the visualisation data it draws on screen (see
[Research: 3D preview of CATIA V5 parts](../research/catia-cgr-preview.md)).
Creo stores no tessellation, but its exact faces with every edge already
sampled; TeamCraft triangulates those faces itself – planes, cylinders,
cones, tori, spline, fillet and ruled faces (see
[Research: 3D preview of Creo parts](../research/creo-brep-preview.md)).

TeamCraft shows that tessellation in the 3D viewer, refined along the
exact surface normals stored with it, so round faces stay round.

An **assembly** of NX, SOLIDWORKS or Inventor is put together from its part
files (CATIA products are not previewed yet).
Check the parts in, under their file names, either in the assembly's
revision or as items of their own. The assembly then shows every component
in its place, sub-assemblies included (see
[Research: 3D preview of NX parts](../research/nx-jt-preview.md),
[3D preview of SOLIDWORKS files](../research/solidworks-display-preview.md),
[3D preview of Inventor files](../research/inventor-display-preview.md)).

These components are left out:

* components that are not in TeamCraft;
* NX components on layers the assembly hides;
* SOLIDWORKS components that are suppressed or hidden;
* Inventor components that are hidden or suppressed in the assembly.

**Rhino** models (`.3dm`, Rhino 1 to 8) are shown from the meshes Rhino
saves with them: mesh objects, the render meshes of polysurfaces and
extrusions, block instances in their places; SubD objects are refined
from their control net. Hidden objects and hidden layers are left out. A
model saved without render meshes (*Save Small*, or very old files) has
no 3D preview and shows the picture Rhino stored instead (see
[Research: 3D preview of Rhino models](../research/rhino-3dm-preview.md)).
**Fusion** designs (`.f3d`) carry the triangles Fusion draws on screen;
TeamCraft shows them, refined the same way (see
[Research: 3D preview of Fusion designs](../research/fusion-ogs-preview.md)).

**SketchUp** models (`.skp`, SketchUp 3 to the current version) open in the
3D viewer as well: SketchUp stores plain polygons, so TeamCraft triangulates
the faces (holes included) and places every component and group, nested ones
too, in millimetres. Hidden faces, hidden components and everything on a
hidden tag (layer) are left out; colours and textures are not shown. A file
with only edges and no faces keeps its stored picture (see
[Research: 3D preview of SketchUp models](../research/sketchup-preview.md)).

For the rest TeamCraft shows what other PLM systems show:

* **JT files** (`.jt`, version 10) carry parts and assembly in one file:
  the 3D preview shows every instance in its place, and the file's picture
  is rendered from it (see
  [Research: 3D preview of stand-alone JT files](../research/jt10-preview.md)).
* **A picture of the 3D preview** for NX, SOLIDWORKS and Inventor parts and
  assemblies, CATIA and Creo parts, Rhino and SketchUp models and Fusion
  designs. It replaces the image stored in the file and is a 512 × 512 PNG
  with a transparent background, drawn once and cached. An assembly's
  picture is drawn again when one of its parts changes. Drawings, and
  assemblies none of whose parts are in TeamCraft, keep the stored image.
* **The preview image stored in the file.** Inventor drawings and
  presentations (`.idw`, `.ipn`), Solid Edge (`.par`, `.psm`, `.asm`, `.pwd`, `.dft`), NX assemblies and drawings (`.prt`, a JPEG) and
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
