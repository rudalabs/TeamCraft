# 3D preview of Creo assemblies

*2026-11-12. Findings from reading the files only – no PTC software, no
toolkit, nothing decompiled.*

## Question

Can TeamCraft put together the 3D preview of a PTC Creo assembly
(`name.asm.N`) from the previews of its parts, as it does for NX,
SOLIDWORKS, Inventor and CATIA? What does an assembly store about its
components, and in which unit?

## Samples

No openly licensed Creo assembly was found (search of 2026-11-08: NIST,
GitHub, Zenodo, GrabCAD, PTC tutorials, open hardware). The format was
decoded on two public GitHub projects **without a licence**, which are
therefore used locally only and never committed:

* a model rocket (`rocket.asm.1`: three parts, millimetres, Creo 3);
* a scissor jack (`sc9132-2000-1.asm.1`: three sub-assemblies, 27 part
  instances, skeleton models, family-table generics, inches, Creo 7).

The repository tests build synthetic assemblies around the NIST part
`ctc-01` (see *Tests*) until real assemblies can be added.

## Findings

* An assembly uses the part container (`#UGC:2 ASSEMBLY …` header, text
  table of contents, LZW sections) and the same tagged format
  ([3D preview of Creo parts](creo-brep-preview.md)). Its own `VisibGeom`
  holds only assembly-level datums – the geometry is in the part files.
* **Section `NeuAsmSld`** (struct `neut_assem`) lists the direct
  components in `members`, one struct each:

  ```text
  id = 42  type = 2  name = 'MS_MID'  family_name = 'MS_MID'
  e1 = [1, 0, 0]  e2 = [0, -0.995, 0.096]  e3 = [0, -0.096, -0.995]
  origin = [34.58, 0, 0]
  ```

  * `type` 2 is a part (`name.prt.N`), 1 an assembly (`name.asm.N`,
    including skeleton assemblies); other types are not seen.
  * `e1`, `e2`, `e3` are the images of the component's x, y and z axes in
    the parent's frame and `origin` its origin: a component point `p` lands
    at `origin + p.x·e1 + p.y·e2 + p.z·e3` – the row convention of
    `nxasm::Component` (`rotation = [e1, e2, e3]`). The other reading
    (transposed) puts the rocket's base cone the wrong way round; the
    composed models match the stored thumbnails.
  * Only **direct** children are listed: a sub-assembly is its own file,
    read the same way, its placements composed with the parent's.
  * `name` is the model name in upper case without extension; a
    family-table instance is listed under the generic's name
    (`SC9132-2010-X_GENERIC`), whose file the instance's geometry comes
    from.
  * The same placements appear in `FeatDefsCmp` (`place_rec_ptr.local_sys`,
    `trf_arr.transf`) with the component's `model_ptr`; `MdlRefInfo` holds
    the referenced models of each placement constraint. `NeuAsmSld` is
    enough for the preview.
* **Units** are in section `ND:0:UnitSystemDef_L03:1`, an object archive
  parsed by the same tagged reader: `string_xar` holds the names in order
  of first use, and the length unit's name is the string after `Offset`
  (`mm`, `in`, …, with its definition after it: `0.001000 m`,
  `1.000000 in`). `double_xar[0]` is the length scale in metres for
  non-metric systems (`0.0254`). Parts carry the same section: the NIST
  part `ctc-05` is in **inches** (its STEP export too), `ctc-01` in mm.
  Parts and placements are converted to millimetres, so components in
  different units fit together.
* File names: TeamCraft stores Creo files without the iteration suffix
  (`bracket.prt.7` → `bracket.prt`, `cadformat::creo_iteration`), so a
  component is found as `<name>.prt` or `<name>.asm` (lower case), like
  SOLIDWORKS and Inventor components (`services::assemblies`).

## Part reader: parts without a starting face

The parametrisation conventions of a part's faces are resolved by
propagation from planes with the identity frame (and freeform faces) –
[3D preview of Creo parts](creo-brep-preview.md). Turned parts often have
none (all planes are perpendicular to an off-axis axis): every face then
kept the default convention and the mesh fell apart (rocket nose: 102 open
edges). When a part has **no starting face at all**, one face (planes
first, then cylinders, cones, tori) is now tried with each candidate
convention; the propagation that makes the most edge samples coincide is
kept, the default convention only loses when another candidate is strictly
better. The rocket's body became closed, its nose went from 102 to 9 open
edges. Parts with a starting face are unchanged: seeding their unreached
faces as well made two NIST parts worse.

## Gaps

* Family-table instances are shown with the generic's geometry.
* Suppressed or excluded components, simplified representations and
  explode states are not evaluated (`NeuAsmSld` lists the members of the
  master representation).
* The sample parts from older Creo versions show part-reader gaps: some
  faces arrive as stubs without data (rocket nose: 2 of 12 faces), and
  several scissor-jack parts keep open edges or single wrong faces.
* No real assembly is a repository fixture yet.

## Tests

Unit tests encode small sections in the tagged format (a writer that
spells out every transition) and check the members, the placement
convention, units and damaged input. A synthetic assembly placing the NIST
part `ctc-01` twice (one sub-assembly, rotated, in inches) checks the
composition end to end.
