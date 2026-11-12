# Mass properties (mass, CG, inertia)

TeamCraft computes the mass, the centre of gravity (CG) and the inertia
tensor of an assembly – a vehicle, a rocket, a robot – from the 3D files of
its parts, the densities of their materials and the masses you measured.
Nobody has to load the whole assembly in the CAD system to get the numbers:
open the assembly revision and choose **Mass properties** (on the revision
page next to the BOM, or on the BOM page).

The method and its limits are described in the research note
[Mass properties from exported meshes](../research/mass-properties.md).

## The workflow

1. **Export every part in the assembly frame.** In the CAD system, export
   each part as STL (or 3MF, OBJ, STEP …) *in the coordinate system of the
   whole assembly or vehicle*, not in the part's own frame. TeamCraft then
   only adds the parts up – no placements, no CAD assembly file needed.
   A part used several times (four fins) is exported once with all its
   instances in one file; set **Units shown in the file** to 4.
2. **Attach the files** to the part revisions as usual. Attach the files of **one part** to a part revision, in as many
   formats as you like. The
   geometry file is then picked automatically (STL, 3MF, OBJ, PLY, OFF, AMF,
   glTF, 3DS, then STEP, then the lightweight data in NX, SOLIDWORKS, JT and
   Rhino files; the native format of your preferred CAD program first among
   the native ones) and the page says so: *Geometry: `box.stl` (chosen
   automatically; 2 other formats of the same model)*. Files belong to one
   model when their names agree apart from case, `_`, `-`, spaces, the
   extension and a trailing `rev-X` (`box.prt`, `Box_rev-B.SLDPRT`,
   `box.stp`). If the revision holds files of **several different parts**,
   TeamCraft does not guess – see [Geometry ambiguous](#geometry-ambiguous).
3. **Name the material.** The *material* attribute of the revision (e.g.
   `Aluminium 7075-T6`) is looked up in the [material library](#material-library),
   ignoring case. The *Material* field of *New item* and *Edit revision* (and
   of the desktop app) suggests the library while you type, with the density
   next to each name; picking a suggestion enters exactly the library name.
   Free text stays allowed – for a bought part with a measured mass, say –
   but a name that is not in the library shows a hint, and the mass
   properties report a missing density. A **density override** in the mass settings wins over the
   library – for 3D-printed parts with partial infill, for example.
4. **Enter measured values** for bought parts and weighed sub-assemblies:
   mass, optionally the CG and the inertia tensor about that CG.
5. **Check the result** on the mass properties page and export it as CSV or
   JSON for the flight simulation, the structural analysis or the balancing.

## Where each number comes from

For every revision the first source that applies is used:

| Source                         | When                                                                                | Gives                                                           |
|--------------------------------|-------------------------------------------------------------------------------------|-----------------------------------------------------------------|
| **measured**                   | a measured mass is entered                                                          | that mass; CG and inertia if entered; covers the whole sub-tree |
| **sum of children**            | the revision has BOM lines                                                          | the lines added by the parallel axis theorem                    |
| **computed**                   | a 3D file and a density (override or material library)                              | mass, CG, inertia from the closed mesh                          |
| **entered mass on geometry**   | a 3D file, no density, but the revision's *mass* attribute                          | the entered mass spread uniformly over the mesh                 |
| **entered mass (no position)** | only the *mass* attribute                                                           | counts in the total mass, not in the CG and inertia             |
| **geometry ambiguous**         | 3D files of several different parts and none chosen, no measured mass, no BOM lines | nothing – the line is flagged, the totals are incomplete        |
| **missing**                    | none of the above                                                                   | nothing – the line is flagged                                   |

Lines below a measured assembly are shown as *covered by measured parent*.
An assembly's own 3D file and entered mass are not added to the sum of its
lines (they would count the parts twice); enter a measured mass to replace
the sum.

A BOM line with quantity *n* adds *n* units. In the common frame these
units sit at the same place unless the file shows them all: a line whose
quantity differs from the units shown in the file is flagged *copies
stacked at the same place* – the mass is right, the CG and the inertia
usually are not.

### Geometry ambiguous

A plausible but wrong number is worse than none, so a revision whose 3D
files belong to several different parts (typically an assembly attached as
one item: the parts, the assembly and the neutral JT or STEP file) does not
get the mass of whichever file sorts first. Its source is **geometry
ambiguous**, it adds nothing to mass, CG and inertia, and the totals are
flagged incomplete: *N files of M different parts – choose the geometry file
in Mass settings, or split the parts into BOM lines*. The same applies to a
part revision inside an assembly. An entered mass still counts (without
position).

* TeamCraft suggests the **likely whole assembly**: the file that references
  other files of the revision and is referenced by none of them, else the
  file named like the item (number or name). The *Mass settings* card offers
  a one-click *Use `NIST mtc crada assembly.jt`* (the revision must be
  checked out); the choice is stored like a manual one. It is a suggestion
  only – nothing is used without the click.
* The file selector lists the candidates **grouped by model**, the likely
  assembly first, and marks the file in use as *automatic* or *chosen*; *Automatic* restores the automatic pick.
* The better fix is usually to model the assembly as an assembly: one item
  per part with the part's files, and BOM lines with quantities (see
  [The workflow](#the-workflow)).
* REST, the JSON export and the MCP tool carry `geometry` per line and
  `root_geometry`: `choice` (`automatic`, `chosen`, `ambiguous`), the file,
  the number of other formats, the candidates by model and the likely file.
  The CSV has the columns `geometry_file` and `geometry_choice`.

## Warnings

* **open mesh** – edges with one triangle only (holes): the volume is not
  well defined. Fix the export (closed solids only).
* **inconsistent orientation** – some triangles face inwards.
* **non-manifold edges** – bodies touching along an edge or duplicated
  faces; usually still correct.
* **inside out** – every triangle faced inwards; TeamCraft flipped the mesh.
* **no density** – no material, or the material is not in the library.
* **no position** – a mass without CG (entered mass, measured without CG):
  counted in the total mass, left out of the CG and inertia.
* **restricted lines** – items of project groups you are not a member of
  are left out, like in the BOM roll-up.
* The entered *mass* attribute differs by more than 5 % from the computed
  mass.

## Units and conventions

* The **length unit** is set per file: millimetres by default (glTF:
  metres), metres or inches. Results are always SI: kg, m, kg·m².
* The CG and the tensors are in the frame of the geometry files.
* The inertia tensor is given **about the CG** and **about a reference point**
  you enter (`x,y,z` in metres, e.g. the nose tip), in tensor form:
  the off-diagonal entries are the *negative* products of inertia,
  `Ixy = −∫ xy dm`. The principal moments are listed as well.
* Accuracy: a mesh approximates curved surfaces from inside. With an export
  chord tolerance of 0.01–0.05 mm the error is far below that of handbook
  densities; an icosphere of 1 280 triangles is 0.9 % light, one of 20 480
  triangles 0.05 %.

## Mass settings

The **Mass settings** card on the page holds, per revision: the geometry
file with its length unit and number of units shown, the density override,
and the measured mass, CG, inertia tensor and a note (scale, data sheet,
date). Changing them needs the revision checked out, like every other
attribute; *Revise* carries them over to the next revision.

## Material library

**Admin → Materials** (`/materials`) lists the densities. Every signed-in
user can read it; administrators add, rename and remove materials. Names
are unique ignoring case. Removing a material does not change any revision;
their masses then report a missing density.

## Exports and interfaces

* **CSV** and **JSON** from the *Export* menu of the page (the totals, both
  tensors and every line with its source and warnings). Like the BOM
  downloads they follow the project group's download switch.
* REST: `GET /api/v1/items/{number}/revisions/{code}/mass-properties?rule=&ref=x,y,z`
  and `GET /api/v1/materials` ([REST API](../reference/api.md)).
* MCP: the read-only tool `get_mass_properties` ([MCP server](../reference/mcp.md)).

## Limits

* Placements (transforms per BOM line) are not stored: parts exported in
  their own frame cannot be combined yet.
* Overlapping bodies in one file are counted twice.
* Point clouds and simulation results have no volume.
* Measured values can only be changed in an in-work revision.
