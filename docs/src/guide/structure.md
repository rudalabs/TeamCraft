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

### Working with a large structure

For a big product (hundreds or thousands of lines across several project
groups) the BOM page offers:

* **Fold and unfold** – every line that has lines below it has a ▾ / ▸ button (keyboard operable, `aria-expanded`);
  *Collapse all* and *Expand all* sit
  above the table. This needs JavaScript; without it every line stays
  visible.
* **Levels** – *Levels: 1, 2, 3, 4, 5, All* (`?depth=n`) lists only the first
  n levels, also without JavaScript, and the link can be shared. A line at the
  last level shown has a `+n` marker with the number of lines hidden below it.
  Mass and cost always cover the whole structure, not just the levels shown.
  It combines with the revision rule (`?rule=`).
* **Group column** – the project group of each line, linked to the group page.
  A line you may not see (a restricted group) stays a placeholder and shows no
  group.
* **Group filter** – *Group* (`?group=CODE`, only groups that occur in the
  structure are offered) keeps the lines of that group (highlighted) and
  the path from the top to them, so they stay in context; other lines
  below a match are left out unless they lead to a match too. With a level
  limit as well, the filter applies first. An unknown or hidden code gives
  "not found".

The CSV and Excel exports and Pack and Go ignore levels and group: they
always contain every line. The desktop app's structure tab does not have
these controls yet.

Where-used shows the parents of an item; *All levels* on the revision page
opens the where-used page, which walks up through the sub-assemblies (1, 2,
3, 5 or all levels). Cycles are rejected. The BOM
exports as CSV and as Excel workbook (which the BOM import reads back), and **Pack and Go** downloads a revision with
the files of its whole structure
as ZIP (`<item>_<revision>/`, `bom.csv`, `manifest.json` with SHA-256 per
file).

## Creating a BOM from the CAD references

An assembly uploaded as a file has no BOM, although the server already knows
which part files it uses (the *References* tab). **Create BOM from CAD
references…** on the BOM card of the revision (also offered as a hint while
the BOM is empty) shows what the BOM would be, one row per child:

* item, name, **quantity** and where it comes from – *CAD instances* (NX:
  the placed instances of the component, hidden layers left out; SOLIDWORKS:
  the instances in the component tree without *exclude from BOM*) or
  *assumed 1* (the file does not tell; the line gets the note *quantity not
  known from the CAD file – check it*);
* the CAD file that references it;
* a status: *new line* (ticked), *already in the BOM*, *differs* (BOM 2, CAD
  4 – changed only when you tick it and *Also change the quantity*), or
  *circular* (refused, as when adding a line by hand).

Only assembly files count (NX files only when they have component
instances); a drawing's link to its model stays a *drawing of* reference.
Referenced files TeamCraft does not have are listed as *not in TeamCraft* and
never created; parts in project groups you cannot see are only counted.
*Add the ticked lines* needs the revision checked out by you (the page
offers *Check out and create*) and adds all lines in one transaction. The
REST API has the same two steps: `GET …/bom/from-references` returns the
plan, `POST …/bom/from-references` with
`{"children": ["PRT-000004"], "update_quantities": false}` applies it.

## Comparing BOMs

*Compare…* on the BOM page lists the lines that were **added**, **removed**
or **changed** (quantity, find number, notes) between two revisions – by
default the previous revision of the same item; any other revision can be
chosen, and the API also compares by code (`GET …/revisions/B/bom/compare?with=A`).

*Levels: All levels* compares the whole structure: every child is resolved
with the chosen revision rule, the children of two matched lines are
compared the same way, and an added or removed sub-assembly brings its
whole sub-tree. A child that now resolves to another revision counts as
changed. When one of the two revisions is released, the default rule is *As released*: a released parent uses the child
revisions it was released
with, an in-work one the newest released children, so comparing two
revisions shows exactly which parts went into the new one; otherwise the
default is *Latest*. *Latest* and *Latest released* resolve a child by its
item alone – both sides then get the same child revision and sub-tree, and
only the direct lines can differ. In the API: `depth=all` and
`rule=latest|released|as_released` (same default). The Excel
download contains the level and the revisions too.

## Alternates, supersession and replacing a part

The card *Alternates and replacement* on the revision page holds two
relations of the item itself (not of one revision – they stay true for old
assemblies too). Changing them needs edit rights in the item's project group.

- **Alternates** are items that may be built in instead of this one in every
  assembly – same form, fit and function, e.g. a second source of a purchased
  part. *Two-way* (the default) also makes this item an alternate of the other
  one; that needs edit rights in the other item's group too, otherwise add it
  one-way. The other item then lists it under *may be replaced by*.
- **Supersession** says: do not use this item for new designs any more, use
  its successor. An item has at most one successor, and a chain of successors
  never leads back to the item. The revision page of a superseded item shows a
  notice, BOM lines that use a superseded item are marked *Superseded by …*,
  and adding one to a BOM still works but says so.
  Superseding changes no BOM and no lifecycle state; making the old revisions
  obsolete stays a change order's job.

**Replace in assemblies** on the where-used page puts another item (by
default the successor, otherwise an alternate) in place of this one. The
preview lists every assembly whose *latest* revision uses the item directly
and what will happen to it:

| Latest revision of the assembly                | Result                                                       |
|------------------------------------------------|--------------------------------------------------------------|
| in work, not checked out or checked out by you | the line is replaced (quantity, find number and notes stay)  |
| in work, checked out by somebody else          | left alone – ask them, or replace it there yourself later    |
| in work, in a group you cannot edit            | left alone                                                   |
| in review                                      | left alone until the review is finished or rejected          |
| released                                       | needs a new revision – revise it (usually in a change order) |
| would contain itself through the replacement   | left alone (circular BOM)                                    |

A revision that was not checked out is changed directly, like check out,
edit, check in; one checked out by you stays checked out. Older revisions keep
their BOM. Every replaced line is recorded in the history of the assembly (`bom.replace`). Assemblies in groups you
cannot see are only counted.

## References

Besides the BOM, revisions reference other items: *drawing of*, *specification for*, *references*. CAD files are scanned
for the files they
reference (assemblies, drawings, STEP, KiCad; ASCII and UTF-16 paths as
written by SolidWorks, Inventor, Solid Edge, CATIA, …; NX parts and
SOLIDWORKS 2015+ files through their containers, see
[Formats](../reference/formats.md)) and matched against
the vault: referenced files that are *not in TeamCraft* are flagged and the
missing BOM lines are suggested. The other direction warns before damage:
removing a file that CAD files of other items still reference names them
(up to three, then a count) in the confirmation, so a part still used in an
assembly or drawing is not deleted by accident. Deleting an item is refused
while it is used in a BOM.

## Change propagation

When a change releases a new revision (or a revision becomes obsolete), every
dependent – via BOM, references, CAD file references or as the basis of a
[simulation](simulation.md) – is flagged **Check needed**. A CAD editor acknowledges it with
a note (*form, fit and
function unchanged*) or revises and releases the dependent, which resolves
the check and propagates one level further. Every change shows this **impact analysis** before it is approved.

*Dependents to check* on the dashboard (and in the desktop app) groups the
open checks by the dependent revision: one header per assembly, drawing or
simulation (*ASM-000002/A Electronics enclosure – 4 changes to check*) with **Open**, **Revise** and **Acknowledge
all…**, and one compact row per source
below it – the item that changed, *released* or *obsolete* via which change,
and the link (BOM line, CAD file reference, drawing, specification, reference,
simulation basis) – each with its own **Acknowledge…**. **Acknowledge all…**
resolves exactly the checks shown with one note, in one step; checks resolved
by someone else in the meantime are skipped, and every acknowledged check is
recorded in the item's history. The revision page shows its open checks the
same way.
