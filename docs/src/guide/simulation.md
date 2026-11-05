# Simulations (FEA, CFD)

TeamCraft manages the analyses that belong to a design next to the design
itself: which part revision was analysed, with which file, what came out –
and whether that is still valid after the design changed.

## A simulation is an item

Create an item of type **Simulation** (numbers `SIM-000001`, …) with *New
item*, or through the link in the *Simulations* card of a part. One simulation item is one analysis,
e.g. *Bracket – static, LC1–LC3* or *Pump housing – modal*. It has the usual
lifecycle, revisions, check-out/check-in and change orders. Its files are
the model, the solver deck, selected results, the report and pictures;
TeamCraft recognises the common [model and result
formats](../reference/formats.md#simulation-models-and-results-fea-cfd)
(VTK, CalculiX, Nastran, ANSYS, Abaqus, OpenFOAM, Fluent, STAR-CCM+, CGNS,
Exodus II, LS-DYNA, …) but does not parse them.

## Based on – what was simulated

The **Based on** card of a simulation revision lists the revisions it was
computed with, each with a role:

| Role      | Typical basis                                    |
|-----------|--------------------------------------------------|
| Geometry  | the part or assembly revision that was analysed  |
| Mesh      | a mesh / FE model kept as its own item           |
| Material  | a material specification or data sheet           |
| Load case | the load specification or a test report          |
| Other     | anything else the result depends on              |

Add a basis by item number, revision code (empty = latest revision) and,
optionally, a **file name**: TeamCraft then pins the *current version* of
that file, so the simulation records exactly which file version it used.
Like BOM lines the basis links belong to the simulation revision: they
change only while it is checked out, *Revise* copies them, and they are
frozen when the simulation is released.

The part or assembly shows the other side in its **Simulations** card: every
simulation whose latest revision is based on it, with state, failed results
and whether it is up to date.

## Out of date

A simulation is **out of date** when, for one of its bases,

* a newer revision of the basis item was released,
* the basis revision became obsolete, or
* the pinned file has a newer version (or the pinned version was deleted).

The reason is shown next to the basis (*PRT-000012/C was released
(simulated: B)*). A basis in a restricted group that the reader may not see
shows no reason (see [Read permissions](../admin/permissions.md)). Releasing or obsoleting a basis also **flags the
simulation for a check** through the normal [change
propagation](structure.md#change-propagation): the simulation shows *Check
needed*, its author and watchers are notified, and the change's impact
analysis lists it. Acknowledge the check with a note when the result is
still valid (*fillet change outside the stressed region*), or revise the
simulation, re-run it, update the basis and the results and release it –
that resolves the check.

Releasing the basis revision itself (the simulation was based on an in-work
revision that is now released) only flags the simulation when a pinned file
has changed since; without a pinned file TeamCraft cannot tell.

## Results

The **Results** card holds the key numbers of the run: name, value, unit,
optional lower and upper limit, load case, solver and solver version. A value
**passes** when *lower limit ≤ value ≤ upper limit* (limits inclusive, either
may be empty) and **fails** otherwise; values without limits are informative.
The card summarises failed and passed values; the part's *Simulations* card
shows the number of failed values.

Values are edited in the table while the simulation revision is checked
out, or imported from a **CSV or Excel** table:

```text
name,value,unit,limit_low,limit_high,load_case
Max. von Mises stress,182.5,MPa,,250,LC1
Safety factor,1.37,,1.5,,LC1
First eigenfrequency,2.35e2,Hz,200,,modal
```

Optional columns: `solver`, `solver_version`, `note`. Semicolons, tabs,
decimal commas and German column names (`Kennwert`, `Wert`, `Einheit`,
`Untergrenze`, `Obergrenze`, `Lastfall`) work too; `min`/`max` are accepted
as limits. A value with the same name and load case is updated, new ones are
appended; *Replace all values* removes the values that are not in the file.
*CSV* on the card exports the table in the same layout.

Solver scripts write their results through the REST API
(`PUT …/simulation/results` replaces the set in one transaction; see the
[REST API](../reference/api.md#simulations)).

## Results in 3D

Result files in open formats open in the browser's 3D viewer like CAD
previews – no ParaView needed. *View result in 3D* on the Results card (or
*3D* next to the file) shows the outer surface of the mesh coloured by a
result field, with the field's minimum and maximum above the canvas.

| Format                          | Read                                                                                     |
|---------------------------------|------------------------------------------------------------------------------------------|
| Legacy VTK (`.vtk`)             | ASCII and BINARY, `UNSTRUCTURED_GRID` and `POLYDATA`, file versions up to 5.1            |
| VTK XML (`.vtu`, `.vtp`)        | `ascii`, inline `binary` and appended (`raw`/`base64`) data, zlib compression, all pieces |
| CalculiX (`.frd`)               | ASCII files: nodes, elements and every nodal result block (`DISP`, `STRESS`, `NDTEMP`, …) |

How the preview is made:

* **Surface.** Volume elements (tetrahedra, hexahedra, wedges, pyramids,
  quadratic ones by their corner nodes) contribute only the faces no other
  element shares; shells and polygons are drawn as they are, beams and lines
  are not. Interior nodes are dropped.
* **Fields.** Point data is shown as it is: one-component values directly,
  vectors as magnitude, six- or nine-component tensors as von Mises
  equivalent. Cell data is averaged to the nodes (shown as *name (cells)*).
  Up to eight fields are carried; the viewer offers them in a selector, the
  active scalars of a VTK file (or `STRESS` for `.frd`) first. A CalculiX
  block that occurs for several increments shows the **last** increment.
  Nodes without a value are grey. Units are shown when the file names them –
  VTK and FRD files usually do not.
* **Limits.** Up to 20 million nodes and cells; surfaces with more than two
  million triangles are thinned out evenly (gaps are visible), surfaces with
  more than eight million are refused. Collapsed elements (a hexahedron
  written as a wedge, …) are matched by their distinct corner nodes. Structured
  grids (`.vts`, `.vtr`, image data), time-step collections (`.pvd`,
  `.vtm`), binary `.frd`, Exodus II and CGNS are not previewed – export the
  step you want to show as `.vtu` (ParaView: *Save Data*, OpenFOAM:
  `foamToVTK`, CalculiX: `*NODE FILE, OUTPUT=ASCII` or ccx2paraview).

Like all previews, the conversion runs once per file version in a separate
process with a time limit and is cached; damaged files show an error message
instead of a model.

### Large results

Result files can be huge, and TeamCraft does not read them fully. A file above
`TEAMCRAFT_PREVIEW_MAX_RESULT_MB` (default 64 MB) is stored and downloadable
but gets no 3D preview (the page says so); smaller ones are converted in an
isolated process with a time and memory limit. Numbers are never extracted
from solver output on their own. To get the key figures into the *Results*
table, type them in, import a CSV/Excel table, or let a script or an agent
fill them in from a report. For pictures, attach rendered images (PNG, JPEG
or WebP, at most 8 MB each): they appear as a small gallery on the *Results*
card of the revision.

## Comparing results

*Compare with another revision* on the Results card puts the values of two
revisions of the simulation side by side – matched by name and load case,
with the difference, the relative change and the pass/fail status of both.

## Limits of version 1

* Result values are not extracted from result files automatically (by
  design: large results are never parsed fully); VTK,
  VTU/VTP and `.frd` files are previewed in 3D (see above), the other result
  formats are stored and recognised, not parsed.
* Large result files go through the normal upload (`TEAMCRAFT_MAX_UPLOAD_MB`);
  chunked, resumable uploads and a retention rule for intermediate results
  are open work.
* The desktop app shows simulation items and their files; creating a
  simulation *from* a part revision and checking out the basis files are
  planned.
