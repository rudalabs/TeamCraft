# 3D preview of SketchUp models

*2026-11-08*

## Question

Can TeamCraft show a SketchUp model (`.skp`) in the 3D viewer without
SketchUp, the SketchUp SDK or a conversion service?

## Findings

* SketchUp is a polygon modeller: a model is made of planar **faces** bounded
  by loops of **edges** (the first loop is the outer boundary, further loops
  are holes), plus **component definitions** and **instances** of them
  (groups are definitions with a single instance). There is no separate
  display tessellation as in NX or SOLIDWORKS – the faces *are* the geometry
  and only need to be triangulated. Units are inches, Z is up.
* Two unrelated containers exist, both opening with the same UTF-16 header
  (`FF FE FF 0E "SketchUp Model"`, then the version such as `{25.0.575}`):
  * **SketchUp 2021 and later** ("VFF"): a ZIP archive behind the header.
    `model.dat` is a tag–length–value tree (u16 tag, u32 length; about forty
    tags are containers). Vertices (`C409`: id, 3 × f64), edges (`B80B`:
    start and end vertex), faces (`AC0D`: normal, loops of coedges
    referencing an edge and a direction), instances (`6419`: definition id
    and a 13-double transform), definitions (`7C15`) and layers (`8C3C`:
    name, hidden byte). Hide flags and layers sit in a common `D007` record.
    The archive also carries `meta/model_thumbnail.png`.
  * **SketchUp 3 to 2020** ("classic"): one uncompressed MFC `CArchive`
    object stream with a single store map. Objects cannot be skipped without
    knowing their class layout, about 30 classes occur (vertices, edges,
    loops, faces, curves, layers, materials, textures, attributes,
    definitions, instances, groups, images, guides, section planes, text,
    dimensions, line styles), and several field sizes and quirks depend on
    the version.
* **OpenSKP** (github.com/iamahsanmehmood/openskp, MIT) is an open reader and
  writer for both containers with ports in C++, Python, Rust, TypeScript, C#
  and Dart, and a format description (`docs/BINARY_FORMAT.md`, the docstring
  of `legacy.py`). Its Rust port (`packages/rust`, crate `openskp` 1.5.0) has
  no dependencies but is **not** on crates.io. The crate published there
  under the same name (0.5.0, `hew3d/openskp`) is a different project under
  GPL-3.0 and was not used.
* The OpenSKP repository ships 40 `.skp` test files (MIT) from SketchUp 3 to
  2026: 31 classic, 9 VFF, with curves, nested components, scaled instances,
  layers and hidden geometry; several models are saved in more than one
  version.
* A prototype (Python, outside the repository) with its **own VFF decoder**
  and OpenSKP's classic walker loaded all 40 files; 35 contain faces (47 495
  faces, 247 972 triangles), the other 5 are empty templates or hold only
  edges. On the 9 VFF files the own decoder produced exactly the faces,
  instances, definitions and – after triangulation and placement – the same
  world mesh triangle for triangle as OpenSKP. The same model saved in
  several versions (`nesting`, `arcs`, `instance_scale_term`, `legacy_vN`)
  gives identical meshes across both containers.
* **Instance transforms**: `p' = s · (R p + t)` with R's rows
  `(m0 m1 m2) (m3 m4 m5) (m6 m7 m8)`, `t = (m9 m10 m11)` and `s = m12`
  (SketchUp's homogeneous scale term). Verified on the models with skewed
  rotations: with the transposed matrix the window frames of one model fall
  apart, and another model matches its stored thumbnail from the same view.
  A mirrored instance (negative determinant) reverses the triangle order.
* **Triangulation**: a simple ear clipper with a hand-made hole bridge failed
  on about 400 real faces (holes, loops touching themselves). The earcut
  algorithm (mapbox/earcut, ISC) handles them; the triangle area matches the
  loop area within 1e-3 on all but a handful of faces whose loops overlap
  themselves in the source data. Inner loops that overlap each other do
  occur; then the outer loop is triangulated alone and triangles whose
  centre lies in an inner loop are dropped (as OpenSKP does).
* **Visibility**: hidden faces and instances (flag bit 0 of the display
  flags) and everything on a hidden layer are left out, an instance with its
  whole subtree; image definitions (textured rectangles) as well. OpenSKP
  itself reports these flags but does not filter.
* Most SketchUp models are not solids (single faces, open shells, faces
  turned inside out): only 4 of the 35 meshes are closed. The viewer and the
  thumbnail renderer have to draw both sides; closedness and volume are no
  test criteria here.

## Decision

Implement `src/mesh/sketchup.rs`: the VFF container (SketchUp 2021+) with an
own TLV reader on the existing `zip` crate, an earcut triangulation, placement
of nested components, visibility filtering, units converted to millimetres.
`.skp` gets the format id `sketchup` in the 3D preview and a thumbnail rendered
from the mesh. The 40 OpenSKP test files go into `tests/fixtures/cad` with
their MIT licence.

The classic container (SketchUp 3 to 2020) is decided separately: taking over
OpenSKP's MIT Rust walker with attribution, or leaving it out for now. A git
dependency is ruled out by `deny.toml`, and writing an own reader for the
classic stream is not worth it.

Not now: colours, materials and textures, edges without faces, scenes.

## Sources

* OpenSKP, MIT licence, © 2024–2026 Ahsan Mehmood, commit `e7b4674`:
  `docs/BINARY_FORMAT.md`, `packages/python/src/openskp/{_core,legacy,scene}.py`,
  `packages/rust/src/` (read; Python modules run unchanged in the prototype for
  comparison).
* earcut, mapbox, ISC licence (algorithm ported).
* No SketchUp installation, no SketchUp SDK; nothing decompiled.
