# 3D preview of SOLIDWORKS parts and assemblies

*2026-11-07*

## Question

Can TeamCraft show native SOLIDWORKS parts (`.sldprt`) and assemblies (`.sldasm`) in the 3D viewer without SOLIDWORKS,
eDrawings or a
translation service, and with the same quality as the NX preview ([3D preview of NX parts](nx-jt-preview.md))?

## Findings

Files from SOLIDWORKS 2015 on are a sequence of Deflate sections (see
`src/swpart.rs`). Three of them answer the question.

### Parts: the display tessellation

* `Contents/DisplayLists` holds the tessellation SOLIDWORKS draws the part
  with, serialised as an MFC archive (class definitions `ff ff 01 00`,
  class tags with bit 15 set).
    * One `uoTempBodyTessData_c` per body, each followed by its
      `uoTempFaceTessData_c` faces.
    * Each face is a set of plain **triangle strips**. There is no index
      buffer; the triangle order is the usual strip order, counter-clockwise
      around the outward normals.
    * Each face carries three typed arrays, each with a header of
      `element size, type (8 = integer, 100 = float), 2, count`:
        * the strip lengths (`u32`);
        * the vertices (`f32` xyz, metres);
        * a **unit surface normal per vertex** (`f32` xyz).
    * The face header is `n_triangles, n_strips` (SOLIDWORKS 2016/2018); 2024
      adds 32 bytes. Holes with a *shaded cosmetic thread* hold a second mesh
      on the same cylinder, recognisable because it has no face header; it is
      skipped.
* **Tested files:**
    * 57 parts decoded completely, 470 000 triangles: 47 parts of a real
      gearbox project (SOLIDWORKS 2024) and the 10 NIST parts in
      `tests/fixtures/cad` (2016 and 2018).
    * The NIST parts agree with their STEP and SAT exports: ctc-01 has an
      identical bounding box, ftc-11 is within 0.03 mm (chords of a 31.5 mm
      radius).
* **Watertight:**
    * Neighbouring faces do not share vertices, so positions are welded (1 µm). After that 45 of 47 parts are closed.
    * The rest have micrometre cracks where two faces sample their common
      edge differently (T-junctions, helical gear flanks). These are
      invisible in the viewer.
* **Fineness:** the resolution is the document's image quality setting,
  about 15° between neighbouring normals (≈ 24 facets around a cylinder),
  much like the NX lightweight data. Because every vertex carries the exact
  surface normal, the same **PN-triangle refinement** as for NX applies.
  On NIST ftc-11 against the analytic surfaces the deviation drops from
  0.034 mm to 0.004 mm (95 % of the points).

### Assemblies: placements in the component tree

* `swXmlContents/COMPINSTANCETREE` (already read for file references)
  contains:
    * one `swReference` per component instance, with `swTransform`: 16
      numbers `r00 r01 r02 0 r10 r11 r12 0 r20 r21 r22 0 tx ty tz 1`. A point
      lands at `p · R + t` (row vectors, t in metres), the same convention as
      NX;
    * the flags `swSuppressed` and `swHidden`;
    * the referenced model, and through it the file (`swFile swPath`, the
      author's absolute path, so components are found by file name).
* Sub-assemblies are models with their own `swReference` children, placed
  relative to the sub-assembly. Each configuration of the assembly has its
  own top model; `swMostRecentConfiguration="YES"` marks the one that was
  active when the file was saved.
* **Checked against NIST:** the NIST MTC assembly gives the same 36
  instances as its NX version. The positions agree with the NX placements (themselves verified against a JT export)
  within 0.025 mm after one global
  shift of the origin.
* **Checked on the gearbox:** a real gearbox assembly lists 128 instances,
  including two levels of sub-assemblies. Its placed part boxes fill the
  assembly's stored bounding box exactly.
* Suppressed components keep their transform and are always marked hidden
  too. `swHidden` reflects the display state that was active when the file
  was saved.
* Newer assemblies (format 16000 and up) also store copies of their parts'
  meshes (`FaceTessellations/*`, same face records, part coordinates).
  Older ones copy the parts' display lists into their own. Composing from
  the part files, as for NX, works for both and stays current when a part
  changes.

### The exact geometry

* `Contents/Config-N-Partition` is a zlib-wrapped **Parasolid transmit
  file**: neutral binary, schema 13006 plus embedded deltas. A test reader
  parsed all 55 part streams and matched the NIST STEP exports to 1e-15 m.
* A full tessellation of it would be possible: planes, cylinders, cones,
  tori, spheres, rational B-surfaces, swept surfaces and rolling-ball
  blends all occur. With `truck` that is weeks of work, mainly for the
  blends and the tolerant edges.
* The display tessellation with PN refinement is good enough for a
  preview, so the exact geometry is not needed now.

### Licences and law

* No SOLIDWORKS software or SDK was used, and nothing was decompiled. The
  layout was found by analysing files, which EU law permits for
  interoperability (Directive 2009/24/EC Art. 6, UrhG §69e).
* "SOLIDWORKS" and "Parasolid" are trademarks of Dassault Systèmes and
  Siemens; they are named only to say which files are read.
* The files of the gearbox project used for testing are not part of the
  repository. The tests use the openly licensed NIST files only.

## Decision

* `solidworks-part`: read `Contents/DisplayLists`, weld the positions (1 µm) and refine with PN triangles like NX
  (`mesh/sw.rs`).
* `solidworks-assembly`: compose the preview from the part files found by
  name, with the placements and the hidden or suppressed flags of the
  component tree. This is the same pipeline as NX assemblies (`services/assemblies.rs`).
* The thumbnails of SOLIDWORKS parts and assemblies are rendered from the
  3D preview instead of the stored `PreviewPNG`. An assembly none of whose
  parts are in TeamCraft keeps the stored image.
* Not covered:
    * files before 2015 (OLE compound files);
    * virtual components (their geometry lives inside the assembly);
    * the active display state, if it differs from the saved `swHidden`;
    * the Parasolid geometry.
