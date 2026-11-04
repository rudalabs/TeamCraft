# Previews of native CAD files (Inventor, Solid Edge)

**Question:** how can TeamCraft show previews of Autodesk Inventor (`.ipt`,
`.iam`, `.idw`, `.ipn`) and Siemens Solid Edge (`.par`, `.psm`, `.asm`,
`.dft`, `.pwd`) files – the way other PLM systems do? Investigated on
2026-11-04.

## How other PLM systems do it

| System                                                                                                | 2D thumbnail                                                                                                                                                                                                                                                                            | 3D view                                                                                                                                                                                                           |
|-------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Autodesk Vault                                                                                        | Inventor writes a thumbnail into the file when saving (512 × 512 since Inventor 2023); Vault does not render anything, it extracts that image at check-in                                                                                                                               | visualization files (DWF/DWFx) published at check-in by the Inventor add-in, or later by the *Job Processor*, which runs a licensed Inventor                                                                      |
| Siemens Teamcenter with Solid Edge                                                                    | the preview stored in the file                                                                                                                                                                                                                                                          | the Solid Edge integration (SEEC) writes JT files **through Solid Edge at check-in** – Siemens' recommended practice; the alternative are server-side translation services (Dispatcher) with licensed translators |
| SolidWorks PDM, PTC Windchill (for comparison)                                                        | the preview stream of the file                                                                                                                                                                                                                                                          | conversion tasks or CAD workers that run the licensed CAD program                                                                                                                                                 |
| Open-source tools, e.g. [`cad-preview`](https://github.com/cinderblock/cad-preview) (MIT, JavaScript) | read the preview image embedded in the file: Inventor files are OLE compound documents with a PNG in a stream, Solid Edge files OLE compound documents with a Windows DIB thumbnail; old SolidWorks files have a `PreviewPNG` stream, newer ones a 640 × 480 PNG in their own container | none                                                                                                                                                                                                              |

The common pattern:

1. the **thumbnail** is the image the CAD program stores in the file – the PLM
   only reads it;
2. a **3D view** is always produced by the CAD program itself or by the
   vendor's translator – on the user's computer at check-in (Inventor add-in,
   SEEC) or on a conversion server that holds a licence (Job Processor,
   Dispatcher).

## What is possible for an open-source project

| Option                                                                                                                        | Feasible?                                                                                                                                                                                                                                                                          | Decision                                                                                                                                                                           |
|-------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Read the embedded preview image on the server                                                                                 | yes – the container is the OLE compound file format documented by Microsoft ([MS-CFB], [MS-OLEPS]); only the image is read (PNG, or a DIB turned into a BMP), no proprietary geometry                                                                                              | **implement** – pure Rust with the `cfb` crate (MIT)                                                                                                                               |
| Read the native geometry on the server for a 3D view                                                                          | no – the formats are undocumented; readers exist only as vendor SDKs (Inventor Server / Design Automation, Parasolid, Solid Edge SDK) under commercial licences that cannot be redistributed with an open-source project, and reverse engineering is excluded by the project rules | **skip** – impossible due to licensing                                                                                                                                             |
| Conversion server with a licensed CAD program (like the Job Processor or the Dispatcher)                                      | needs a licensed CAD installation that converts for everybody; student and education licences are personal and meant for learning, not for a shared conversion service                                                                                                             | **skip** for the open-source project; an organisation with commercial licences can still do it outside TeamCraft (upload the STEP)                                                 |
| Export through the user's own CAD installation in the desktop app (like the Inventor add-in for Vault or SEEC for Teamcenter) | yes – on Windows, when every user has Inventor or Solid Edge installed under their own licence (student licences included, within their terms)                                                                                                                                     | **implement** – the desktop app asks the local CAD program for a STEP export next to the native file; the STEP is checked in with it and shown by TeamCraft's pure-Rust 3D preview |

How the desktop export works:

* **Inventor** (COM automation, the documented API also used by iLogic):
  open the document invisibly and `Document.SaveAs("…\part.stp", True)` –
  Inventor picks its STEP translator by the extension (for options the
  STEP translator add-in `{90AF7F40-0C01-11D5-8E83-0010B541CD80}` with
  `SaveCopyAs`).
* **Solid Edge**: `SolidEdgeTranslationServices.exe -i part.par -o part.stp
  -t step`, the command-line translator installed with Solid Edge; or COM
  automation (`SolidEdge.Application`, `Documents.Open`, `SaveAs` with a
  `.stp` name, like the interactive *Save As → STEP*).
* TeamCraft contains no Autodesk or Siemens component and redistributes
  nothing; it only starts features the user's own licence already includes.

## Licence notes

* **Autodesk education licences** are for learning, training and research –
  no commercial, professional or for-profit use. Universities read club use
  differently (the University of Freiburg, for example, lists club and
  association use as not covered). Each team has to check its own case with
  the current Autodesk terms or its university.
* **Solid Edge Student Edition** is free for active students and intended for
  academic course work; its files cannot be opened in commercial versions of
  Solid Edge.
* **Inventor Read-only Mode** (part of every Inventor installation) and **Inventor View** (with Vault or standalone)
  open files without consuming a
  licence, but they are viewers – they cannot be automated for exports.

## Status

* **Embedded preview images – implemented:** `src/thumbnail.rs` (OLE
  compound files through the `cfb` crate, PNG or DIB → BMP, cached in
  `previews/`), `GET /files/{id}/thumbnail` (web and REST API), shown on the
  revision page and in the desktop app. Tested with generated files in the
  layouts described above, **not yet with real Inventor, Solid Edge or
  SolidWorks files**. Update: tested with the NIST files in
  `tests/fixtures/cad` – Inventor 2021, NX 1980 and SOLIDWORKS 2018. In
  SOLIDWORKS 2015+ files the PNG is not stored as such but in a
  Deflate-compressed `PreviewPNG` section of their own container; it is
  inflated by `src/swpart.rs`.
* **STEP export through the local CAD program – implemented** in the desktop
  app (`desktop/src/cadexport.rs`): *STEP* button in the workspace and *Create STEP exports … first* at check-in;
  Inventor and Solid Edge through
  COM automation from Windows PowerShell. The Windows code compiles for
  `x86_64-pc-windows-gnu`, but it was **not run against a real Inventor or
  Solid Edge** – that needs a Windows computer with one of them.

## Sources

* Vault thumbnails come from the CAD application, Job Processor and DWF:
  [Autodesk Community – Vault ideas](https://forums.autodesk.com/t5/vault-ideas/thumbnail-preview-size-custom/idi-p/11833190)
* Inventor thumbnail as a property of the summary information:
  [Autodesk Community](https://forums.autodesk.com/t5/inventor-programming-forum/getting-the-part-assembly-thumbnail/m-p/7202910),
  [Inventor help – Thumbnails iProperties](https://help.autodesk.com/cloudhelp/2014/ENU/Inventor/files/GUID-CDF02C48-FC7C-4CE9-86B7-B816D1158B27.htm)
* Embedded previews of Inventor, Solid Edge and SolidWorks files:
  [cinderblock/cad-preview](https://github.com/cinderblock/cad-preview)
* SEEC creates JT at check-in, Dispatcher alternative:
  [Siemens support PL8016354](https://support.sw.siemens.com/en-US/okba/PL8016354/Use-Teamcenter-dispatcher-workflow-to-create-JT-files-for-Solid-Edge-datasets-instead-of-SEEC/index.html)
* Inventor STEP export through the API:
  [Autodesk Community – Save as STEP using iLogic](https://forums.autodesk.com/t5/inventor-programming-forum/save-as-step-using-ilogic/m-p/5770957/highlight/true),
  [STEP translator add-in](https://forums.autodesk.com/t5/inventor-programming-forum/osteptranslator/m-p/11346353)
* Solid Edge STEP export:
  [Translating Solid Edge files from the command line](https://blogs.sw.siemens.com/solidedge/Translating-Solid-Edge-Files-from-the-Command-Line/),
  [Solid Edge help – Save As other formats](https://www.soliddna.com/SEHelp/ST5/EN/i_v/save2a.htm)
* Licences:
  [Autodesk education plan FAQ](https://forums.autodesk.com/t5/student-room/autodesk-education-plan-faqs/m-p/11788065),
  [University of Freiburg – Autodesk](https://rz.uni-freiburg.de/en/services/procurement/software/AutoDesk),
  [Solid Edge Student Edition](https://resources.sw.siemens.com/en-US/download-solid-edge-student-edition),
  [Inventor Read-only Mode](https://help.autodesk.com/cloudhelp/2020/ENU/Inventor-Help/files/GUID-64995BEF-2E8F-4074-BEFC-116D295FC8BD.htm)
* Container formats: [MS-CFB] Compound File Binary File Format,
  [MS-OLEPS] Object Linking and Embedding Property Set Data Structures (Microsoft Open Specifications)

[MS-CFB]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cfb/
[MS-OLEPS]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-oleps/
