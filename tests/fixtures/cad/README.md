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

## Files

| Folder | CAD programs | Contents |
|---|---|---|
| `nist/ctc-01/` | Inventor 2021, Siemens NX 1980, SolidWorks MBD 2018, Creo 10 (iteration `.prt.5`), CATIA V5-6R2019, STEP AP203 and AP242 | the combined test case CTC 01 in every format |
| `nist/ftc-11/` | Inventor 2021, SolidWorks MBD 2018, STEP AP203 (7.6 kB) | small files for fast tests |
| `nist/mtc-assembly/solidworks/` | SolidWorks (and an ACIS `.SAT`) | the box assembly: `.SLDASM` with its eight parts |
| `nist/mtc-assembly/nx/` | Siemens NX (and a JT) | the same box assembly: assembly `.prt` with its eight parts |

Not covered yet: **Solid Edge** – no openly licensed files were found – and
Fusion, FreeCAD or KiCad projects.

## What is tested

`tests/cad_fixtures.rs` (no database needed):

* every file is unchanged (size and SHA-256) and recognised as its format;
* the Inventor files carry a preview image (PNG);
* the NX assembly references all eight part files;
* the small STEP files get a 3D preview.

Gaps found with these files are ignored tests in the same file – run them
with `cargo test --test cad_fixtures -- --ignored` – and are listed under
*Fixes* in [docs/OPEN_WORK.md](../../../docs/OPEN_WORK.md).
