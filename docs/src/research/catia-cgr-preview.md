# 3D preview of CATIA V5 parts from their visualisation data

*2026-11-08*

## Question

Can TeamCraft show a native CATIA V5 part (`.CATPart`) in the 3D viewer
without CATIA, a CGM licence or a translation service, the way it shows NX
parts from their JT stream?

## Findings

* A CATIA V5 document is a **`V5_CFV2` container**: the magic `V5_CFV2\0`,
  then the offset and length of the directory (u32, big-endian; offset +
  length = file size). The stream data is packed from byte 0x40 on. The
  directory starts with `CATIA_V5 CB0001` and lists every entry:
  * a 76-byte block (parent id, type 1 = directory or 2 = stream, size,
    timestamps);
  * the name in UTF-16BE with its length in front;
  * a record with the id and the segments (absolute offset, length).
* Every container of the document (`CATPrtCont`, `CATFeatCont`, the B-rep in
  `CATBRepModeContainer` and `CGMGeom`, …) is a stream named `Cont_<id>`.
  Its `CATStorageProperty` stream says which one it is.
* The container **`CATCGRCont`** holds the tessellation CATIA draws on
  screen. It is stored as a *complete `V5_CFV2` document nested in the
  file*, with offsets relative to its own start. The mesh is in its stream
  `SceneGraph/MainDataStream`; `SceneGraph/SurfacicReps` only holds the
  bounding boxes of the faces.
* **Nothing is compressed.** The `78 01` byte pairs that look like zlib
  headers are coincidences inside float data; none of them inflates.
* **The mesh** is one record per body, behind the marker `FF FF 02 00 01`
  (after the bounding sphere of the body). Counts are a byte, or `FF`
  followed by a little-endian u32. The record has three blocks:
  * **Vertices** in millimetres, part coordinates, sorted by (x, y, z) and
    stored with prefix sharing. A 2-bit code per vertex says how many of the
    last coordinates of the previous vertex are replaced: x y z, y z, z, or
    none (a copy where the normal changes).
  * **Normals** of the curved faces. Per normal, two components are stored
    as int16 (× 1/32767) and the largest one is derived, with a 4-bit code
    for its axis and sign. Normals along an axis are a code only.
  * **One record per B-rep face.** Each lists its triangles as a list,
    strips and fans, indexes the vertices (u8 below 256 vertices, otherwise
    big-endian u16), and carries one face normal for planar faces. The
    record also holds the chordal tolerance CATIA tessellated with (0.2 mm
    by default, 0.01 mm in the NIST file).
* The triangles are counter-clockwise seen from outside. The vertex normals
  are the exact surface normals of the B-rep, so the mesh can be refined
  along the true curvature exactly like the JT data of NX parts (curved PN
  triangles, `src/mesh/pn.rs`).
* Mirrored parts ("Symmetry of …") carry no transform: the mirrored geometry
  is stored in the vertices.
* **Products** (`.CATProduct`) and **drawings** have no `CATCGRCont`; a
  product's preview has to be put together from its parts, as for NX
  assemblies.
* No public description of the CGR encoding was found (one web search; the
  results are listed under *Sources*). Dassault's own readers (CATIA, the
  3DEXPERIENCE viewer) are proprietary, and no open implementation is known.
  Everything here was read off sample files: no CATIA software, SDK,
  documentation or third-party code was used.

## Verification

* **NIST CTC-01** (`tests/fixtures/cad/nist/ctc-01/catia-v5/`, V5-6R2019):
  6 932 triangles on 117 faces. That is the same number of faces as the
  117 `ADVANCED_FACE`s of the AP242 STEP export. The mesh's bounding box
  (-400, -225, -100)..(400, 225, 50) is the vertex box of the STEP file
  exactly. The mesh is closed and its volume is positive.
* **24 parts of a real hobby project** (V5R21: pipes, connectors, a frame, a
  bucket, mirrored copies), used locally only and not committed:
  * all decode, and the renders match the drawings (lengths and diameters
    within the 0.2 mm tolerance);
  * 23 are closed. The 24th has cracks of less than the tolerance between a
    finely tessellated sliver face and its neighbours. That is CATIA's own
    tessellation, not a decoding error.
* Two independent decoders, one following the container directory and one
  searching the raw bytes for vertex and index arrays, arrived at the same
  layout.

## Decision

* **Implement** a reader for the CGR mesh in pure Rust (`src/mesh/catia.rs`):
  * find the nested `V5_CFV2` document and decode each mesh record behind
    its marker;
  * check every count against the bytes that are left and every index
    against the vertex count, and skip records that do not add up (other
    scene graph nodes share the marker);
  * weld the corners that differ only by their normal, refine along the
    stored normals (PN triangles, at most 3 × 3, 500 000 triangles), and
    work in millimetres.
* The thumbnail of a CATIA part is rendered from the 3D preview, as for NX
  parts (`thumbnail::rendered`).
* **Products** are put together from their part files like NX assemblies
  (see [3D preview of CATIA V5 products](catia-product-structure.md)).
* **Later:** multi-body parts, bodies with a transform in the scene graph, and indices
  above 65 535 vertices are untested.
* **Never:** the exact B-rep (CGM). It is undocumented and would need a
  geometry kernel. The visualisation data is what other PLM viewers show as
  well.
* Parts saved without visualisation data (an empty `CATCGRCont`) get no 3D
  preview and say so.

## Sources

* Sample files: NIST *MBE PMI Validation and Conformance Testing* models
  (public domain, already in `tests/fixtures/cad`). The hobby project files
  were used locally only.
* Web search for a CGR or `V5_CFV2` format description (2026-11-08). It
  returned only user documentation, which confirms that CGR files hold the
  graphical representation (vertices and triangles) for viewing, and no
  format specification:
  * <https://grabcad.com/questions/cgr-converting>
  * [Windchill Workgroup Manager for CATIA V5: CGR](https://support.ptc.com/help/windchill/r13.1.2.0/en/Windchill_Help_Center/WWGMforCatiaV5/WGMCATIAV5AdvTechCGR.html)
  * [SOLIDWORKS help: CATIA graphics files](https://help.solidworks.com/2013/English/SOLIDWORKS/Sldworks/t_CATIA_Graphics_Files.htm)
* Prior work in TeamCraft: [3D preview of NX parts](nx-jt-preview.md) (PN
  refinement, thumbnails rendered from the mesh).
