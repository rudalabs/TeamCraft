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
different element layout and a scene graph with transforms), the Parasolid
block.

## Addendum 2026-11-07: assemblies and thumbnails

The JPEG NX stores in a file is small (about 240 × 180 px) and poor for
assemblies. NX parts therefore get a thumbnail rendered from their 3D
preview, and assemblies a 3D preview put together from their parts:

* An NX assembly has no `/Root/FastLoad/JT` of its own. Its
  `/Root/FastLoad/Structure` stream is an NX object serialisation ("OM":
  header, object table of offsets and tags). The largest object holds the
  placement records (two rotation rows and the translation in mm, numbers
  as a tag byte plus a shortened big-endian double, all divided by 4), the
  component names (the file names without `.prt`, upper case) and the name
  index of every instance. Another object is a hash map from instance to
  record; instances without a record sit at the origin.
* Verified against the JT 10.5 export of the NIST assembly (scene graph,
  property table and transform attributes decoded for this): all 36
  instances match in part, rotation and translation (differences below
  0.00001 mm). A real assembly with two levels of sub-assemblies (a pintle
  injector, 59 placed parts, 0.63 million triangles) renders correctly.
* Sub-assemblies keep their placements in their own file (same format);
  placements compose as row vectors (`p · R + t`).
* The component files are found by file name like file references; their
  saved folders are in `/Root/FastLoad/ExternalReferences`, but inside
  TeamCraft the name match is what counts.
* Two of three exploration agents worked out the mapping independently
  (one from the JT ground truth, one from the `.prt` alone); a third ruled
  out the other streams (`RMFastLoad`: attributes and mass properties,
  `UG_PART`: the same transforms keyed by internal object maps).

Decision: `src/nxasm.rs` reads the components, `services::assemblies`
composes the preview from the parts' previews (cache key = content hashes
of everything used), the thumbnail of parts and assemblies is drawn from the
3D preview (`mesh/render.rs`). Not known yet: NX's "hidden component"
flags (NX's own picture may leave out components the preview shows) and
units of assemblies modelled in inches (all files seen are in mm).

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
