# 3D preview of Fusion designs from their display mesh

*2026-11-08*

## Question

Can TeamCraft show an Autodesk Fusion design (`.f3d`) in the 3D viewer
without Fusion, an Autodesk account or a translation service?

## Findings

* A `.f3d` file is a **ZIP archive**. Most entries are compressed with **Zstandard** (ZIP method 93); a few small ones
  are stored. Common ZIP
  tools that know only Deflate refuse these entries. The archive holds one
  folder per document (`FusionAssetName[Active]/…`) with:
    * `Breps.BlobParts/BREP.<uuid>.smb` / `.smbh`: the exact geometry as
      binary **Autodesk ShapeManager** save files (ASM, the ACIS-derived
      kernel; `.smbh` with history). Tessellating them needs a B-rep kernel.
      `truck` cannot read them, and no open kernel does.
    * `FusionDesignSegmentType1/…`: the design (timeline, sketches, features).
    * `Previews/small.png`, `Previews/big.png`: thumbnails Fusion rendered when
      saving.
    * `OGS.BlobFolder/OGS/DefaultScene/world` and `stream_mesh_000`: the **display scene** of Fusion's graphics system
      (OGS). Fusion stores it so
      that the design opens quickly. It holds the triangles Fusion draws for
      every face of every body.
* `world` is a binary serialisation of the scene graph. Class names are
  stored as `u32 length` + UTF-16LE: `GroupNode`, `GeometryNode`,
  `OgsSimpleMesh`, `TransformAttribute`, `VisibilityAttribute`, and the
  Fusion client types `Component`, `Instance`, `Body`, `Faces`, `Face`,
  `Edges`, `Edge`, `ProfileFace`, `SketchCurve` and so on. Shared objects
  are written once and then referenced by an id string (a pointer value
  printed as 16 hex digits).
* An `OgsSimpleMesh` record:

  ```text
  "OgsSimpleMesh"  6 × f32 bounding box, u32 primitive type (4 = triangle list,
                   2/3 = lines, 5 = points), 3 bytes flags
  "VertexFormat"   id string; on first use: u32 count, count × 24 bytes
                   (u32 type: 1 = float2, 2 = float3; u32 semantic: 0 position,
                   1 normal, 2 uv, 3/4 tangent/binormal; u32 offset in floats; 3 × u32 0)
  vertex buffer    u32 1, u32 stream number, u32 offset, u32 byte length
  u8               1 = an index buffer follows
  "AIndexBuffer"   id string, u32 stream number, u8 0, u32 offset, u32 byte length
  ```

  The offsets point into `stream_mesh_NNN`, which holds the raw buffers:
  interleaved little-endian `f32` vertices (position, normal, uv, …) and
  `u32` triangle indices. The buffers follow each other without gaps.
* Coordinates are **centimetres** (Fusion's internal unit). The meshes of a
  body's faces share their boundary points bit for bit, so the faces weld
  into a closed surface. Every vertex carries the exact surface normal of
  its face.
* The sample (a mechanical key switch: 5 bodies with 179
  faces in all) was compared with the STL and STEP files Fusion exported from the
  same design:

  |                          | display mesh of the `.f3d` | STL export of Fusion  |
  |--------------------------|----------------------------|-----------------------|
  | triangles                | 1,108                      | 4,490                 |
  | open edges after welding | 0                          | 0                     |
  | connected shells         | 4                          | 4                     |
  | volume                   | 0.98459 cm³                | 0.98485 cm³ (−0.03 %) |
  | bounding box             | identical                  | identical             |

  The STEP export has the same box in millimetres (14.8 × 14.8 × 15.4 mm).
  The display mesh is coarser than an export, but it has exact normals, so
  the PN-triangle refinement of the NX preview (see
  [3D preview of NX parts](nx-jt-preview.md)) applies unchanged.
* Each body appears twice in the scene: once without meshes and once with
  the `Face` meshes. Edges, sketch profiles, sketch curves, dimensions,
  constraints, work planes and origin geometry are separate meshes under
  their own client types and are left out.
* Instances carry a `TransformAttribute`: a 4 × 4 `f32` matrix, row-major,
  with the translation in the last row (`p' = p · M`). In the sample every
  instance and body transform is the identity, because the components are
  modelled in place.
* Prior art: `cadmpeg-codec-f3d` (Rust, Apache-2.0, clean-room) decodes the
  ZIP container, the ASM B-rep streams and the design records of `.f3d`
  files. It does not mention the OGS display scene. TeamCraft reads only the
  display scene and copies no code. The Zstandard decoder is `ruzstd`
  (pure Rust, MIT).

## Decision

* Read the display mesh: open the ZIP, inflate `world` and the
  `stream_mesh_*` entries with `ruzstd` (sizes capped), and take the
  triangle meshes whose client type is `Face`. Weld them per body and
  refine them along their normals like NX parts. Convert centimetres to
  millimetres.
* Render the thumbnail from the 3D preview, as for NX. Fall back to
  `Previews/big.png` when the file has no display scene.
* The B-rep (`.smb`) stays unread: the display mesh already shows what
  Fusion shows.

## Open points

* **Instance transforms.** The only sample places every component with the
  identity. An assembly whose components are moved, or that uses one
  component several times, is needed to confirm where the `Instance`
  transform applies. Until then the meshes are taken as stored.
* **Hidden bodies and components.** It is not yet known whether Fusion
  writes hidden bodies into the display scene, or how their
  `VisibilityAttribute` is set.
* **`.f3z`** (a design with its referenced designs) is a ZIP of `.f3d`
  files; it can be read once assemblies are confirmed.
* Files saved without a display scene (if Fusion writes such files) get
  the stored PNG.
