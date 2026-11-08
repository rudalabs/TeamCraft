# Creo parts: a 3D preview from the stored B-rep

*Research notes, 2026-11-08. Findings from reading the files only – no PTC
software, no toolkit, nothing decompiled.*

## Summary

PTC Creo part files (`name.prt.N`) store **no display tessellation** of the
body. Unlike NX (JT stream), SOLIDWORKS (display lists) or Inventor
(graphics segments), the only geometry in the file is the exact B-rep:
analytic surfaces with their contours, and every edge sampled as a short
point list **in the parameter spaces of both neighbouring faces**. That
sampling is what makes a preview possible without a geometry kernel: the
face boundaries come ready-made, each face is ear-clipped in its parameter
plane, mapped to 3D, welded with its neighbours and refined along the
analytic normals (`mesh/pn.rs`, as for NX and SOLIDWORKS).

Status: `src/mesh/creo/` reads planes, cylinders, cones and tori. The NIST
test part `ctc-01` (Creo 10) gives a closed mesh whose bounding box is the
STEP export's to 0.05 mm. Spline, fillet and ruled surfaces are not yet
evaluated (see *Gaps*).

## Container

Text header, then a table of contents in fixed-width text records:

```text
#UGC:2 PART 2149 1860 800 1 1 15 4100 2023294 00000002cc \    last word: base offset (hex)
#- VERS … \  #- HOST … \  #- CMNM <name> \                    more header lines
#-END_OF_UGC_HEADER
#Creo  TM  10.0  (c) 2023 by PTC Inc. …
#UGC_TOC 2 32 81 17####…                                       32 entries of 81 bytes
<name> <off> <size> <usize> <ver> <kind> <x> <crc> <crc>      hex, '#'-padded
NEXT_TOC_ENTRY <off> …                                         chain to the next table
```

A section starts with the line `#<name>` at `base + off` and is `size` bytes
long. The *kind* digit tells the storage: `9` plain tagged data, `a`/`12`
Unix `compress` (LZW, `1f 9d`, 9–16 bit codes, no block mode in the files
seen), `11` binary (JPEG previews, `DIV-OL07BL` graphics), `b` scrambled,
`c` an LZW variant (only `DispDataTable`, not decoded). The inflated size is
`usize`.

Sections that matter: `VisibGeom` (visible B-rep), `NovisGeom`
(construction geometry, not shown), `AllFeatur`/`FeatDefs` (feature tree),
`MdlRefInfo` (references – useful for assemblies later), `THMB_IMG_MAIN`
(JPEG, not used as the thumbnail is rendered). `SolidPrimdata`,
`SolidPersistTable` and `CGFPersistTable` contain tri-strips, but only for
the PMI annotations (text, symbols, fonts), never for the body.

## The tagged format

A section is a stream of values whose schema is written on first use. Field
*N* is defined by `e0 TT name 00` (type TT; in order of appearance), its
value follows. The writer remembers, for every field, the transition that
followed it last time – how many struct levels close and which field comes
next – and writes only what differs:

| Token | Meaning |
|---|---|
| `e0 TT name 00` | field definition, then its value |
| `f1` `f2` `f3`, `f4 N` | close 1, 2, 3, N struct levels; `f0` = explicitly none |
| `f7 NN` | the next field is number NN |
| `e1` | null; `e2`/`e3` a struct begins |
| `f8 N`, `f9 R C`, `fa K d1…dK` | array of N, R×C, K dimensions (remembered per field); `fc R` reuses the column count |
| `f8 N f7 NN fb` | array of N structs, the elements follow |
| `e4`–`e6`, `e7 N` | repeat the previous array element 1–3 or N times |

Types: `00` struct, `01 05 07 08 09 0b` integers, `02` double, `03`/`0a`
C string, `04` byte, `06` float32-precision number. Integers: `00`–`7f`,
`80`–`bf` + 1 byte, `c0`–`df` + 2 bytes, `e8`–`eb` + 1–4 bytes, `f5 <int>`
negated, `f6` = −1.

Doubles are IEEE-754 big-endian values with a compressed top:

* one-byte constants: `0f` 1, `10` −1, `11` π, `12` π/2, `13` 2π, `14`
  3π/2, `15` −π, `16` −π/2, `18` 0, `0d` 0.25, `0e` 0.5, `0c` 0.1, `0b`
  0.06, `04` 1−ε, `03` 0.125 (`05`–`0a` unknown);
* near-constants `00`/`01`/`02` + constant token + 1/2/3 bytes that replace
  the lowest bytes of that constant (e.g. `00 14 d4` = 3π/2 + 2 ulp);
* group tokens `19`–`4a`: ten groups of five, the group fixes the top byte
  (`3c 3d 3e 3f 40 bc bd be bf c0`), the sub-type the payload (7 bytes;
  2 bytes + the last byte repeated; 2 bytes + zeros; 3 bytes; 6 bytes);
* class tokens `4b`–`df`: the top 16 bits from a fixed table
  (`tagged::double::class_top`), then 6 mantissa bytes;
* escapes `e8`–`ef` with explicit bytes (`ed` = all eight).

With this, 99.9 % of the edge samples of the 17 NIST parts land on the same
3D point from both neighbouring faces (chord check, tolerance 1e-5).

## The B-rep in `VisibGeom`

```text
Sld_VisGeom / active_geom
  srf_array[]   geom_id, geom_type (34 plane, 36 cylinder, 37 cone, 38 torus, 40 spline,
                41 fillet, 42 ruled), orient (±1), envlp (uv box), outline (3D box),
                srf_prim_ptr(<kind>) { local_sys[4][3], radius | half_angle | radius1, radius2 | … },
                frst_cntr_ptr / next_cntr_ptr { frst_cntr_crv_hdr_ptr, trv }, topol_ref_data { list_id, list_type }
  crv_array[]   crv_id, type, crv_pnt_dir[2], crv_pnt_arr[n][4] = (u0 v0 u1 v1),
                crv_hdr_geom_ptr[0/1], next_crv_hdr_ptr[0/1]
  body_array[]  body_id, outline, first_geom_ptr, mat_name_ptr …
```

* `local_sys`: rows 0–2 are the rotation **by columns** (X, Y, Z), row 3 the
  origin. All 2 492 frames of the test parts are right-handed and
  orthonormal.
* Plane: `O + u·X + v·Y`. Cylinder: `O + |r|(cos u·X + sin u·Y) + v·Z`.
  Cone: radius `v·tan(half_angle)`. Torus: `O + (R1 + r cos v)(cos u·X +
  sin u·Y) + r sin v·Z`. Which sign each parameter has, whether an angle
  starts at 0 or π, and which torus angle is `u` is **not stored**; it is
  resolved by walking from planes with the identity frame across shared
  edges and keeping the variant whose points coincide with the neighbour's
  (`brep::resolve`).
* Edges: `crv_pnt_arr` holds the same points in both faces' parameter
  spaces (row *i* of both sides is one 3D point); `crv_pnt_dir` gives the
  direction per side, `next_crv_hdr_ptr` the next edge of the contour on
  that side. The sampling is the display resolution (about 18° on circles).
* Contours: `trv = 1` is an outer contour, `trv = −1` a hole. One surface
  record may carry **several regions** (ctc-01: the four feet of the plate
  share one plane record, each with its own outer contour). A hole belongs
  to the region that contains its first point.
* Faces of the solid body have `topol_ref_data.list_type = 98` (`list_id` =
  body id). Surfaces with `list_id = −1` are loose or cosmetic and are not
  meshed.
* **Twins:** a periodic face is split in two halves of which only one has a
  record; the other is referenced by edges only (ctc-01: 212 beside 210).
  It shares the primitive of the known curved face behind a straight seam
  edge (type 1 or 8, constant *u*) and is rebuilt from the edges that
  reference it.
* Normals: triangles counter-clockwise in the parameter plane have the
  normal ∂u×∂v; `orient = −1` turns it. The signed volume of the whole mesh
  decides the global sense.

## Verification

* 28 NIST parts (22 FTC/CTC in Creo 4 and Creo 10, 5 STC, 1 HTC); the FTC
  and CTC definitions come with STEP exports.
* `ctc-01`: 145 body faces (8 twins), 151 regions, closed (0 open edges),
  bounding box equal to the STEP export, volume 14.7·10⁶ mm³; the PN
  refinement keeps the mesh closed.
* `ctc-02`, `ctc-03`, `ctc-04`, `ftc-06`, `ftc-09`: bounding boxes equal to
  STEP within 0.05 mm; not closed where fillet or spline faces are skipped
  or torus conventions are unresolved.
* Prototype and notes: `cad-decoding-notes/creo.md` and
  `creo-prototypes/` next to the repository (not in it).

## Gaps

* Spline surfaces (`splsrf`: interpolated point grid with end tangents),
  fillet surfaces (three splines of a rolling-ball blend) and ruled surfaces
  are skipped – 64 of 4 332 faces in the test parts.
* Model units: the file says mm or inch in `UnitSystemDef`; the mesh keeps
  the model units for now.
* Assemblies (`.asm.N`): no freely licensed test file was found (NIST has no
  Creo assembly; the GitHub finds carry no licence). Component names are in
  `MdlRefInfo`; placements have not been looked at.
* Drawings and other Creo files keep the stored JPEG.

## Licences and prior art

The layout was worked out from the files alone, which the EU software
directive (2009/24/EC, art. 6) and § 69e UrhG allow for interoperability.
No open implementation of the Creo binary format was found, nothing was
copied. "Creo" is PTC's trade mark and is only used to name the format.
Only the openly licensed NIST files are committed as fixtures.
