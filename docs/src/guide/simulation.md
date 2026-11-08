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

### Fluent, STAR-CCM+ and Simcenter 3D files

* **Simcenter 3D (NX CAE):** `.sim`, `.fem` and `.afm` are recognised as NX
  simulation files by the `SPLMSSTR` container NX writes them in. The Nastran
  deck Simcenter 3D writes as `.dat` (`SOL`, `CEND`, `BEGIN BULK`, a `$`
  comment naming Nastran or Simcenter) is a *Nastran input deck*; `.f06`,
  `.f04`, `.pch` and `.op2` are Nastran results. A text `.fem` is an
  OptiStruct/HyperMesh deck, not an NX FE model.
* **Fluent:** case, data and mesh files – legacy (`.cas`, `.dat`, `.msh`),
  gzip-compressed (`.cas.gz`, …, checked by the gzip magic) and HDF5
  (`.cas.h5`, `.dat.h5`, `.msh.h5`, checked by the HDF5 signature) – plus the
  project (`.flprj`), CFD-Post data (`.cdat`), journals (`.jou`), transcripts
  (`.trn`) and report files (`report-file-*.out`).
* **STAR-CCM+:** the `.sim` (when its beginning names STAR-CCM+), the
  solution history (`.simh`) and macros (`.java` files that name STAR-CCM+ or
  import its API – other Java sources stay unclassified).

None of the binary files (`.sim`, `.simh`, `.op2`, HDF5, legacy binary data)
is parsed beyond its first bytes. The text files – transcripts, report
files, journals, macros, `.f06`/`.f04`/`.pch` – are read as text for the
search, the assistant and the extraction templates (below).

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

### Solver summary of a file

For solver output a one-line summary appears next to the file on the revision page
(and in the vault explorer): solver and version, how the run ended (`converged`,
`END OF JOB`, `fatal`), the last iteration or time step, and the number of `FATAL`
and `WARNING` lines. It is made at check-in from the **first and the last 64 KiB**
of the file only, so the counts cover those two windows, not the whole file – the
line says so. Supported: Fluent transcripts (`.trn`) and report files, Nastran
`.f06` and decks (`SOL`), STAR-CCM+ macros (`.java`) and batch logs
(`.log`/`.txt` that name STAR-CCM+). The summary is a hint read from a
user-written file, never a verified result; the MCP tools mark it as untrusted
data. The rules follow public documentation and typical output and are **not yet
checked against real files** of these programs; a file that does not match shows
no summary. Files stored before the feature have none until they get a new version.

### Values from files at check-in

Two bounded ways fill the table without typing – neither reads big result
files:

**The exchange file.** A file named `teamcraft-results.json` or
`teamcraft-results.csv` (at most 1 MB) checked in with the simulation
revision sets the values in the same transaction as the check-in, with an
audit entry:

```json
{ "results": [
  { "name": "Pressure drop", "value": 388.25, "unit": "Pa", "limit_max": 500, "load_case": "nominal" },
  { "name": "Max. von Mises stress", "value": 182.5, "unit": "MPa", "limit_min": null, "limit_max": 250,
    "solver": "CalculiX", "solver_version": "2.21", "note": "" }
] }
```

A bare array works too. `name` and `value` (a number) are required;
`unit`, `limit_min`, `limit_max`, `load_case`, `solver`, `solver_version` and
`note` are optional; other keys are refused. The CSV has the columns
`name,value,unit,limit_min,limit_max,load_case` (see above). A value with the
same name and load case is updated. A malformed file **refuses the whole
check-in** with the place of the problem (`results[1].value must be a number`,
`Row 3: …`, JSON line and column) – nothing is half-written, the revision stays
checked out.

**Extraction templates.** Managers of a project group (and administrators)
define templates on the group's page, or through the REST API. A template
finds one number in a small *text* file (`.txt`, `.csv`, `.log`, `.out`,
`.dat`, `.rpt`, `.html`, Fluent `.trn` and `.jou`, Nastran `.f06`, `.f04` and
`.pch`, STAR-CCM+ `.java`, … – the first 2 MB are read, and with `pick = last`
the last 2 MB of a larger file as well). An HTML report is
not read as markup: the template sees the text TeamCraft extracted from it
(see below), one line per table row with the cells separated by ` | `, so
`Maximum stress \| ([-+0-9.eE]+) \| MPa` finds the row
`Maximum stress | 152.4 | MPa`, and a `csv` template reads such a table
with `|` as the separator (always for an HTML file, whatever the title and
paragraphs above the table contain; an empty header cell keeps its column).

| Field | Meaning |
|---|---|
| `name` | the result's name (with `load_case`: the key of the value) |
| `file_pattern` | file names it applies to, `*` and `?`, case-insensitive, e.g. `*.log` |
| `method` | `regex`: the first capture group of `pattern` on a line is the number; `csv`: the column `csv_column` (header name), optionally only the row where `csv_key_column` = `csv_key_value`; `table`: the same for a whitespace-separated table with quoted column names (Fluent's `report-file-*.out`) |
| `pick` | `last` (default) or `first` match – a solver log reports the final value last. `last` reads the end of a file larger than 2 MB first (its last 2 MB, from the first whole line on) and falls back to the beginning; a `csv`/`table` header always comes from the beginning. `first` reads the beginning only |
| `unit`, `lower_limit`, `upper_limit`, `load_case`, `solver` | stored with the value; the limits decide pass or fail |
| `enabled` | switch a template off without deleting it |

Example: `Pressure drop:\s*([-+0-9.eE]+)\s*Pa` on `*.log`. Regular
expressions run in linear time (no backtracking) and are limited in size, so a
hostile pattern or file cannot hang the server. The value is shown with its
source (`solver.log:412`) in the *Solver* column. For an HTML report the line
number refers to the extracted text – the text `read_file_text` shows –, not
to the line in the HTML file. A template that does not
match does nothing; templates belong to one group and apply to simulations of
that group only. Values typed in by hand are overwritten by a template or the
exchange file with the same name and load case on the next check-in.

A value found in the end of a large file shows its place counted from the
end (`run.trn: line 3 from the end`), because the lines before it were not
read.

**Fluent report files.** A report file (`report-file-0.out`, written by the
report definitions of a run) looks like this:

```text
"report-def-0-rfile"
"Iteration" "pressure-drop" "flow-time"
("Iteration" "pressure-drop" "flow-time")
1 2.53e+02 0.001
2 2.49e+02 0.002
```

A `table` template with `csv_column = pressure-drop` and `pick = last` reads
the value of the last row (`2.49e+02`); with `csv_key_column = Iteration` and
`csv_key_value = 500` it reads the row of iteration 500 instead. STAR-CCM+
exports its monitors and reports as CSV (`csv` method). For Simcenter 3D /
Nastran a `regex` template on the `.f06` works for numbers that stand on one
line; the tables in the middle of a large `.f06` are better written to an
exchange file by a post-script.

### Values from a report (assistant)

A CFD or FEA report is usually a PDF or an HTML page. TeamCraft reads the text
of both when they are uploaded, so the assistant can read it with
`read_file_text`:

* **PDF:** the text layer, in an isolated process with a time, size and memory
  limit: at most 32 MB, 100 pages and 200 000 characters. Scanned PDFs have no
  text; there is no OCR.
* **HTML** (`.html`, `.htm` – the report generators of many FEA/CFD tools write
  them): the readable text, in the same isolated process and with the same
  limits (the first 16 MB of the file, 200 000 characters). A table row becomes
  one line with the cells separated by ` | `
  (`Equivalent (von Mises) stress | 0.0213 | 152.4 | MPa`); numbers, signs,
  exponents and units stay as written, character references (`&deg;`,
  `&nbsp;`, `&#176;`) are decoded. Scripts, styles, comments, embedded
  (base64) images and **hidden elements** (`hidden`, `aria-hidden="true"`,
  inline `display:none` / `visibility:hidden`) are skipped – hidden text is
  not part of the report and could carry instructions for the assistant. The
  charset comes from a byte order mark or `<meta charset>`, else UTF-8, else
  Windows-1252. Only the HTML file itself is read: images, style sheets and
  linked pages (a report with a `report_files/` folder) are never fetched or
  followed – attach a sub-page as its own file if its numbers are needed.
  MHTML (`.mht`) and ZIP-packed reports are not read.
* **Text** (`.txt`, `.csv`, `.tsv`, `.log`, `.md`, the solver logs, report
  files and scripts `.out`, `.trn`, `.jou`, `.f06`, `.f04`, `.pch`, `.java`, …):
  the text itself, line by line with its decimal numbers (the first 4 MB,
  200 000 characters). A larger file keeps its **end** as well – the last
  64 KiB (at most 50 000 characters) after the marker
  `[… middle of the file left out …]` – where a solver writes its status and
  final values.

The **Fill with AI…** button on the *Results* card (shown to editors of the
group when the server has an assistant and the group has not switched the
assistant off) opens a box: *What to extract* (the values you want, optional –
empty means every key number of the report), *Limits to use* (optional, for
example "upper limit 250 MPa for stress", applied only where it fits a value
you named) and the report files to read (PDF, HTML or text, all ticked).
*Propose values* is the request for drafts. Values you asked for that the
report does not contain are reported as *Not found* and are not proposed;
numbers are taken as written, never converted. Proposing works without a
check-out, accepting needs it.

The quick action **Fill result values from the report** on the simulation page
(drafts must be allowed) – or the MCP tool `propose_result_values` – proposes
the key numbers the report states. A proposal is a **draft**: it names the
file version and quotes the text it was read from (the quote must stand in
the report, so numbers cannot be invented from nothing) and appears under
*Proposed from reports* on the *Results* card. A CAD editor of the group
**accepts** it (the revision must be checked out to them, as for any value;
the value then shows the file as its source) or **rejects** it. The assistant
never writes a final value and never releases anything; every proposal,
acceptance and rejection is audited. Compare every number with the quote
before accepting – the model can misread a table.

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

## Finding simulations by their results

The item list (*Result values* and *Result name* in the filter bar, `result=`
and `result_name=` in the URL and in `GET /api/v1/items`) looks at the result
values of the **latest revision** of simulations:

| `result` | Matches simulations with at least one value that |
|---|---|
| `failed` | is outside its limits |
| `passed` | has limits and is within them |
| `above_limit` | is above its upper limit |
| `below_limit` | is below its lower limit |

`result_name` is a case-insensitive "contains" on the value name and can be
combined with `result` (the same value must match both), the type, project,
state and text filters. An unknown `result` is refused by the REST API.
Only items the user may read are found and counted: restricted groups stay
out of the list and its total.

The dashboard shows a card *Simulations to check*: simulations the user may
read whose latest revision (not obsolete) has a failed value or is out of
date (see above), ten at a time, with a link to the search. Bases the user may
not see never make a simulation appear out of date.

## Limits of version 1

* Result values are not extracted from solver result files (by design:
  large results are never parsed fully) – use the exchange file, templates
  on small text files, a CSV/Excel table or the REST API; VTK,
  VTU/VTP and `.frd` files are previewed in 3D (see above), the other result
  formats are stored and recognised, not parsed.
* Large result files go through the normal upload (`TEAMCRAFT_MAX_UPLOAD_MB`);
  chunked, resumable uploads and a retention rule for intermediate results
  are open work.
* The desktop app shows simulation items and their files; creating a
  simulation *from* a part revision and checking out the basis files are
  planned.
