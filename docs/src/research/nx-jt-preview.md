# 3D preview of NX parts from their lightweight representation

*2026-11-06*

## Question

Can TeamCraft show a native Siemens NX part (`.prt`) in the 3D viewer
without NX, a Parasolid licence or a translation service?

## Findings

* An NX part is a stream container (`SPLMSSTR`, see `src/nxpart.rs`). Next to
  the NX data (`/Root/UG_PART/UG_PART`, which embeds the exact geometry as a
  zlib-compressed **Parasolid** transmit block) every saved **part** carries
  `/Root/FastLoad/JT`: a short index followed by a complete **JT 9.4** file
  (`Version 9.4 JT DM 10.8.0.1`) with the tessellation NX uses for fast
  loading.
* Assemblies and drawings have no such stream (checked with the NIST
  assembly and with a real project folder: 23 of 27 `.prt` files had one, the
  rest were assemblies, a drawing backup and one part saved without it).
* The JT file holds three levels of detail: one segment of type 7 (LOD 0,
  the finest) and two of type 6. Each is a *Tri-Strip Set Shape LOD* element
  with **topologically compressed** data, the most involved encoding of the
  format: 24 integer packets (codecs bitlength, arithmetic, chopper) drive a
  dual-mesh coder that rebuilds the triangles; coordinates are quantised per
  axis, the per-corner normals use Deering's normal code. Coordinates are
  metres.
* JT 9.5 is published by Siemens as the *JT File Format Reference* and was
  standardised as ISO 14306 (2012); the encoding is documented there.
  Open implementations to compare against: `oce-jt` (C++, LGPL-2.1, an Open
  CASCADE fork), `jt-java`, `JTSharp`. TeamCraft (MPL-2.0) implements the
  algorithms from the format reference and copies no code.
* The Parasolid block cannot be tessellated without a Parasolid kernel
  (commercial); no open reader exists.
* LOD 0 is coarse: 16–30 facets around a cylinder, up to 0.45 mm away from
  the true surface on a 60 mm part. But every corner carries the exact
  surface normal of the B-rep. With curved point-normal triangles
  (Vlachos et al. 2001) and creases handled through the cross product of
  the two face normals, a 3 × 3 refinement brings the deviation on the
  cylinders of a real part from 448 µm to 10 µm (mean 35 µm → 0.6 µm), and
  the mesh stays closed.
* A prototype decoded all NX parts of the NIST fixtures and of a real
  project folder (23 parts, 128 to 9 760 triangles): every mesh is closed
  (each edge shared by two triangles), consistently oriented, with a positive
  volume; the normals all agree with the triangle orientation.

## Decision

Implement: `src/mesh/jt.rs` reads LOD 0 of the JT stream of NX parts
(JT 9.x only), `src/mesh/pn.rs` refines it along the normals up to 3 × 3
(about 500 000 triangles at most). NX parts get the format id `nx-part` in
the 3D preview; assemblies and drawings keep their preview image.

Not now: stand-alone `.jt` files (the NIST assembly's is JT 10.5 with a
different element layout and a scene graph with transforms), assemblies
(their components' meshes placed by the assembly), the Parasolid block.

## Sources

* Siemens, *JT File Format Reference*, version 9.5 (2010), and ISO 14306:2012
  *Industrial automation systems and integration – JT file format
  specification for 3D visualization*.
* A. Vlachos, J. Peters, C. Boyd, J. L. Mitchell, *Curved PN Triangles*,
  ACM Symposium on Interactive 3D Graphics, 2001.
* [oce-jt](https://github.com/cbsghost/oce-jt), [jt-java](https://ddprojects.chickenkiller.com/corubolo/jt-java),
  [JTSharp](https://github.com/viachpaliy/JTSharp) – compared against only.
* NIST CAD test files in `tests/fixtures/cad/nist` (see
  [Openly licensed CAD files](cad-test-files.md)).
