# Supported file formats

TeamCraft accepts files of any type. The formats below are **recognised**:
labelled with program and kind, searchable by program, delivered with their
registered content type. **3D** marks the formats the preview shows; **refs** the formats scanned for referenced files.
The complete, current
list is on every server at `/formats` and `GET /api/v1/formats`
(source: `src/cadformat.rs`).

## Mechanical CAD (native)

| Program                      | Extensions                                                                                                                   |
|------------------------------|------------------------------------------------------------------------------------------------------------------------------|
| SolidWorks                   | `.sldprt` `.sldasm` (refs) `.slddrw` (refs), templates `.prtdot` `.asmdot` `.drwdot` `.slddrt`, `.sldlfp` `.sldblk`          |
| Siemens NX                   | `.prt` (parts, assemblies and drawings, refs), `.sim` `.fem` `.afm`                                                          |
| Siemens Solid Edge           | `.par` `.psm` `.asm` (refs) `.pwd` `.dft` (refs)                                                                             |
| Autodesk Inventor            | `.ipt` `.iam` (refs) `.idw` (refs) `.ipn` `.ipj`                                                                             |
| Autodesk Fusion              | `.f3d` `.f3z` (refs) `.f2d`                                                                                                  |
| AutoCAD                      | `.dwg` `.dxf` `.dwt`                                                                                                         |
| PTC Creo                     | `.prt` `.asm` (refs) `.drw` (refs) `.frm` `.lay` `.sec` `.mfg` `.neu` `.pvz` `.pvs`; iterations `name.prt.7` become versions |
| CATIA V5                     | `.CATPart` `.CATProduct` (refs) `.CATDrawing` (refs) `.cgr` `.CATAnalysis` `.CATProcess` `.CATMaterial` `.3dxml`             |
| Ansys SpaceClaim / Discovery | `.scdoc`                                                                                                                     |
| FreeCAD, OpenSCAD            | `.FCStd`, `.scad`                                                                                                            |
| SolidCraft, FieldCraft       | `.sc` `.solidcraft`, `.fieldcraft` – see [FieldCraft and SolidCraft](interop.md)                                             |
| Rhino, Alias                 | `.3dm`, `.wire`                                                                                                              |

SketchUp models (`.skp`) of SketchUp 2021 and later are a ZIP archive behind
a UTF-16 header; `src/mesh/sketchup.rs` reads its `model.dat`, a tree of
tag–length–value records (vertices, edges, faces with their loops, component
definitions, instances with a 13-value transform, layers), triangulates every
face with an earcut port (`src/mesh/earcut.rs`, holes and loops touching
themselves included), places nested components and groups (up to 32 levels,
mirrored ones with reversed triangles) and converts inches to millimetres.
Hidden faces and instances, hidden layers and image entities are left out.
Files of SketchUp 3 to 2020 (one MFC object stream) are read by the Rust
port of OpenSKP (MIT), vendored unchanged in `vendor/openskp`, and placed and
triangulated the same way. The thumbnail is rendered from the 3D preview.

NX parts (NX 1980 and later) are read through their stream container (`SPLMSSTR`, `src/nxpart.rs`): the referenced
component files come only from
its reference streams (not from the save history), the preview image from
`/Root/images/preview`, and the 3D preview of parts from
`/Root/FastLoad/JT`: a JT 9 file whose finest level of detail (topologically compressed tri-strip sets,
`src/mesh/jt.rs`) is refined
along its stored surface normals up to 3 × 3 (`src/mesh/pn.rs`, at most
about 500 000 triangles) and shown in millimetres. Assemblies carry no
such stream: their 3D preview is put together from the previews of their
part files (`src/nxasm.rs` reads the components and placements from
`/Root/FastLoad/Structure`, `src/services/assemblies.rs` places the parts).
Components are found by file name, like file references – in the
assembly's revision first, otherwise in the latest revision of another
item – and sub-assemblies are followed up to 8 levels deep; components
that are not in TeamCraft are left out, and so are components on a layer
the assembly's layer settings hide (read from `/Root/UG_PART/UG_PART`), as
in NX. Drawings have no 3D preview.

Stand-alone `.jt` files (Teamcenter visualisation, exports of NX, CATIA,
Creo):

- **JT 10** (`src/mesh/jt10.rs`) carries parts and assembly in one file. The
  scene graph (an LZMA-compressed segment) gives every shape occurrence with
  its transform, the units of the model and the nodes NX marks hidden
  (section and trace-line geometry). Each shape's finest level of detail is
  decoded with the JT 9 topology coder plus the newer integer codecs, its
  hash checked, refined along the stored normals within the same budget as
  NX parts and placed per instance.
- **JT 9** with topologically compressed triangle LODs, as NX writes them,
  goes through the reader of NX parts (no placements).
- Other versions report their version. Not read: files that keep their parts
  in other `.jt` files ("shattered"), colours, PMI and the B-rep segments.

SOLIDWORKS files from 2015 on are read through their section container (`src/swpart.rs`). The 3D preview of a part is
the display tessellation
SOLIDWORKS stores in `Contents/DisplayLists` (`src/mesh/sw.rs`):

- each face is stored as triangle strips with a unit surface normal per
  vertex;
- the per-face vertices are welded on a 1 µm grid;
- the result is refined along the normals like NX parts and shown in
  millimetres.

An assembly is put together from the previews of its part files. The
placements come from the component tree `swXmlContents/COMPINSTANCETREE`
(`swpart::placements`), using the configuration that was active when the
file was saved, with sub-assemblies resolved. Suppressed, hidden and
virtual components are left out, and components are found by the file
name of their stored path. Files saved before 2015 (OLE compound files) and
drawings have no 3D preview.

Rhino models (`.3dm`, versions 1 to 8) are read as openNURBS archives (`src/mesh/rhino.rs`): nested chunks, the object,
layer, block-definition
and settings tables. The 3D preview shows what Rhino stored for display –
mesh objects, the render meshes of polysurfaces (one per face, welded at
1 µm), the mesh cache of extrusions (Rhino 6 and later) and the quantised
meshes of Rhino 1 files – and refines SubD objects, which store only their
control net, by Catmull-Clark subdivision (crease edges and corners kept,
up to about 200 000 quads per SubD) onto the limit surface. Block
instances place the objects of their definition with the stored 4 × 4
transform (nested up to 8 levels). Hidden objects, objects on hidden layers
and block-definition objects themselves are left out; coordinates are
converted from the model unit to millimetres. Polysurfaces, extrusions and
NURBS surfaces saved without render meshes, curves, points, annotations,
linked blocks and colours are not shown; such a model keeps the preview
bitmap Rhino stored as its thumbnail.
Fusion designs (`.f3d`) are ZIP archives whose entries are mostly
compressed with Zstandard (read by `src/fusion.rs` with the pure-Rust
`ruzstd`, sizes capped and checked against the stored CRC-32). Their 3D
preview is the display scene Fusion stores for fast opening
(`OGS.BlobFolder/OGS/DefaultScene/world` and `stream_mesh_NNN`,
`src/mesh/f3d.rs`): the triangles of every body face, welded per body,
refined along their stored normals like NX parts and converted from
centimetres to millimetres. Sketches, edges, dimensions and work geometry
are left out. The thumbnail is rendered from the 3D preview; a file
without a display scene shows `Previews/big.png`, the image Fusion
stored. The exact B-rep (ShapeManager `.smb`) is not read. See
[Research: 3D preview of Fusion designs](../research/fusion-ogs-preview.md).

Inventor parts and assemblies are OLE compound files whose data lives in
zlib-compressed segments below `RSeStorage` (`src/invpart.rs`). The 3D
preview of a part is the display tessellation in `PmGraphicsSegment`
(`src/mesh/inventor.rs`):
- each B-rep face is an indexed triangle list (centimetres) with a unit
  surface normal per vertex;
- the faces of solid bodies are taken (all faces of a part without a
  solid), welded on their exact coordinates and refined along the normals
  like NX parts, and shown in millimetres.

An assembly is put together from the previews of its part and
sub-assembly files (`src/invplace.rs`): the referenced files and the
visibility of each occurrence come from the stream `UFRxDoc`, the
placements from `AmRxSegment`, including the poses a parent gives the
children of flexible sub-assemblies. Hidden and suppressed components are
left out, and components are found by the file name of their stored path.
Not covered: design views other than the saved one, weld beads, drawings
and presentations.

SOLIDWORKS files from 2015 on are no OLE files any more but a sequence of
Deflate-compressed sections (`src/swpart.rs`; verified with SOLIDWORKS 2018
files). TeamCraft inflates only the sections it needs, with a hard size
limit and a CRC check: the preview image from `PreviewPNG`, the component
files of an assembly from its component tree (`swXmlContents/COMPINSTANCETREE`: direct components, without virtual
components and without the old names of renamed parts that the rest of the
file still contains). Files without a component tree (drawings) use the
names in the document manager's section (`SwDocContentMgr/SwDocContentMgrInfo`);
this was not verified with a real drawing. SOLIDWORKS 2014 and older files
are OLE files and are read as before. Assemblies stored with an older
TeamCraft keep the references found then until the next check-in – or run
`teamcraft rescan-references` once (see [Operations](../admin/operations.md)).

**Instance counts.** For SOLIDWORKS 2015+ assemblies (component tree,
instances with *exclude from BOM* not counted), NX assemblies (placed
components, hidden layers left out) and STEP assemblies with external part
files (`NEXT_ASSEMBLY_USAGE_OCCURRENCE` per child whose definition is linked
to a `DOCUMENT_FILE` – through `APPLIED_DOCUMENT_REFERENCE`,
`PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS` or
`DOCUMENT_PRODUCT_EQUIVALENCE`; sub-assemblies inside the file count with
their own occurrences) and Inventor assemblies (the occurrences of each
referenced file from the file descriptor table in `UFRxDoc`, suppressed
occurrences left out – a file with only suppressed ones counts 0, see
[the research note](../research/inventor-instance-counts.md))
the scan also stores how often each referenced file is
placed (`file_references.instances`, `instances` in `GET …/references`); the
desktop folder import uses it as the BOM quantity, *Quantities from CAD…*
corrects existing lines with it. Other formats – Solid Edge, Creo, CATIA, … –
store no count (`null`): their references are found by name only. The current
versions of SOLIDWORKS, NX and referencing STEP files stored before this are
read again once at the next start; for Inventor assemblies stored before run
`teamcraft rescan-references --format inventor-assembly` once (*Quantities
from CAD…* reads the counts from the file and works without it).

`.prt` and `.asm` are told apart by content (Creo's `#UGC:` header, Solid
Edge's OLE container); `.sc` must be a SolidCraft ZIP or JSON design.

## BIM / architecture

| Program              | Extensions                  |
|----------------------|-----------------------------|
| Autodesk Revit       | `.rvt` `.rfa` `.rte` `.rft` |
| Autodesk Navisworks  | `.nwd` `.nwc` `.nwf`        |
| Bentley MicroStation | `.dgn`                      |
| Graphisoft Archicad  | `.pln` `.pla`               |
| SketchUp             | `.skp`                      |
| IFC (open BIM)       | `.ifc` `.ifcxml` `.ifczip`  |

## 3D / DCC / animation

| Program                  | Extensions                               |
|--------------------------|------------------------------------------|
| Blender                  | `.blend`                                 |
| Autodesk Maya            | `.ma` `.mb`                              |
| Autodesk 3ds Max         | `.max`, `.3ds` (3D)                      |
| Cinema 4D, Modo, Houdini | `.c4d`, `.lxo`, `.hip` `.hipnc` `.hiplc` |

## Neutral exchange

| Format                   | Extensions                                    |
|--------------------------|-----------------------------------------------|
| STEP (ISO 10303)         | `.step` `.stp` `.p21` (3D, refs)              |
| IGES                     | `.iges` `.igs`                                |
| Parasolid                | `.x_t` `.xmt_txt` `.x_b` `.xmt_bin`           |
| ACIS                     | `.sat` `.sab`                                 |
| JT (ISO 14306), PLM XML  | `.jt` (3D, refs), `.plmxml`                   |
| OpenCascade BREP, VDA-FS | `.brep` `.brp`, `.vda`                        |
| 3D PDF (U3D / PRC)       | `.pdf` – recognised by its embedded 3D stream |

## Meshes, visualisation, 3D printing

| Format                     | Extensions                                             |
|----------------------------|--------------------------------------------------------|
| STL, OBJ, PLY, OFF         | `.stl` `.obj` `.ply` `.off` (3D)                       |
| 3MF, AMF                   | `.3mf` `.amf` (3D)                                     |
| glTF                       | `.gltf` `.glb` (3D)                                    |
| FBX, COLLADA, USD, Alembic | `.fbx`, `.dae`, `.usd` `.usda` `.usdc` `.usdz`, `.abc` |
| VRML, X3D                  | `.wrl` `.vrml`, `.x3d`                                 |

## Point clouds and scans

| Format             | Extensions                            |
|--------------------|---------------------------------------|
| ASTM E57           | `.e57`                                |
| LAS / LAZ          | `.las` (3D), `.laz`                   |
| PTS, PTX, XYZ, PCD | `.pts` (3D) `.ptx` `.xyz` (3D) `.pcd` |

## Electronics, simulation, manufacturing, documents

KiCad (`.kicad_pcb` `.kicad_sch` `.kicad_pro` `.kicad_sym` `.kicad_mod`,
refs), Altium, Eagle, Gerber and Excellon; OpenRocket, RockSim, RASAero,
motor curves (`.eng` `.rse`); ANSYS Workbench, Abaqus, COMSOL; G-code;
PDF, office documents, CSV, images and archives.

## Simulation models and results (FEA, CFD)

Recognised and versioned (see [Simulations](../guide/simulation.md)). *Model* = setup or solver deck, *result* = solver
output (kind *Simulation
result*). Legacy VTK (`.vtk`, unstructured grid and polydata), VTK XML
unstructured grids and polydata (`.vtu`, `.vtp`) and CalculiX `.frd`
(ASCII) are shown in the 3D viewer, coloured by their result fields; the
other result formats are stored, not parsed.

| Program / format      | Extensions                                                                                                                                                                                                              |
|-----------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ANSYS                 | `.wbpj` (Workbench), `.db` (model), `.rst` `.rth` (results)                                                                                                                                                             |
| Abaqus                | `.cae` `.inp` (model), `.odb` `.sta` `.msg` (results)                                                                                                                                                                   |
| Nastran               | `.bdf` `.nas` `.dat` (input deck), `.op2` `.f06` `.f04` `.pch` (results)                                                                                                                                                |
| OptiStruct            | `.fem` (text input deck)                                                                                                                                                                                                |
| CalculiX              | `.inp` (Abaqus syntax), `.frd` (results)                                                                                                                                                                                |
| LS-DYNA               | `.k` (keyword deck), `d3plot`, `d3plot01`, … `.d3plot` (results)                                                                                                                                                        |
| COMSOL                | `.mph`                                                                                                                                                                                                                  |
| NX / Simcenter        | `.sim` `.fem` `.afm`                                                                                                                                                                                                    |
| OpenFOAM              | `controlDict` (marks the case), `.foam` (ParaView marker file)                                                                                                                                                          |
| Fluent                | `.cas` `.cas.gz` `.cas.h5` (case), `.dat` `.dat.gz` `.dat.h5` `.cdat` (data / results), `.msh` `.msh.gz` `.msh.h5` (mesh), `.flprj` (project), `.jou` (journal), `.trn` (transcript), `report-file-*.out` (report file) |
| STAR-CCM+             | `.sim`, `.simh` (solution history), `.java` (macro)                                                                                                                                                                     |
| VTK / ParaView        | `.vtk` (legacy), `.vtu` `.vtp` `.vts` `.vtr` `.vtm` (XML), `.pvd` (time-step collection)                                                                                                                                |
| CGNS, Exodus II, Gmsh | `.cgns`; `.exo` `.e`; `.msh`                                                                                                                                                                                            |

Extensions that other programs use too are decided by the file content:

* `.sim`, `.fem` and `.afm` starting with NX's `SPLMSSTR` container are NX
  simulation files. Any other `.sim` is a STAR-CCM+ simulation when the
  beginning of the file names STAR-CCM+, otherwise an NX simulation file (older NX releases have no container
  signature). A text `.fem` is an
  OptiStruct/HyperMesh deck, another binary `.fem` and a non-NX `.afm`
  (Adobe font metrics) stay unclassified. The STAR-CCM+ marker is a
  heuristic; it was not verified with files of every STAR-CCM+ version.
* `.dat` counts with Fluent's section syntax (`(0 "…")`, `(1 "…")`) as Fluent
  data, and as a Nastran deck when it is text with a line starting with
  `SOL `, `CEND`, `BEGIN BULK` or `INIT MASTER`, or a `$` comment naming
  Nastran or Simcenter (Simcenter 3D writes its decks as `.dat`); all other
  `.dat` files stay unclassified.
* `.msh` is a Fluent mesh with Fluent's sections (`(0 `, `(1 `, `(2 `,
  `(10 `) and a Gmsh mesh with `$MeshFormat`; otherwise, and by name alone,
  it stays unclassified.
* `.cas.h5`, `.dat.h5` and `.msh.h5` need the HDF5 signature (at offset 0 or
  512), `.cas.gz`, `.dat.gz` and `.msh.gz` the gzip magic. The HDF5 files are
  never opened as HDF5.
* `.out` is a Fluent report file when its first line is a quoted title and
  the second a header of quoted column names (by name alone:
  `report-file-*.out`); `.java` is a STAR-CCM+ macro only when its beginning
  names STAR-CCM+ or imports `star.common`; `.op2` must be binary, `.f04`,
  `.pch`, `.jou` and `.trn` must be text.
* `.e` is Exodus II only as netCDF or HDF5 file; `.db` is not ANSYS when it
  is an SQLite database, an OLE file (`Thumbs.db`) or text; `.rst` text files
  are reStructuredText; `.msg` and `.sta` must be text (Outlook mails are
  OLE files); `.odb` ZIP files are OpenDocument databases; `.bdf` files
  starting with `STARTFONT` are bitmap fonts; `.k` must be text.

Result files are never treated as CAD files referenced by assemblies.

## STEP in the 3D preview

STEP files (AP203, AP214, AP242; `.step`, `.stp`, `.p21`) are previewed from
one of two sources:

* **B-rep** – `CLOSED_SHELL` / `OPEN_SHELL` of `ADVANCED_FACE`s with planes,
  cylinders, cones, spheres, tori, B-spline and Bézier surfaces (also
  rational), extrusions and revolutions, tessellated with the
  [truck](https://github.com/ricosjp/truck) kernel. TeamCraft's own Part 21
  reader hands the kernel only the geometry and topology it understands, in
  a normalised form: PMI, presentation, product and unit data – where AP242
  exporters write empty lists, escaped quotes or binary values that the
  kernel's parser rejects – never reach it.
* **Tessellated geometry** of AP242 (`TRIANGULATED_FACE`,
  `COMPLEX_TRIANGULATED_FACE`, `TRIANGULATED_SURFACE_SET`,
  `COMPLEX_TRIANGULATED_SURFACE_SET` with their `COORDINATES_LIST`) when
  there is no B-rep or it is incomplete – no kernel needed. Tessellated PMI (dimensions and notes drawn as triangles,
  used by an
  `…ANNOTATION…OCCURRENCE`) is not shown.

Each shell is tessellated on its own; when the kernel fails on a face – an
unsupported surface type, broken trimming, or a panic inside the kernel – the
faces are tessellated one by one and only the bad ones are missing. The
viewer then says **partial preview: N of M faces**.

Not applied: length units (the viewer shows model units as in the file) and
assembly placements (`MAPPED_ITEM`, `CONTEXT_DEPENDENT_SHAPE_REPRESENTATION`
– every shell appears in its own coordinates). STEP files above 256 MB are
not previewed; the conversion has a time limit of 180 s and the usual
triangle limit.

## Why not every format is previewed

Most native formats are proprietary and undocumented; reading their geometry
would need the vendors' kernels. TeamCraft stays pure Rust and previews the
open formats. For native CAD files keep a STEP export next to them – it is
previewed, searchable and scanned for references.
