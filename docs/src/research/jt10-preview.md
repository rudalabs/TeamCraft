# 3D preview of stand-alone JT files (JT 10)

*2026-11-08*

## Question

Can TeamCraft show a stand-alone `.jt` file – the lightweight exchange
format of the automotive supply chain (ISO 14306) – in the 3D viewer,
parts and assemblies, in pure Rust? TeamCraft already reads the JT 9 stream
NX keeps inside its parts ([3D preview of NX parts](nx-jt-preview.md)).

## Findings

* **Specification.** Siemens publishes the *JT File Format Reference*
  free of charge: version 10.0 Rev B (2015) and version 10.5 Rev B (2023).
  Its licence terms allow writing software that reads and displays JT data
  and using the documented data structures for that, with a copyright
  notice; the document itself is not redistributed. Annex B of 10.5 has
  the codec code, Annex C the hash.
* **Test data.** The only openly licensed JT 10 file found is the one
  already in `tests/fixtures/cad/nist/mtc-assembly/nx/` – the NIST box
  assembly exported by NX (`Version 10.5 JT DM 9.8.0.0`): 36 instances of
  8 parts. NIST names JT only in its test reports; the JT2Go samples and the
  JT downloads of robot makers have no clear licence. NIST's STEP files of
  the box, cover and plate (dataset mds2-2146) serve as reference.
* **Container.** As in JT 9 (80-byte version text, TOC, segments), but the
  TOC offset and segment offsets are 64-bit, and segments that are
  compressed as a whole (scene graph, metadata, B-rep, PMI) use
  **LZMA in the `.xz` container** (CRC-64) instead of zlib. Shape LOD
  segments are not compressed as a whole.
* **Mesh.** The *Tri-Strip Set Shape LOD* keeps the JT 9 topology coder
  (dual VF mesh, the same 22 symbol arrays, Deering normals), so
  `mesh/jt.rs` is reused. New are:
  * the *third-generation Int32 compressed data packet*: bitlength fields
    with "nibbler"-coded limits (4-bit nibbles plus a continue bit; run
    widths and lengths in 4-bit fields), a probability context with an
    escape flag per entry, a move-to-front codec (window of 16), and
    out-of-band values **only when the context has an escape entry** –
    this rule and the 4/4 constants are not in the text of the
    specification and were found by checking that the code text is
    consumed exactly;
  * one packet for the upper 32 bits of the 8th attribute-mask context
    (JT 9: two packets of 30 and 4 bits) and the large masks as a plain
    word array;
  * vertex coordinates as raw `f32` bits (Lag-1 predicted) or quantised
    codes; normals as one packet of combined Deering codes
    (octant, sextant, θ, ψ from the top – as the text of the
    specification says; the sample code in its Annex B swaps octant and
    sextant);
  * NX writes two blocks the specification does not list (a length plus a
    constant 16-byte id after the vertex bindings, and five bytes before the
    first packet); the reader recognises them by the length and otherwise
    follows the specification.
* **Validation.** Every LOD carries hashes (Bob Jenkins' lookup2 on the
  decoded symbols). With the layout above the composite topology hash and
  the vertex hash match in 24 of 24 LODs of the NIST file.
* **Scene graph.** The LSG segment holds the nodes (partition, group,
  part, instance, range LOD, shape nodes), property atoms and the property
  table. A shape node points to its LOD segment through the late-loaded
  property `JT_LLPROP_SHAPEIMPL`; transforms are 4 × 4 row-vector
  matrices stored sparsely (a 16-bit mask of the non-identity elements),
  composed `p · A · M` down the graph – the convention TeamCraft already
  uses for NX and SOLIDWORKS. Range LOD child 0 is the finest LOD. The part
  nodes carry `JT_PROP_MEASUREMENT_UNITS` ("Millimeters" in the NIST file),
  section and trace-line geometry is marked `__PLM_PS_Hidden = 1`.
* **Result.** A prototype decoded all 8 parts as closed, consistently
  oriented meshes with a positive volume; the stored normals agree with the
  face normals (mean cosine ≥ 0.99). All 36 placements match NX's own
  (translations within 6e-6 mm, rotations within 4e-6), the part sizes
  match NIST's STEP files (box 152.4 × 101.6 × 73 mm, cover 152.4 × 101.6 ×
  25.05 mm, plate 120 × 71.16 × 5 mm).
* **LZMA in Rust.** [`lzma-rs`](https://github.com/gendx/lzma-rs) is pure
  Rust, MIT licensed and reads `.xz` streams with CRC-64 checks; its output
  is written into a size-capped buffer so a damaged file cannot make it
  allocate more than the preview limit.
* Prior art: `oce-jt`/JT Assistant (LGPL/GPL) only read JT 8 and 9; no code
  was looked at or copied for JT 10.

## Decision

Implement: `src/mesh/jt10.rs` reads stand-alone JT 10 files (format id
`jt`): container with LZMA, scene graph with transforms, units and hidden
nodes, LOD 0 of every tri-strip shape, refined like NX parts and placed per
instance; the hashes are checked. The thumbnail of a `.jt` file is rendered
from its 3D preview. Stand-alone JT 8 and 9 files get a clear message
(no openly licensed test file; their scene graph uses the older element
layout).

Not now: shattered files (a partition that names another `.jt` file),
polygon-set LODs, colours and materials, PMI, the B-rep segments (XT, ULP,
LWPA).

## Addendum 2026-11-12: JT 9.5 assemblies

Until now a stand-alone JT 9 file went to the reader of the JT inside NX
parts, which reads the LOD 0 segments without the scene graph: a JT 9.5
assembly showed its parts piled up at the origin.

* **Format.** The scene graph of JT 9.5 (*JT File Format Reference* 9.5
  Rev A, 2010 = ISO 14306:2012) has the same elements, property atoms and
  property table as JT 10; what differs are field widths: version numbers
  are `I16` (JT 10: `U8`), transform elements `F32` (JT 10: `F64`), Base
  Attribute Data has no *field final flags*, Base Property Atom Data is
  `I16` + `U32`. The TOC has 32-bit offsets (28-byte entries), the scene
  graph is zlib-compressed with the same segment header as JT 10.
* **Checked** on the JT 9.4 streams of all eight NIST NX parts: their scene
  graphs parse with this layout and end exactly at the end of the segment;
  each part has one tri-strip shape node whose late-loaded property points
  to its LOD 0 segment. They carry no unit property (NX writes metres).
* **Test data.** No openly licensed JT 9.5 assembly was found (the JT
  Assistant samples and vendor samples have no clear licence). The tests
  build JT 9.5 files by the reference around the real shape segment of the
  NIST hex nut: several instances with translation, rotation and mirror
  transforms, units, damaged bytes.

Decision: stand-alone JT 9 files with more than one shape occurrence or a
placed one are put together through the scene graph (`Layout::Jt9` in
`src/mesh/jt10.rs`, shapes through the JT 9 reader), in metres unless the
graph names its units. A single part and any file whose scene graph cannot
be read go through the JT 9 reader as before, so their meshes do not
change; the JT inside NX parts is not touched. Known gap: verified only on
synthetic assemblies.

## Sources

* Siemens, *JT File Format Reference*, version 9.5 Rev A (2010),
  version 10.0 Rev B (2015) and version 10.5 Rev B (2023); ISO 14306. JT is a trademark of Siemens; the
  JT data format is © Siemens.
* NIST, *Design, Manufacturing, and Inspection Data for a Box Assembly*
  (NX and JT files in `tests/fixtures/cad/nist/mtc-assembly`) and
  *Reference Knowledge Graphs of STEP and QIF Data for a Three-Part Box
  Assembly* (mds2-2146, STEP reference).
