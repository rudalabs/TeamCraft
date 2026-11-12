# CAD test files

Real native files of several CAD programs for end-to-end tests of TeamCraft:
format detection, preview images, reference scan, STEP 3D preview and the
folder import of the desktop app. How they were chosen:
[docs/src/research/cad-test-files.md](../../../docs/src/research/cad-test-files.md).

## Source and terms

All files are unchanged copies from the **MBE PMI Validation and Conformance
Testing** project of the National Institute of Standards and Technology
(NIST), U.S. Department of Commerce:

| Archive | Download (2026-11-04) | SHA-256 of the archive |
|---|---|---|
| `NIST-FTC-CTC-PMI-CAD-models.zip` | <https://www.nist.gov/document/nist-ftc-ctc-pmi-cad-models> | `17e14dbf207f447f16e0182a7eda5254ee19afff591dd409b8b510103dd05d10` |
| `NIST-PMI-STEP-Files.zip` | <https://www.nist.gov/document/nist-pmi-step-files> | `8fa78429e6d8d9b0d7681d223b6aa9ec98c3772185c55b1a0e3679b21c181911` |
| `NIST-MTC-Assembly.zip` | <https://www.nist.gov/document/nist-cad-models-mtc-assembly> | `9aeb53e54f682ea1732857d06a7f0513c71667a2d84407396325fa6ce5340bbc` |

Terms, from the NIST disclaimer on the
[download page](https://www.nist.gov/el/systems-integration-division-73400/mbe-pmi-validation-and-conformance-testing/download-free-cad):
*"The test cases, CAD models, and STEP files can be used without any
restrictions. Their use in other software or hardware products does not
imply a recommendation or endorsement of those products by NIST."*

**Acknowledgement** (requested by NIST): CAD models and STEP files courtesy
of the National Institute of Standards and Technology (NIST), MBE PMI
Validation and Conformance Testing project; the box assembly comes from the
NIST project *Design, Manufacturing, and Inspection Data for a Box Assembly*
(<https://smstestbed.nist.gov/tdp/mtc/>).

[`manifest.tsv`](manifest.tsv) lists every file with its SHA-256, its size,
the TeamCraft format it must be recognised as and its path inside the NIST
archive.

## Rhino models (rhino3dm, MIT)

`rhino3dm/` holds three unchanged test models of McNeel's
[rhino3dm](https://github.com/mcneel/rhino3dm) library (`tests/models`,
branch `main`, downloaded 2026-11-08), MIT licensed – the licence text
is in [`rhino3dm/LICENSE`](rhino3dm/LICENSE) and must stay with the files:

| File | Contents |
|---|---|
| `mesh.3dm` | a mesh object (Rhino 8) |
| `sphereDecals.3dm` | a polysurface with its stored render mesh |
| `subd_creases.3dm` | a SubD with crease edges and a corner (no stored mesh) |

## Files

| Folder | CAD programs | Contents |
|---|---|---|
| `nist/ctc-01/` | Inventor 2021, Siemens NX 1980, SolidWorks MBD 2018, Creo 10 (iteration `.prt.5`), CATIA V5-6R2019, STEP AP203 and AP242 | the combined test case CTC 01 in every format |
| `nist/ftc-11/` | Inventor 2021, SolidWorks MBD 2018, STEP AP203 (7.6 kB) | small files for fast tests |
| `nist/mtc-assembly/solidworks/` | SolidWorks (and an ACIS `.SAT`) | the box assembly: `.SLDASM` with its eight parts |
| `nist/mtc-assembly/nx/` | Siemens NX (and a JT) | the same box assembly: assembly `.prt` with its eight parts |
| `openskp/` | SketchUp 3 to 2026 (`.skp`, 31 classic files of SketchUp 3–2020, 8 of 2021+) | test files of OpenSKP (see below): nested and scaled components, curves, layers, hidden geometry, the same models saved in several versions |

### SketchUp files from OpenSKP

`openskp/` holds unchanged copies of the test files of OpenSKP
(<https://github.com/iamahsanmehmood/openskp>, commit `e7b4674`,
`packages/cpp/tests/fixtures/` and `examples/web-viewer/samples/`), under
the MIT licence, Copyright (c) 2024-2026 Ahsan Mehmood – see
[`openskp/LICENSE`](openskp/LICENSE). Left out: the 5.5 MB
`coedge_orientation_regression.skp`. `manifest.tsv` names the path of each
file in that repository.

Not covered yet: **Solid Edge** – no openly licensed files were found – and
Fusion, FreeCAD or KiCad projects.

## What is tested

`tests/cad_fixtures.rs` (no database needed):

* every file is unchanged (size and SHA-256) and recognised as its format;
* the Inventor files carry a preview image (PNG);
* the NX assembly references all eight part files;
* every STEP file (AP203 and AP242) gets a complete 3D preview; the PMI of the AP242 file is not shown as geometry.
* every Rhino model gets a closed 3D preview in millimetres (mesh, B-rep render mesh, subdivided SubD).
* every SketchUp file with faces (SketchUp 3 to 2026) gets a 3D preview with the triangle counts and boxes of the prototype, the same model saved by different versions gives the same triangles, files without faces say so; damaged SketchUp files never panic.

Gaps found with these files are ignored tests in the same file – run them
with `cargo test --test cad_fixtures -- --ignored` – and are listed under
*Fixes* in [docs/OPEN_WORK.md](../../../docs/OPEN_WORK.md).
