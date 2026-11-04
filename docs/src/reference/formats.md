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

NX parts (NX 1980 and later) are read through their stream container
(`SPLMSSTR`, `src/nxpart.rs`): the referenced component files come only from
its reference streams (not from the save history), the preview image from
`/Root/images/preview`.

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
| JT (ISO 14306), PLM XML  | `.jt` (refs), `.plmxml`                       |
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

## Why not every format is previewed

Most native formats are proprietary and undocumented; reading their geometry
would need the vendors' kernels. TeamCraft stays pure Rust and previews the
open formats. For native CAD files keep a STEP export next to them – it is
previewed, searchable and scanned for references.
