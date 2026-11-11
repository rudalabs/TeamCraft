# Instance counts of Inventor assemblies

Question: can TeamCraft tell how often an Autodesk Inventor assembly (`.iam`)
places each part or sub-assembly – the BOM quantity – without Inventor, the
way it already does for SOLIDWORKS, NX and STEP assemblies (#247)? Until now
every Inventor BOM line from the CAD references had quantity 1 with the note
*check the quantity*. Date: 2026-11-11.

## Test data

32 assemblies of one real Inventor project (not published), saved with
Inventor 2025 to 2027: 390 occurrences of 190 referenced files, among them
Content Center standard parts placed up to 10 times, Design Accelerator
assemblies, mirrored parts and suppressed occurrences. The expected counts
came from Inventor 2027 itself, read through its COM API
(`ComponentDefinition.Occurrences`, `ReferencedDocumentDescriptor`,
`Suppressed`, `BOMStructure`) with an invisible Inventor instance that
opened every assembly read-only. The files themselves cannot be published,
so the unit tests use synthetic files with the same layout.

## Findings

An `.iam` is a compound file (MS-CFB). The stream `UFRxDoc` holds a table
with one *file descriptor* per referenced file:

| Field                  | Content                                                                   |
|------------------------|---------------------------------------------------------------------------|
| `u32 n`, `n` × UTF-16  | full path of the referenced file                                          |
| `u32`                  | kind of the path (`0x43` workspace, `0x4d` library, `0x2a` older paths)   |
| `u32 m`, `m` × UTF-16  | workspace or library name (e.g. *Content Center Files*)                    |
| `u16`                  | flags (1 workspace, 3 library)                                            |
| `u32 k`, `k` × UTF-16  | display name, may be empty                                                |
| 6 bytes                | always 0                                                                  |
| 2 × 16 bytes           | identifiers (not found anywhere else in the file)                          |
| `u32`, `u16`           | unknown; the `u16` is always 0                                            |
| `u16`                  | **number of occurrences of the file in this assembly**                     |
| 10 bytes               | then the next descriptor                                                  |

* The last `u16` matched Inventor's own occurrence count for **all 190
  files** of the 32 assemblies, standard parts included.
* The stream also keeps copies of the table from earlier saves, some with
  older paths; within a table the descriptors follow each other at exactly
  10 bytes (214 of 214), between tables there is a large gap. Only the first
  table is current.
* The table does not tell suppressed occurrences apart: Inventor's BOM
  leaves them out, the count includes them. In the test data that changed 6
  of 190 counts (parts whose occurrences were all or partly suppressed).
  The suppression state lives in the assembly segment (`AmDcSegment`), which
  is not decoded.
* The occurrence names (`Bolt:3`) are no help: they are renamed freely and
  stored elsewhere.

## Decision

`src/invasm.rs` reads the first descriptor table of `UFRxDoc` and returns
(file name, occurrences); `cadrefs::instance_counts` uses it for
`inventor-assembly` like the other formats, so the folder import, *Quantities
from CAD…* and the BOM-from-references page get Inventor quantities.
Anything that does not fit the layout is skipped, never guessed: a file
without a descriptor has no count (quantity 1, *check the quantity*), as
before. No migration: *Quantities from CAD…* reads the counts from the
stored file, so assemblies imported earlier get them at once; their stored
counts (`file_references.instances`) are refreshed by `teamcraft
rescan-references --format inventor-assembly` (a migration that marks them
for the startup scan would change v0.1.0 data, which the upgrade test
forbids).

Later, if needed: the suppression state from `AmDcSegment`, Inventor
*Reference* and *Phantom* BOM structures (also in the assembly segment), and
Solid Edge assemblies (no test files yet).
