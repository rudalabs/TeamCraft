# Importing existing CAD folder trees

**Question:** how do PLM/PDM systems bring an existing folder of CAD data –
often hundreds of files, with project files – into the vault, and what should
TeamCraft's import do? Investigated on 2026-11-04.

## How other systems do it

| System                     | Tool                                                                        | What it does                                                                                                                                                                                                                                                             |
|----------------------------|-----------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Autodesk Vault             | **Autoloader**, or *Check In Project* in Inventor                           | scans the folder with the Inventor project file (`.ipj`), reports missing references, uploads in batches (up to 20 000–30 000 Inventor files), keeps the folder structure, works on a copy – the source files stay untouched; DWF creation can be switched off for speed |
| SolidWorks PDM             | copy/paste of folders, scan tools, VAR import programs                      | migration guides: scan first, map folders and properties, fix broken references and duplicates before the import, decide whether files come in at their existing revision                                                                                                |
| Teamcenter with Solid Edge | SEEC *Data Preparation*: *Analyze*, then *Add to Teamcenter* (with dry run) | expects a flat source folder **without duplicate file names**; *Analyze* reports duplicate names, duplicate document numbers and broken links; item IDs are generated when the document number is empty; Design Manager can flatten folders and rename duplicates        |

The common pattern:

1. **Prepare** – one project per product (Inventor: gather the files and the
   `.ipj` into one project, e.g. with *Pack and Go*), fix broken references,
   make file names unique.
2. **Analyse first (dry run)** – a report of what will be imported, duplicate
   names, broken references; nothing is changed yet.
3. **Import** – in batches, from a copy (the source stays untouched), with
   generated or property-based numbers; viewables optional.
4. **Afterwards** – check the structure, release the data through the
   normal workflow.

## Decision for TeamCraft

TeamCraft matches CAD references by **file name**, like Teamcenter – so file
names must be unique within an import. The desktop app gets an **Import folder…** wizard:

* **Scan (dry run):** walks the folder, skips lock/backup files and
  `OldVersions` folders, sorts the files into models (parts, assemblies),
  drawings, exports (STEP, STL, PDF, …), project files (`.ipj`, …) and other
  files, and reports duplicate file names. Nothing is uploaded yet.
* **Plan:** one item per model file (assembly or part); a drawing and
  exports with the **same base name** go into the same item (the item's
  datasets); project files and other documents can go into one document
  item named after the folder. Numbers are generated, or taken from the file
  names when they are valid item numbers.
* **Import:** item by item with progress and stop, from the original files (read only); every new item is checked in at
  the end, so nothing stays
  locked; the server scans the assemblies for the files they reference.
* **Structure:** optionally BOM lines from the references found in the
  assemblies (quantity 1, to be checked) – exact quantities come from the
  BOM export of the CAD program (*Import BOM…*).
* **Release** afterwards with one change order, like every other release in
  TeamCraft (no import "as released" past the lifecycle).

An in-app tutorial (desktop *Help → Import a CAD folder*) explains the
preparation (Pack and Go, project file, unique names), the wizard and the
check afterwards.

## Sources

* Autoloader and Inventor projects:
  [Vault help – Autoloader](https://help.autodesk.com/cloudhelp/2025/ENU/Vault-General/files/GUID-EA9B2552-30B9-4789-9A5E-6580B8648981.htm),
  [Check in Project with Inventor](https://autodesk.com/support/technical/article/caas/sfdcarticles/sfdcarticles/CHS/How-To-Check-in-Project-using-Autodesk-Inventor-and-Autodesk-Vault.html),
  [Modify an Inventor project](https://help.autodesk.com/cloudhelp/2026/ENU/Vault-General/files/GUID-DB018A2B-8B78-401D-B45C-B0AEE3613D30.htm)
* SolidWorks PDM migration:
  [TriMech – data migration best practices](https://trimech.com/solidworks-pdm-data-migration-best-practices-guide/),
  [GoEngineer – manually importing CAD data](https://www.goengineer.com/blog/manually-importing-cad-data-into-solidworks-pdm-overview),
  [Javelin – how to prep for a PDM migration](https://www.javelin-tech.com/blog/2022/05/how-to-prep-for-a-solidworks-pdm-migration/)
* Solid Edge and Teamcenter:
  [SEEC Data Preparation tools](https://support.sw.siemens.com/en-US/okba/PL8547626/Using-the-SEEC-Data-Preparation-Tools-simplified-workflow/index.html),
  [Analyze](https://soliddna.com/SEHelp/ST5/EN/a_h/analyze2ad.htm),
  [Add to Teamcenter](https://www.soliddna.com/SEHelp/ST5/EN/a_h/addtoteamcenter1a.htm),
  [Design Manager – flatten folder structure](https://support.sw.siemens.com/en-US/okba/PL8015842/Using-Design-Manager-to-flatten-folder-structure/index.html)

## Status

* **Implemented** in the desktop app: *Tools → Import CAD Folder…*
  (`desktop/src/folderimport.rs` for the scan, the plan and the import in
  `desktop/ui/app.js`) and the tutorial *Help → How to Import a CAD Folder*.
  Imported items are remembered per folder (browser storage of the app), so
  an interrupted import continues where it stopped.
* Tested: the folder scan (unit test) and the planning with a sample tree (items, duplicates, Creo iterations, project
  files). **Not yet tried with a
  real Inventor or Solid Edge project of hundreds of files** – the BOM lines
  depend on the heuristic reference scan of the server.
