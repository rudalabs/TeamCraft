# BOM, references and change propagation

## Bill of materials

A revision's BOM lists child items with find numbers, quantities and notes.
The BOM page expands the structure over all levels with mass and cost
roll-up and three **revision rules**:

| Rule | Resolves each child to |
|---|---|
| Latest | its newest revision, whatever the state |
| Latest released | its newest released revision |
| As released | the exact revision the parent was released with (e.g. the as-built configuration) |

Where-used shows the parents of an item. Cycles are rejected. The BOM
exports as CSV, and **Pack and Go** downloads a revision with the files of
its whole structure as ZIP (`<item>_<revision>/`, `bom.csv`,
`manifest.json` with SHA-256 per file).

## References

Besides the BOM, revisions reference other items: *drawing of*,
*specification for*, *references*. CAD files are scanned for the files they
reference (assemblies, drawings, STEP, KiCad; ASCII and UTF-16 paths as
written by SolidWorks, Inventor, Solid Edge, CATIA, …) and matched against
the vault: referenced files that are *not in TeamCraft* are flagged and the
missing BOM lines are suggested.

## Change propagation

When a change releases a new revision (or a revision becomes obsolete), every
dependent – via BOM, references or CAD file references – is flagged
**Check needed**. A CAD editor acknowledges it with a note (*form, fit and
function unchanged*) or revises and releases the dependent, which resolves
the check and propagates one level further. Every change shows this
**impact analysis** before it is approved.
