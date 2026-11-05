# BOM, references and change propagation

## Bill of materials

A revision's BOM lists child items with find numbers, quantities and notes.
The BOM page expands the structure over all levels with mass and cost
roll-up and three **revision rules**:

| Rule            | Resolves each child to                                                            |
|-----------------|-----------------------------------------------------------------------------------|
| Latest          | its newest revision, whatever the state                                           |
| Latest released | its newest released revision                                                      |
| As released     | the exact revision the parent was released with (e.g. the as-built configuration) |

Where-used shows the parents of an item. Cycles are rejected. The BOM
exports as CSV and as Excel workbook (which the BOM import reads back), and
**Pack and Go** downloads a revision with the files of its whole structure
as ZIP (`<item>_<revision>/`, `bom.csv`, `manifest.json` with SHA-256 per
file).

## Comparing BOMs

*Compare…* on the BOM page lists the lines that were **added**, **removed**
or **changed** (quantity, find number, notes) between two revisions – by
default the previous revision of the same item; any other revision can be
chosen, and the API also compares by code (`GET …/revisions/B/bom/compare?with=A`).

*Levels: All levels* compares the whole structure: every child is resolved
with the chosen revision rule, the children of two matched lines are
compared the same way, and an added or removed sub-assembly brings its
whole sub-tree. A child that now resolves to another revision counts as
changed – with *As released*, comparing two released revisions shows
exactly which parts went into the new one. In the API: `depth=all` and
`rule=latest|released|as_released`. The Excel download contains the level
and the revisions too.

## References

Besides the BOM, revisions reference other items: *drawing of*, *specification for*, *references*. CAD files are scanned
for the files they
reference (assemblies, drawings, STEP, KiCad; ASCII and UTF-16 paths as
written by SolidWorks, Inventor, Solid Edge, CATIA, …; NX parts and
SOLIDWORKS 2015+ files through their containers, see
[Formats](../reference/formats.md)) and matched against
the vault: referenced files that are *not in TeamCraft* are flagged and the
missing BOM lines are suggested.

## Change propagation

When a change releases a new revision (or a revision becomes obsolete), every
dependent – via BOM, references, CAD file references or as the basis of a
[simulation](simulation.md) – is flagged **Check needed**. A CAD editor acknowledges it with
a note (*form, fit and
function unchanged*) or revises and releases the dependent, which resolves
the check and propagates one level further. Every change shows this **impact analysis** before it is approved.

*Dependents to check* on the dashboard (and in the desktop app) groups the
open checks by the dependent revision: one header per assembly, drawing or
simulation (*ASM-000002/A Electronics enclosure – 4 changes to check*) with
**Open**, **Revise** and **Acknowledge all…**, and one compact row per source
below it – the item that changed, *released* or *obsolete* via which change,
and the link (BOM line, CAD file reference, drawing, specification, reference,
simulation basis) – each with its own **Acknowledge…**. **Acknowledge all…**
resolves exactly the checks shown with one note, in one step; checks resolved
by someone else in the meantime are skipped, and every acknowledged check is
recorded in the item's history. The revision page shows its open checks the
same way.
