# Openly licensed CAD files for end-to-end tests

**Question:** where can TeamCraft get real native files of the common CAD
programs – parts and assemblies – that may be redistributed in a public
MPL-2.0 repository, to test the import, the format detection, the preview
images, the reference scan and the STEP preview end to end? Investigated on
2026-11-04.

## Sources checked

| Source | What it offers | Licence | Usable |
|---|---|---|---|
| **NIST MBE PMI validation and conformance testing** | test parts CTC 01–05, FTC 06–11, STC, HTC in native CATIA V5, Siemens NX, Autodesk Inventor, PTC Creo and SolidWorks plus STEP AP203/AP242 and PDF; the **MTC box assembly** in NX (with JT) and SolidWorks (with ACIS `.SAT`) | "The test cases, CAD models, and STEP files can be used without any restrictions." NIST asks for an acknowledgement and does not allow its logo in promotional material | **yes** |
| GrabCAD and similar community libraries | many native files | no licence per file, terms of the site only | no |
| `inventor-kit` (PyPI) | reads `.ipt`/`.iam`, files not distributed | PolyForm Noncommercial since 0.3.0 | no |
| SolidWorks repositories of open hardware projects | `.SLDPRT`/`.SLDASM` | mostly no licence, CC BY-SA or unverified "MIT" claims | no (not verifiable / share-alike) |
| Solid Edge community samples | automation code samples | no model files with a stated licence found | no |

**Solid Edge** is the gap: no openly licensed native Solid Edge files were
found. Options: create small test parts and an assembly with Solid Edge
(Community Edition) and publish them under CC0, or ask the Solid Edge
community for donated files.

## Decision

A curated subset of the NIST files goes into `tests/fixtures/cad/nist/`
(about 30 MB), with the NIST terms, the acknowledgement, the source of every
file and its SHA-256 in `tests/fixtures/cad/README.md`:

* **CTC 01** in every native format: Inventor 2021 `.ipt`, NX 1980 `.prt`,
  SolidWorks 2018 `.SLDPRT`, Creo 10 `.prt.5` (a Creo iteration), CATIA V5
  `.CATPart`, and STEP AP203 and AP242;
* **FTC 11** as small files for fast tests: Inventor `.ipt`, SolidWorks
  `.SLDPRT` and a 7.6 kB STEP AP203;
* the **MTC box assembly** in SolidWorks (`.SLDASM`, parts, `.SAT`) and in
  NX (assembly and part `.prt`, `.jt`) – real assemblies for the reference
  scan and the folder import.

A test (`tests/cad_fixtures.rs`) checks every file against its SHA-256 and
the expected TeamCraft format, and records what the heuristics really find
in these files: preview images (Inventor, SolidWorks), referenced part files
(SolidWorks and NX assemblies) and the STEP 3D preview.

## Sources

* [NIST – Download free CAD models and STEP files](https://www.nist.gov/el/systems-integration-division-73400/mbe-pmi-validation-and-conformance-testing/download-free-cad)
  (terms quoted from the *Disclaimers* section of that page),
  [project page](https://www.nist.gov/el/systems-integration-division-73400/mbe-pmi-validation-and-conformance-testing-project),
  [data catalog entry](https://catalog.data.gov/dataset/nist-cad-models-and-step-files-with-pmi),
  MTC: [Design, Manufacturing, and Inspection Data for a Box Assembly](https://smstestbed.nist.gov/tdp/mtc/)
* [inventor-kit on PyPI](https://pypi.org/project/inventor-kit/)
* [SolidEdgeCommunity/Samples](https://github.com/SolidEdgeCommunity/Samples)

## Results with the files (2026-11-04)

What TeamCraft found in the NIST files (`tests/cad_fixtures.rs`):

* **Format detection:** all 32 files are recognised correctly – NX parts by
  their `SPLMSSTR` header, the Creo iteration `.prt.5`, CATIA V5, JT and ACIS
  included.
* **Preview images:** Inventor 2021 parts carry a PNG of about 26 kB, which
  is found. SolidWorks 2018 files carry none that can be found: since 2015
  SolidWorks keeps its streams in compressed segments of its own container.
* **Reference scan:** the NX assembly names all eight part files. The
  SolidWorks 2018 assembly names none (compressed, see above). False
  references: NX parts report earlier file names of their save history and
  binary noise, the STEP header its export name (`FILE_NAME`), JT files
  noise.
* **3D preview:** the small STEP files are tessellated.

The gaps are ignored tests in `tests/cad_fixtures.rs` and fixes in
`docs/OPEN_WORK.md`.
