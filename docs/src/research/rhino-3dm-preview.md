# 3D preview of Rhino models (`.3dm`)

*2026-11-08*

## Question

Can TeamCraft show a Rhino model (`.3dm`) in the 3D viewer without Rhino,
and what does it take?

## Findings

* The `.3dm` format is publicly documented through McNeel's openNURBS
  toolkit (C++ source on GitHub): a 32-byte header with the version
  (1, 2, 3, 4, 5, 50, 60, 70, 80), then nested chunks (`u32` type code and a
  32-bit length, 64-bit from version 50; short chunks carry their value in
  the length field; chunks whose type code has bit `0x8000` end in a CRC-32).
  Tables (properties, settings, layers, block definitions, objects) are
  sequences of records; every object is a class-tagged record (class UUID +
  data) followed by its attributes.
* What is stored for display:
  * **Mesh objects** – the mesh itself (float vertices, quads/triangles;
    vertex arrays zlib-compressed with a CRC).
  * **Polysurfaces (B-reps)** – one *render mesh* per face, saved after the
    exact geometry, when the file was saved with display data (Rhino 4 and
    later do this by default; "Save Small" and files written by scripts or
    the openNURBS examples do not). Most Rhino 2/3 files have none.
  * **Extrusions** – a mesh cache since Rhino 6 (user data in Rhino 5).
  * **SubD objects** (Rhino 7, 8) – never a mesh, only the control net
    (vertices with tags, edges with crease tags, faces as edge loops).
* **Blocks** are the assemblies of Rhino: a block definition lists the ids
  of its objects (stored in the object table, flagged as definition
  objects); a block instance holds the definition id and a 4 × 4 transform
  (column vectors, `p' = M p`). Instances nest.
* Visibility: objects can be hidden themselves or through their layer.
* A prototype read all 165 openNURBS and rhino3dm example files (Rhino 1
  to 8) without an error. 104 contain surface geometry; 55 of them give a
  preview, the other 49 have no stored meshes. All 317 meshes read are
  identical to what McNeel's own `rhino3dm` library returns (vertices and
  faces); units and object attributes agree too. 222 of 261 B-reps with
  render meshes are closed after welding coincident edge vertices.
* SubD: Catmull-Clark subdivision of the control net with creases and
  corners, then projection onto the limit surface, reproduces the shape
  of the NURBS version of the same model.
* B-reps without render meshes could be tessellated with the `truck` kernel
  TeamCraft already uses for STEP (its STEP curve and surface types cover
  NURBS), but that needs the whole B-rep topology and every curve and
  surface class of openNURBS; left for later.

## Decision

Implement `src/mesh/rhino.rs` with the format id `rhino`: mesh objects,
B-rep render meshes, extrusion mesh caches, SubD control nets (subdivided),
block instances with their transforms, hidden objects and layers left out,
model units converted to millimetres. Thumbnails are rendered from the 3D
preview; a file without any stored mesh keeps no preview.

Not now: B-reps and extrusions without stored meshes (truck), free-standing
NURBS surfaces, linked blocks (external files), colours and materials.

## Licences

* openNURBS: the licence file states McNeel's copyright and a disclaimer but
  grants no explicit right to modify and redistribute, so the sources were
  only read; TeamCraft's reader is written from the format description and
  copies no code. File formats themselves are not copyrighted.
* `rhino3dm` (MIT) served as a reference in a local prototype only; it is
  not a dependency.
* Test fixtures: the twelve models of `rhino3dm/tests/models` are MIT
  licensed and may be committed with their notice; the openNURBS example
  files have no clear licence and are only used locally.
* "Rhinoceros", "Rhino" and "openNURBS" are trademarks of Robert McNeel &
  Associates and are named descriptively only.

## Sources

* [openNURBS](https://github.com/mcneel/opennurbs) – `opennurbs_3dm.h`,
  `opennurbs_archive.cpp`, `opennurbs_mesh.cpp`, `opennurbs_brep_io.cpp`,
  `opennurbs_beam.cpp`, `opennurbs_subd_archive.cpp`,
  `opennurbs_instance.cpp`, `opennurbs_3dm_attributes.cpp` (read only).
* [rhino3dm](https://github.com/mcneel/rhino3dm) (MIT) – reference library and
  test models.
* E. Catmull, J. Clark, *Recursively generated B-spline surfaces on
  arbitrary topological meshes*, Computer-Aided Design 10(6), 1978;
  H. Hoppe et al., *Piecewise smooth surface reconstruction*, SIGGRAPH 1994
  (crease rules).
