# Mass properties from exported meshes

Mass, centre of gravity (CG) and inertia tensor of an assembly – a vehicle,
a rocket, a robot arm – are needed by everybody downstream of the CAD model:
flight simulation, structural analysis, balancing, transport. Getting them
out of the CAD system is surprisingly painful: the assembly must be fully
loaded, every part needs the right material, bought parts are modelled as
envelopes with the wrong density, and the result lives in one engineer's
session. This page records how mass properties are computed from triangle
meshes, what can go wrong, how other tools take them, and the design
TeamCraft adopts. Date: 2026-11-09.

## The method: signed tetrahedra and the divergence theorem

For a closed, consistently oriented triangle mesh, every volume integral of a
polynomial can be turned into a surface integral (divergence theorem) and
evaluated exactly per triangle. Equivalently: each triangle and the origin
span a tetrahedron whose *signed* volume is `a · (b × c) / 6`; summed over a
closed surface, the parts outside the body cancel and the sum is the volume.

* **Mirtich (1996)**, *Fast and Accurate Computation of Polyhedral Mass
  Properties*, Journal of Graphics Tools 1(2): the classic reduction of the
  volume integrals of 1, x, y, z, x², y², z², xy, yz, zx to face and then
  line integrals (projection onto a coordinate plane).
* **Eberly (2002, revised 2009)**, *Polyhedral Mass Properties (Revisited)*,
  Geometric Tools: the same ten integrals for triangle meshes with a compact
  set of per-triangle sub-expressions – no projection, no case analysis,
  about 50 multiplications per triangle. This is the formula TeamCraft uses.
* **Tonon (2004)**, *Explicit Exact Formulas for the 3-D Tetrahedron Inertia
  Tensor in Terms of its Vertex Coordinates*, Journal of Mathematics and
  Statistics 1(1): the same result written per tetrahedron; useful as an
  independent check in tests.

From the ten integrals with density ρ:

* mass `m = ρ ∫ dV`, CG `c = ρ ∫ r dV / m`;
* inertia about the CG `I = ρ ∫ (|r|² E − r rᵀ) dV − m (|c|² E − c cᵀ)`
  (tensor form: the off-diagonal entries are the *negative* products of
  inertia, `I_xy = −∫ xy dm`; CAD systems differ here, so exports must state
  the convention).

**Numerical care.** Parts exported in a vehicle frame can sit metres away
from the origin while being millimetres thick. `∫ x² dV − m c²` then cancels
almost all digits. The integrals are therefore accumulated relative to a
point inside the part (the centre of its bounding box) and only the CG is
shifted back – the inertia about the CG does not depend on that point.

**Combining parts – the parallel axis theorem.** For bodies `i` with mass
`mᵢ`, CG `cᵢ` and inertia `Iᵢ` about their own CG:
`m = Σ mᵢ`, `c = Σ mᵢ cᵢ / m`, and with `dᵢ = cᵢ − c`:
`I = Σ (Iᵢ + mᵢ (|dᵢ|² E − dᵢ dᵢᵀ))`. The same shift gives the inertia about
any reference point (a nose tip, a launch-lug station, a gimbal point).

## Accuracy against tessellation

A mesh is an *inscribed* approximation of curved surfaces: the volume comes
out slightly low. For a cylinder of radius `r` tessellated with `n` segments
the area ratio is `(n / 2π) sin(2π / n) ≈ 1 − (2π/n)²/6`: 36 segments lose
0.5 %, 360 segments 0.005 %. Expressed in the chordal deviation `δ` that
CAD exporters ask for, the relative volume error of a round section is about
`(4/3) δ / r`: `δ = 0.01 mm` on a 50 mm radius is 0.03 %. Inertia (∝ r²)
has about twice the relative error of the volume. In practice the density –
a handbook value for a material whose real density differs by a percent or
two, infill of printed parts, glue, paint, fasteners – dominates the error,
not the tessellation; an export with a fine chordal tolerance (0.01–0.05 mm)
is ample.

Thin walls are the exception worth knowing: in a tube both surfaces are
inscribed, so the outer one loses volume and the cavity loses volume too.
The two errors partly cancel, but what remains is relative to the small wall
cross-section, not to the whole disc – a 1.5 mm airframe wall exported with
a coarse tolerance can be off by a few percent. Fine export settings matter
there.

## What breaks the method

* **Open meshes** (holes, missing faces, a partial STEP tessellation): the
  signed volume then depends on where the origin is – the result is not a
  physical quantity. Detected by counting edges used by exactly one
  triangle. The values are computed (relative to the part's own bounding box
  centre, which limits the damage for small holes) but flagged.
* **Non-manifold edges** (three or more triangles on one edge – two bodies
  touching along an edge, duplicated faces): usually still correct when the
  bodies are themselves closed; flagged.
* **Inconsistent orientation** (some triangles flipped): the flipped ones
  subtract instead of add. Detected per edge (a shared edge must be used once
  in each direction). Cannot be repaired without a surface walk and is
  flagged.
* **Fully inverted meshes** (every triangle flipped, a common exporter
  mistake): the volume comes out negative – the whole mesh is inverted and
  the sign is simply flipped, with a note.
* **Hollow bodies and cavities** are correct by construction: an inner
  surface oriented inwards subtracts its volume.
* **Several bodies in one file** (fins exported together) add up correctly
  as long as each is closed and they do not overlap; overlapping bodies are
  counted twice (a mesh-boolean would be needed; out of scope).
* **Point clouds** have no volume.

## Units

STL has no unit field at all; OBJ, PLY and OFF neither. 3MF declares a unit
in the model header, glTF is metres by specification, STEP carries its
unit in the file. CAD exports of STL are millimetres by convention but
inch exports exist. Mass properties scale with the unit `s` (metres per
model unit): volume and mass with `s³`, CG with `s`, inertia with `s⁵` – an
inch file read as millimetres is off by 25.4⁵ ≈ 10⁷ in inertia, which is
obvious, but a centimetre file read as millimetres is "only" off by 1000 in
mass, which can slip through. A per-file unit, shown next to every
computed value, is therefore necessary; millimetres as the default, glTF as
metres.

## Frames: one vehicle frame or local frames with placements

Two ways to get parts into one coordinate system:

1. **Every part exported in the assembly / vehicle frame.** The CAD system
   writes each component in assembly coordinates (NX: *Export STL* with the
   assembly as work part and the component selected, or the *output
   coordinate system*; most CAD systems have an equivalent). The PLM then
   only adds – no transforms, no placements, no dependency on the CAD
   system's assembly file. A part used several times (four fins) is either
   exported once with all instances, or once per instance as its own file.
2. **Parts in their local frame plus placements.** The BOM line stores a
   4×4 transform per instance (Teamcenter's *occurrence* transforms, the
   SOLIDWORKS component tree, NX's structure). This is what CAD-integrated
   PLM does, but it needs the placements in the PLM – and keeping them in
   sync is exactly the CAD-integration work that is fragile.

TeamCraft's BOM lines carry no placements today (the 3D preview of NX and
SOLIDWORKS assemblies reads them from the assembly file, not from the BOM).
Option 1 is the workflow that removes the CAD system from the loop, and it
is the one adopted. Placements on BOM lines (per-instance transforms) are
follow-up work.

**Quantities in a common frame.** A BOM line "fin × 4" with a file that
shows one fin would put four fins on top of each other: the mass is right,
the CG and inertia are not. The file therefore records how many units it
shows (`instances`, default 1); the line contributes
`quantity / instances` times the file's body. When the two differ, the line
is flagged ("copies stacked at the same place").

## How other tools take mass, CG and inertia

| Tool                   | Mass properties                                                                                                                         | Overrides                                                                                                                  |
|------------------------|-----------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| NX                     | *Measure Bodies* / assembly *Weight Management* from the exact solid and the assigned material; needs every component loaded with geometry | assigned mass and CG on components; stored values go stale when a component is not loaded or a material is unassigned      |
| SOLIDWORKS             | *Mass Properties* from the solid and the part material; assemblies from the components                                                   | *Override Mass Properties*: mass, CG and inertia per part or assembly                                                      |
| Creo                   | mass properties from the solid and the density in the material                                                                          | *assigned mass properties* per part                                                                                        |
| Onshape                | mass properties tool, material library per part                                                                                         | per-part mass override                                                                                                     |
| Teamcenter / Windchill | store mass as attributes the CAD integration writes on save                                                                             | attributes editable; stale when the CAD side was not saved                                                                 |
| OpenRocket             | components of revolution with analytic mass, CG and longitudinal / rotational inertia from material densities (bulk, surface, line)     | per-component mass override and CG override, optionally covering the sub-components                                        |
| RocketPy               | takes the vehicle as given numbers: dry mass, CG without motor, inertia tuple `(I11, I22, I33, I12, I13, I23)` about that CG             | – (the motor's mass and inertia are combined with the rocket by parallel axis, in the coordinate system the user declares) |

Common ground: computed values come from geometry × material density; a
**measured or assigned value always wins** and covers the whole sub-tree
when set on an assembly; and the simulation tools want *one* set of numbers
– mass, CG and the tensor about the CG – with the coordinate system and the
tensor convention stated.

## Measured mass for bought and finished parts

Bought parts (motors, electronics, valves, bearings) are modelled as
envelopes: geometry × density is meaningless for them. Their mass comes from
a data sheet or a scale; the CG from the data sheet or a balance test, the
inertia rarely. Finished sub-assemblies are weighed as a whole. So a revision
gets a **measured mass** with an optional CG (and an optional inertia tensor
about that CG): when set, it replaces everything computed for that revision
and for its sub-tree, and it is shown as *measured*. Measured mass without a
CG still counts in the total mass, but cannot place the mass; the CG and
inertia then cover the placed mass only and say so.

## Decision

* **Pure module `massprops`**: Eberly's integrals relative to the bounding
  box centre, inertia about the CG per unit density in model units, plus an
  edge check (open, non-manifold, inconsistently oriented edges, welded on a
  tolerance grid like the shape signatures) and the fix of a fully inverted
  mesh; combination by parallel axis; principal moments. Bounded, no panics,
  tested against analytic solids.
* **Computed in the isolated preview process** from the cached preview mesh
  and cached next to it per content hash (`<sha>.v<N>.m<V>.json`), like the
  shape signatures – the server never parses an untrusted mesh. Units and
  density are applied afterwards (unit-free cache).
* **Densities:** an admin-managed **material library** (name → kg/m³); the
  revision's *material* attribute is looked up there by name. A per-revision
  **density override** wins.
* **Per-file settings:** the **length unit** (mm by default, m, in; glTF
  defaults to m) and the number of **instances** the file shows. The
  geometry file of a revision is chosen explicitly or picked automatically
  (STL, 3MF, OBJ, PLY, OFF, AMF, glTF, then STEP, then other formats with a
  3D preview).
* **Measured values** per revision: mass, optional CG, optional inertia about
  that CG, a note; they win over everything computed and cover the sub-tree.
* **Source order per BOM line:** measured → computed from the geometry file
  → the entered mass (no position) → missing. An assembly without a measured
  mass is the sum of its children (its own geometry and entered mass are
  not added – they would count the children twice).
* **Frames:** all parts in the common vehicle frame; placements on BOM lines
  are follow-up work.
* **Roll-up** over the multi-level BOM with the revision rule, quantities,
  and the read rights (restricted lines are left out with a note, as in the
  existing mass roll-up). Result: total mass, CG, inertia about the CG and
  about a chosen reference point, principal moments; per line the source,
  and warnings for open meshes, missing densities, unplaced masses and
  stacked copies.
* **Interfaces:** a page on the revision with CSV and JSON export, `GET
  /api/v1/revisions/{id}/mass-properties`, a read-only MCP tool.
