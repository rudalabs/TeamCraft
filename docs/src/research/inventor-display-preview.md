# 3D preview of Inventor parts and assemblies

*2026-11-08*

## Question

Can TeamCraft show native Autodesk Inventor parts (`.ipt`) and assemblies
(`.iam`) in the 3D viewer without Inventor, Inventor View or a
translation service, and with the same quality as the NX and SOLIDWORKS
previews ([3D preview of NX parts](nx-jt-preview.md),
[3D preview of SOLIDWORKS files](solidworks-display-preview.md))?

## Findings

Inventor files are OLE compound files. Below the storage `RSeStorage`, a
document is split into **segments**. `RSeSegInfo` lists them, and each
segment has two streams with the same id:

* `M<id>`, the meta stream:
  * the text `RSe Meta Stream Version 8`, the segment name (UTF-16) and its
    class UUID, then two dates;
  * then a zlib body: the length of every section in the segment, and the
    table of section classes (UUIDs).
* `B<id>`, the data stream: an 18-byte header, then zlib. Inflated, it is a
  flat list of sections:
  `u32 class word | payload | u32 length again | u8 0`.
  The low byte of the class word indexes the class table, and the repeated
  length is a check.

Sections are named after the first 32 bits of their class UUID (for
example `D79AD3F3`). Inside the sections, arrays share one form:
`u32 0x30000002, u32 count, u32 count, u32 element type`, then the
elements. The element type names a class of the table: a point (3 ×
`f32`), a unit vector or a 2D point.

### Parts: the display tessellation

* Every part has one `PmGraphicsSegment`. It is the scene graph Inventor
  draws the part with.
* **One section of class `D79AD3F3` per B-rep face** holds:
  * the owning face (a key);
  * the vertices (`f32` xyz, **centimetres**, part coordinates);
  * a plain `u32` triangle list, counter-clockwise around the outward
    normal;
  * a unit surface normal per vertex;
  * the uv parameters and edge flags, which the preview does not need.
* A simpler class `A79EACD2` has the same arrays without the extras. It is
  used:
  * for the quads of work planes (owned by no face, and left out);
  * for a second triangulation of a face;
  * in a few files, as the only mesh of a face.
* **Bodies:** the faces come in body groups. A record before each group
  tells solid bodies (1) from surface bodies and construction copies (0).
  The preview takes the solid bodies; a part without any shows all its
  surfaces.
* **No transforms:** every body is stored in part coordinates.
* **Watertight without tolerance:** neighbouring faces share their
  boundary vertices bit for bit. Welding on the exact `f32` values closes
  the mesh, whereas welding with a 1 µm tolerance breaks tiny features.
* **Tested files:**
  * all 873 parts of the Autodesk Inventor 2022 sample set (saved with
    Inventor 2019 and 2022);
  * the two NIST parts in `tests/fixtures/cad` (Inventor 2021).
* **Results:**

  | Result | Parts |
  |---|---|
  | Decode failures | 0 |
  | Closed and consistently oriented | 790 |
  | Watertight, but touching themselves (springs, propeller blades, multi-body mould plates) | 18 |
  | Open, because they are true surface models | 9 |
  | Empty: sketches, route and skeleton parts without a body | 56 |

  NIST ctc-01 has the same bounding box as its NX and STEP versions:
  800 × 450 × 150 mm.
* **Fineness and PN refinement:** the resolution is Inventor's display
  quality, similar to SOLIDWORKS. Every vertex carries the exact surface
  normal, so the same **PN-triangle refinement** as for NX and SOLIDWORKS
  applies.
* **Stored preview image:** the summary information
  (`\x05Zrxrt4arFafyu34gYa3l3ohgHg`, property 17) holds a PNG of 180 or
  512 pixels, which TeamCraft already shows.

### Assemblies: occurrences and placements

* An assembly stores **no mesh of its own**. Its `AmGraphicsSegment` holds
  proxies, so the preview is put together from the part files, as for NX
  and SOLIDWORKS.
* **Which files:** the stream `UFRxDoc` holds:
  * the table of referenced files: the author's absolute path, a file id,
    and flags for iPart/iAssembly factories and members;
  * one record per occurrence: occurrence id, file id, title, and 8 flag
    bytes. Bit `0x40` of the last flag byte means *not visible*.
* **Where:** the `AmRxSegment` holds one section `232792BC` per occurrence:
  * a packed 4 × 4 matrix: one `u32` mask says which elements are +1, 0 or
    −1, and the others follow as `f64`;
  * the occurrence id, to join with `UFRxDoc`.

  The matrix uses column vectors (`p' = R·p + t`) and centimetres.
* **Sub-assemblies** are files of their own and are placed relative to
  their parent.
* **Checks across all 249 sample assemblies:**
  * all of them parse;
  * all 7 700 occurrences resolve to a file;
  * 233 of the 243 assemblies that store a bounding box reproduce it
    exactly with the decoded placements. With the transposed rotation only
    40 do, all symmetric cases.
  * The misses are six cable and harness assemblies, whose wires are
    virtual components, and one at 0.9 %.
  * The visibility bit agrees with Inventor's own DWFx exports.
* **Flexible sub-assemblies** (addendum 2026-11-11). A parent re-poses
  children deeper down with override sets (class `F17A4562`): a path of
  occurrence ids from the parent to the sub-assembly, and refs to override
  sections (class `2238DCC5`). Each override holds the child's persistent
  key (offset 120 of its placement tail) and a packed matrix that replaces
  the child's local placement. The overrides of the outermost assembly win.
  Applying them makes the Test Station bounding box exact (1e-8; 7.2 % off
  without), and the Blower pulleys turn. Kind 1 overrides (positional
  representations, with an empty path) are not applied.
* **Suppressed components** (addendum 2026-11-11). Each occurrence record's
  property block holds a name record `01 05 01 00 00 00 05` and, 31 bytes
  after its end, the record `01 07 01 00 00 00 07` whose byte is 1 for a
  suppressed occurrence. That is the layout
  [Instance counts of Inventor assemblies](inventor-instance-counts.md)
  verified with real suppressed components; in the sample set it sits at
  that place for 2 564 of 2 565 occurrences (all with the byte 0).
  Suppressed components are left out.
* **Not decoded:**
  * design views other than the one active when the file was saved
    (`DesignViewSegment`): in the sample set only Buffer Prep Skid shows a
    difference, a room enclosure hidden in its *Default* view;
  * the weld beads of weldments, which are stored in the assembly itself.

### The exact geometry

* `PmBRepSegment` holds an **Autodesk ShapeManager** body (`ASM
  BinaryFile`, a fork of the ACIS SAB format). Its records can be read, but
  tessellating trimmed NURBS, blends and offset surfaces ourselves is weeks
  of work.
* The display tessellation with PN refinement is good enough for a preview,
  so the exact geometry is not needed now.

### Prior art, licences and law

* **InventorLoader** (FreeCAD add-on, GPL-2.0) reads parts through their
  ShapeManager body and does not support assemblies. It confirmed the
  section framing and the UUID naming. No code was taken: TeamCraft is
  MPL-2.0.
* The **Open Design Alliance** SDK is paid. Autodesk's own readers
  (Inventor View / Apprentice) are Windows-only and not redistributable.
* No Autodesk software or SDK was used, and nothing was decompiled. The
  layout was found by analysing files, which EU law permits for
  interoperability (Directive 2009/24/EC Art. 6, UrhG §69e).
* "Inventor" and "ShapeManager" are trademarks of Autodesk; they are named
  only to say which files are read.
* The Autodesk sample set carries no licence that allows redistribution,
  so it is not part of the repository. The tests use the two openly
  licensed NIST parts.
* There is no openly licensed Inventor assembly yet. The assembly reader is
  tested with assemblies built in the test code.

## Decision

* `inventor-part`: read the face meshes of `PmGraphicsSegment` (solid
  bodies, or all faces of a surface-only part), weld them on their exact
  coordinates and refine them with PN triangles like NX and SOLIDWORKS.
* `inventor-assembly`:
  * compose the preview from the part and sub-assembly files, found by file
    name;
  * use the placements of `AmRxSegment`, with the poses a parent gives
    children of flexible sub-assemblies, and leave out the occurrences that
    `UFRxDoc` marks as not visible or suppressed;
  * this is the same pipeline as NX and SOLIDWORKS assemblies
    (`services/assemblies.rs`).
* The thumbnails of Inventor parts and assemblies are rendered from the 3D
  preview instead of the stored PNG. An assembly none of whose parts are in
  TeamCraft keeps the stored image.
* Later, if needed: design views, weld beads and the ShapeManager geometry.

## Addendum 2026-11-12: Zstandard segments (#271)

A user's 44 Inventor parts all failed with *damaged compressed data*. The
files were intact, but every `M<id>` and `B<id>` stream below `RSeStorage`
held a **Zstandard** frame (magic `28 B5 2F FD`, RFC 8878) instead of zlib
(`78 5E`). Which Inventor version started writing Zstandard is not known:
the version could not be read from those files, and every file we have is
zlib – the Autodesk samples (Inventor 2019 and 2022), NIST 2021 and the
NIST STC set saved with Inventor 2024. No openly licensed Inventor file
with Zstandard segments was found.

What changed in `src/invpart.rs`:

* the body of a meta or data stream is Zstandard when the magic stands
  where the zlib body would start (after the `u8` of the meta stream, after
  the 18-byte header of the data stream), zlib when a valid zlib header
  stands there (RFC 1950 check bits), and otherwise the first Zstandard
  magic within 64 bytes before or after that offset;
* the search is there because the header length in front of a Zstandard
  frame could not be checked against real bytes: if newer files changed the
  `u8` flag or the 18-byte header, the frame is still found;
* frames that follow each other directly are read one after the other;
* the output is capped like the zlib output, and the window a frame asks
  for (which the decoder allocates up front) is capped at 32 MiB;
* the decoder is the pure-Rust `ruzstd` (MIT), the same crate the Fusion
  archives use.

Tests repack the NIST parts with Zstandard (every segment, the frame also a
few bytes off) and check that the preview is the same as from the zlib
original; damaged Zstandard streams are errors, never panics. The fuzz
target `invpart` gets a small synthetic seed with Zstandard segments
(`fuzz/seeds/invpart/zstd-segments.ipt`).

Failed conversions are cached, so the preview cache (3), the thumbnail
cache (7) and the shape signatures (3) got new versions: the parts that
failed before are converted again.

Known gap: no real Zstandard file is in the tests. If one turns up under an
open licence, it belongs in `tests/fixtures/cad` with a fixture test, and
the header bytes in front of the frame should be checked against it.
