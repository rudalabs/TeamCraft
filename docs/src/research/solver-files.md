# Fluent, STAR-CCM+ and Simcenter 3D files

**Question:** how should TeamCraft's simulation management (FEA/CFD) handle
the files of ANSYS Fluent, Simcenter STAR-CCM+ and Simcenter 3D (NX CAE with
Simcenter Nastran)? What makes each file recognisable, what can be read
cheaply and safely, what must never be parsed, how a project folder maps to
TeamCraft items, and how the result values come in. Investigated on
2026-11-07. The investigation used public documentation only. No proprietary
SDKs were used, and no files of these programs were available for testing.
Every byte-level rule below is a **hypothesis until it is checked with real
files** (see *Open questions*).

## Starting point

* `src/cadformat.rs` recognises `.cas`, `.dat` (only with Fluent's `(0 …)`
  section syntax), `.cas.h5` and `.dat.h5` (Fluent) and `.sim`. A `.sim` file is
  STAR-CCM+ when `star-ccm` or `starccm` appears in its first 1024 bytes and NX
  in every other case. `.sim`, `.fem` and `.afm` are "NX simulation / FE model".
  It also recognises `.bdf`/`.nas` (Nastran deck) and `.op2`/`.f06` (Nastran
  results). Format detection never reads more than `HEAD_LEN` = 1024 bytes.
* Result values come from the `teamcraft-results.json|csv` exchange file, from
  per-group extraction templates (`src/resultscan.rs`: regex or CSV column,
  run over the **first 2 MB** of a small text file), from assistant drafts
  quoted from PDF reports (`src/pdftext.rs`), from manual entry, CSV/Excel
  import or the REST API. Large result files are never parsed fully. That is a
  standing decision, and this page keeps it.
* 3D previews exist for VTK/VTU/VTP and ASCII CalculiX `.frd`
  (`src/simmesh.rs`). Not one of the three programs' native formats is
  previewed, and that will not change (see below).
* HTML reports are indexed as raw markup, and their decimal numbers are lost
  ([#141](https://github.com/rudalabs/TeamCraft/issues/141)). Plain-text files
  lose their decimal numbers in the search text as well, and that search text
  is what `read_file_text` returns.

## What the programs write

### ANSYS Fluent

| File | What it is | Content | Role in TeamCraft |
|---|---|---|---|
| `.cas.h5`, `.dat.h5`, `.msh.h5` | case, data, mesh in the *Common Fluids Format* (CFF, the default since 2020 R1) | HDF5 | case/mesh = input, data = result (large) |
| `.cas`, `.dat`, `.msh` (+ `.gz`) | legacy case, data, mesh | `(index …)` sections: `(0 "comment")`, `(1 "header")`, `(2 dims)`, `(10 …)` nodes …; binary sections use the prefixes `20…`/`30…` (single/double precision) and end with `End of Binary Section 2010` | input / result |
| `.cdat` | legacy data interpolated to the nodes, for CFD-Post | binary | result (large) |
| `.flprj` + `<name>.cffdb/` | Fluent project ("managed project"): groups related case/data files and stores relative paths | project file + folder | input (the folder holds the CFF files) |
| `<name>-<n>-<iter>.dat.h5` … | autosaves of a run | HDF5 | result series: keep only the selected step |
| `.jou` | journal: TUI/Scheme commands (`/file/read-case …`, `/solve/iterate 500`) | text | input (run script) |
| `.trn` | transcript of a session or batch run: banner with the version, residual table per iteration, messages | text | log |
| `report-file-*.out` | report file of the report definitions: a quoted title line, a header with quoted column names (`"Iteration" "report-def-0" …`), then one row per iteration or time step | text, whitespace-separated | **result values** (last row) |
| exported XY plots, `.csv` from *Export* | quantities and plots | text | result values |
| `cleanup-fluent-<host>-<pid>.sh` / `.bat` | written by every Fluent session to kill stray processes | script | **ignore** |

### Simcenter STAR-CCM+

| File | What it is | Content | Role |
|---|---|---|---|
| `.sim` | the simulation: geometry, mesh, setup **and** solution in one file | **undocumented binary** – no public description of the header; Siemens documents only `starccm+ -info file.sim` (prints the version used to create the file, iterations, serial/parallel) | input and result in one, often GBs |
| `.sim~` | backup of the previous save | as `.sim` | ignore (the desktop app already skips names ending in `~`) |
| `.simh` | solution history (fields over time steps) | binary | result (large, never parsed) |
| `.java` | macros (recorded or written) | text; recorded macros begin with a comment that names the macro and the STAR-CCM+ version that wrote it | input (run script) |
| plot and table exports `.csv` | monitors, reports, XY plots | text, comma-separated, quoted headers | **result values** |
| scene/plot hardcopies `.png` | pictures | image | gallery on the *Results* card (exists) |
| simulation report `.html`/`.pdf` | exported report | HTML / PDF | report (PDF works today; HTML: #141) |
| batch log (stdout redirected, `.log`/`.txt`) | banner with version, iteration table, stop messages such as *Stopping criterion … satisfied* | text | log |
| `.ccm`, `.dmprj` | STAR-CCM+ export / Design Manager project | binary / project | input; not read |

### Simcenter 3D (NX CAE) with Simcenter Nastran

| File | What it is | Content | Role |
|---|---|---|---|
| `<model>.prt` | master CAD part | NX part (`SPLMSSTR` container since NX 1980) | the **basis**: a part item of its own |
| `<model>_fem1_i.prt` | idealized part (defeatured, split copy of the master) | NX part | input of the FE model |
| `<model>_fem1.fem` | FE model: mesh, materials, physical properties | NX part file with a CAE extension (same container – to be verified) | input (can be a *Mesh* basis) |
| `<model>_sim1.sim` | simulation: solutions, loads, constraints, references the `.fem` | NX part file (same container – to be verified) | input |
| `.afm` | assembly FEM (references several `.fem`) | NX part file | input |
| `<sim>-<solution>.dat` | solver deck that Simcenter 3D writes for Nastran | text, bulk data (`SOL`, `CEND`, `BEGIN BULK`, `$` comments with the translator version) | input |
| `.f06` | print output: tables, messages, `FATAL` / `END OF JOB` | text, can be hundreds of MB with full printout | log / result |
| `.f04`, `.log`, `.diag` | execution summary, solver log | text | log |
| `.op2` | binary results (Fortran records, the first records hold a date and the label `NASTRAN FORT TAPE ID CODE`) | binary | result (large, never parsed) |
| `.h5` | HDF5 results (MSC Nastran; newer Simcenter Nastran releases – to be verified) | HDF5 | result (large, never parsed) |
| `.pch` | punch output | text | result |
| scratch (`.MASTER`, `.DBALL`, `.SCRATCH`, …) | solver work files | binary | **ignore** |

The `_fem1`, `_fem1_i` and `_sim1` suffixes are what Simcenter 3D proposes
by default. They are a convention that users can change, so TeamCraft may use
them as a hint and never as a rule.

**Collision:** Altair OptiStruct/HyperMesh also use `.fem`, but for a **text**
Nastran-style deck (`$$` comments, `BEGIN BULK`). A text `.fem` is therefore
not an NX FE model.

## Decisions

### 1. Recognition (format detection, within the 1024-byte head)

| Rule | Why |
|---|---|
| `.sim` / `.fem` / `.afm` starting with `SPLMSSTR` → `nx-simulation` | NX writes all its part files in this container, so this is a **positive** NX test. The current rule only has a negative test (it falls back to NX) |
| `.sim` naming STAR-CCM+ in the head → `starccm-sim` (unchanged) | the only cheap clue; unverified |
| `.sim` with neither → keep the current fallback (NX), and mark the detection as uncertain (format note "content not recognised") | older NX releases (before 1980) and STAR-CCM+ files without the marker both end up here; a wrong STAR-CCM+ label is worse than an honest "uncertain" |
| text `.fem` → new `optistruct-deck` (or unclassified), binary non-NX `.fem` → unclassified | collision with OptiStruct |
| `.dat` with Nastran bulk-data markers (`SOL `, `CEND`, `BEGIN BULK`, `INIT MASTER`, a `$` comment naming Nastran/Simcenter) → `nastran-bulk` | Simcenter 3D writes its decks as `.dat`; today they stay unclassified because `.dat` only counts for Fluent |
| `.f04`, `.pch` → Nastran (log/result), text only | complete the Nastran set |
| `.cas.h5`, `.dat.h5`, `.msh.h5` by name **and** the HDF5 signature `\x89HDF\r\n\x1a\n` at offset 0 or 512 (HDF5 allows a user block in front) | name alone accepts any file |
| `.cas.gz`, `.dat.gz`, `.msh.gz` by name and the gzip magic `1f 8b` | legacy compressed Fluent files are common in archives |
| `.msh` with Fluent sections `(0 `, `(1 `, `(2 `, `(10 ` → `fluent-mesh`; `$MeshFormat` → Gmsh (if listed); other → unclassified | Gmsh also uses `.msh` |
| `.flprj` → `fluent-project`; `.cdat` → Fluent result; `.jou` → Fluent journal (text); `.trn` → Fluent transcript (text); `.simh` → STAR-CCM+ solution history; `.op2` only when not text | complete the sets |
| `report-file-*.out` and other `.out` files whose first line is a quoted title and second a quoted header → Fluent report file | for the template support below |

HDF5 files are **never opened as HDF5** by the server, not even to read
attributes. Pure-Rust HDF5 readers exist under MIT/Apache-2.0
(`hdf5-pure`, `rust-hdf5`, `rustyhdf5-format`), but they are young (first
releases in 2026) and an HDF5 object header parser is a large attack surface.
The signature is enough for recognition. Reading the Fluent version from the
CFF attributes is not worth that surface. Revisit this only if a concrete need
appears, and then only in the isolated child process.

### 2. Cheap metadata (bounded, text files only)

A small **solver summary** per file version is worth having for the
following, all read from text and all bounded (head **and tail** of at most
64 KiB each, never the middle of a large file):

| File | Summary |
|---|---|
| Fluent `.trn` | solver version (banner), last iteration / time step, "solution is converged" yes/no, last residuals line |
| Fluent `report-file-*.out` | column names, number of rows, the last row (iteration and values) |
| Fluent `.jou` | files read and written (`/file/read-case…`, `/file/write-…`) – used as references, like `cadrefs` |
| STAR-CCM+ `.java` | the version from the header comment; `.sim` files the macro loads or saves (`resolvePath("….sim")`) as references |
| STAR-CCM+ batch log | version, last iteration, stopping criterion message |
| Nastran `.f06` | solver name and version (page header), **run status** from the tail: `END OF JOB`, number of `FATAL` / `WARNING` messages (counted only in the head/tail windows, so the counts are labelled "in the last 64 KiB") |
| Nastran `.dat`/`.bdf` | `SOL` number, the translator/solver version comment |
| NX `.sim`/`.fem`/`.afm` | the referenced files from the `ExternalReferences` streams of the `SPLMSSTR` container (the reader in `src/nxpart.rs`, if these streams exist in CAE parts – to be verified) |

Things such as *max stress from the `.f06`* are **not** read automatically.
The relevant tables sit in the middle of a file that can be hundreds of MB,
and their layout depends on the solution sequence and the output requests.
These numbers come in through templates (bounded, per group, chosen by the
team) or from the report.

### 3. Never parsed

`.dat.h5`, `.cas.h5` (beyond the signature), legacy binary `.dat`/`.cdat`,
STAR-CCM+ `.sim` (beyond the head) and `.simh`, Nastran `.op2` and `.h5`, and
NX CAE parts beyond their stream directory. None of them is converted for the
3D viewer either: the formats are undocumented or too large, and the way to a
3D view stays an export to VTU/VTK (Fluent: *Export → EnSight/CGNS/VTK*;
STAR-CCM+: *Export → VTK/EnSight*; Simcenter 3D: post-processing export),
which the viewer already reads.

### 4. Project folders → items

One simulation item = one analysis (unchanged). For each program:

* **Simcenter 3D:** the `.sim` with its `.fem`, `.afm` and idealized
  `_i.prt` files and the solver files of its solutions
  (`<sim>-<solution>.*`) form **one simulation item**. The master `.prt` is
  **not** copied into it. It is the *Geometry* basis, pinned to the file
  version that was simulated. If several `.sim` files in a folder share one
  `.fem`, the `.fem` and its idealized part become their own item (type
  simulation, role *Mesh* basis of each simulation). The reference chain
  `.sim → .fem → _i.prt → master .prt` (from the NX stream directory) lets
  TeamCraft **propose** the basis links instead of making the user type
  them.
* **Fluent:** the case (`.cas.h5`), the journal, the transcript, the report
  files and the final data file form one simulation item. Autosave series go
  in only for the selected steps. A shared mesh (`.msh.h5`) can be a *Mesh*
  basis. A `.flprj` with its `.cffdb` folder is taken along as it is (the
  relative paths must survive), so it belongs to the item as a folder. The
  case does not reference CAD files, so the geometry basis is set by the user
  or by *New simulation from this revision…*.
* **STAR-CCM+:** the `.sim` file is model **and** result, so it is the
  simulation's main file. Macros, exported CSV tables, scene PNGs and the
  report go into the same item. The geometry basis is set by the user (a
  `.sim` imported from CAD keeps no file references we can read).

Roles of files inside the item (shown as a small label, used by the desktop
import and by retention): **model/input**, **result**, **log**, **report**,
**picture**, **script**. The role follows from the format (kind
`Simulation` / `Result` plus the new log/script/report formats). It is not
stored per file.

### 5. Desktop folder import and check-in

* **Ignore** `cleanup-fluent-*`, Nastran scratch files and solver lock files,
  in addition to the existing backup rules (`.sim~` is already ignored).
* **Group** by the rules in section 4: the `.sim`/`.fem` chain for Simcenter 3D
  (from the stream directory, falling back to the `_simN`/`_femN` name
  convention), the case stem for Fluent, the `.sim` stem for STAR-CCM+.
* **Size:** show the size of each result file, and leave results above a
  threshold (default: the preview limit, 64 MB) unticked in the check-in.
  Autosave series are unticked except for the last step. STAR-CCM+ `.sim`
  files stay ticked (they are the model) with a hint that saving after
  *Clear Solution* makes them small, if the team does not need the solution in
  the vault.
* **Basis:** for Simcenter 3D the import proposes the master part's item as
  *Geometry* basis when it is in the vault (or in the same import). The user
  confirms it.
* Check-in of large results needs the open retention rule (`files.retention =
  'latest'`) and uses the chunked resumable upload (already built).

### 6. Result values

In order of value:

1. **Exchange file written by the run.** Document short, tested snippets that
   write `teamcraft-results.csv` at the end of a run: a STAR-CCM+ Java macro
   (report values → CSV), a Fluent journal/PyFluent script (report
   definitions → CSV) and a Nastran/pyNastran post-script. These are
   documentation only, and they are the most reliable path because the team
   decides which numbers matter.
2. **Templates over text outputs**, with two small extensions to
   `resultscan`:
   * `pick = last` reads the **tail** (last 2 MB) instead of the head. Today a
     long transcript or report file has its final values beyond the first
     2 MB, where templates never look.
   * a `table` method for whitespace-separated tables with quoted headers
     (Fluent `report-file-*.out`): column by header name, last (or first)
     row. STAR-CCM+ CSV exports already work with the `csv` method. They
     need a *last row* pick when there is no key column.
   * add `trn`, `f06`, `f04`, `pch`, `jou`, `java` to `resultscan::scannable`
     and to the plain-text kinds of `textindex`.
3. **Reports:** PDF works today; HTML reports (STAR-CCM+, Simcenter 3D,
   Fluent's report export) come with #141. Plan both together with the
   plain-text `read_file_text` fix: the assistant should read the *real*
   text (with decimal numbers, bounded, head and tail) of `.out`, `.trn`,
   `.f06` and CSV files, so `propose_result_values` can quote them like PDFs.
4. **Assistant proposals** stay drafts with a quote, and a CAD editor accepts
   or rejects them (unchanged).

## Not done, and why

* No parsing of `.op2`, `.h5`, `.dat.h5`, `.sim`, `.simh`: size, undocumented
  formats, attack surface. Exports to open formats are the way to 3D and to
  numbers.
* No use of vendor tools on the server (`starccm+ -info`, Fluent, Nastran):
  licences and installation. A later desktop feature *could* run
  `starccm+ -info` on the user's machine with the user's licence, as the
  STEP export already does with local CAD programs.
* No pyNastran-style OP2 reader in Rust: an F06/OP2 table reader is a project
  of its own. A template on the `.f06` or a script-written exchange file
  covers the key numbers.

## Open questions

* **Real files** for fixtures: the head (first 1–4 KB is enough) of a
  STAR-CCM+ `.sim` from two or three versions, of NX 1980+ `.sim`/`.fem`/`.afm`
  and of an older NX release, a Fluent `report-file-0.out` and `.trn`, a
  Simcenter 3D Nastran `.dat` and `.f06` header. Openly licensed or
  self-made, as for the CAD fixtures.
* Do NX CAE parts carry the `ExternalReferences` streams that `nxpart` reads,
  and do they name the `.fem` / `_i.prt` / master part?
* Does Simcenter Nastran write `.h5` results in the versions teams use, and
  under which name?
* Should a `.sim` without either marker be labelled "uncertain" in the UI, or
  is the NX fallback good enough?
* Does a shared FEM become its own item by default, or only when the user asks?

## Sources

* Ansys Fluent User's Guide 2024 R2, *Legacy Case and Data File Formats*
  (section indices, header example `(0 "fluent24.2.0 build-id: 0")`, binary
  prefixes `20`/`30`, `End of Binary Section`):
  <https://ansyshelp.ansys.com/public/Views/Secured/corp/v242/en/flu_ug/flu_ug_app_form_case_data.html>
* Ansys CFD-Post, *Ansys Fluent Files* (`.cas.h5`, `.dat.h5`, `.msh.h5`, legacy
  `.cas/.dat/.msh(.gz)`, `.cdat`, `.flprj`):
  <https://ansyshelp.ansys.com/public/Views/Secured/corp/v252/en/cfd_post/CJAEEJBJ.html>
* Fluent, *Working With Project Files* (`.flprj`, managed projects in a
  `.cffdb` folder, relative paths):
  <https://ansyshelp.ansys.com/public/Views/Secured/corp/v252/en/fluent_beta_doc/fltg_sec_param_stu_prj.html>
* PyFluent settings API, report definitions:
  <https://fluent.docs.pyansys.com/version/stable/api/solver/_autosummary/settings/report_definitions.html>
* Siemens knowledge base KB000033094 (`starccm+ -info model.sim`):
  <https://support.sw.siemens.com/en-US/okba/KB000033094_EN_US/Could-I-check-the-architecture-used-in-creating-a-STAR-CCM-file-without-opening-it/index.html>
* CFD Online, `.sim~` backup files and the unanswered question whether `.sim`
  is a known archive format:
  <https://www.cfd-online.com/Forums/star-ccm/66804-file-sim.html>
* Siemens knowledge base KB000069296, Simcenter 3D result files and their
  extraction for storage:
  <https://support.sw.siemens.com/en-US/okba/KB000069296_EN_US/Reducing-contents-of-result-file-by-extraction-for-more-efficient-storage/index.html>
* pyNastran OP2 writer (start record `NASTRAN FORT TAPE ID CODE`):
  <https://pynastran-locr.readthedocs.io/en/latest/_modules/pyNastran/op2/writer/op2Writer.html>
* crates.io: `hdf5-pure` (MIT OR Apache-2.0), `rust-hdf5` (MIT),
  `rustyhdf5-format` (MIT), checked 2026-11-07.
