# As-built records

Real hardware is rarely built exactly to the design: a bore ends up a little
too large and is accepted, a purchased part is replaced by a second-source
part, a weld is reworked. An **as-built record** keeps what one physical
unit really is, next to the design it was built to. The background and how
other systems solve it are on the research page [As-built
records](../research/as-built.md).

## A unit is an item

An item of type **As-built** (numbers `AB-000001`, …) is one physical unit
or one lot. It has the usual lifecycle, revisions, check-out/check-in, files (photos, measurement reports, a 3D scan,
the traveller) and change orders. A
new revision of the as-built item is a new state of the *same* unit – after
a rework, a repair or a retrofit; the older revision stays for traceability.

On top of that an as-built revision carries

* a **build record** – the design revision (part, assembly or software) the
  unit was built to, the serial or lot number, the build date and a note;
* **deviations** – how the unit departs from its design and what was decided;
* its **BOM** – the regular BOM of the revision, holding what was really
  installed.

Like BOM lines, build record and deviations belong to the revision: they
change only while it is checked out by you, *Revise* copies them, and they
are frozen at release.

## Recording a built unit

On a part, assembly or software revision, the **Built units** card has *Record a built unit*: serial or lot number,
build date, optionally the
item number, name, note and project group of the new as-built item. TeamCraft
creates the as-built item (revision A, checked out by you) with its build
record and, for an assembly, a copy of the design's BOM – in one step. Edit
that BOM to what was really installed; *Compare BOM with the design* shows
the differences.

An as-built item created with *New item* starts without a build record;
enter it in the **Built to** card of the revision (item number and revision
of the design – empty means its latest revision).

Rules:

* The **serial number is unique per design item** (letters compared without
  case). The revisions of one unit share it; another unit with the same
  serial is refused and named (or called "another unit" when it is in a
  group you may not read).
* The design revision may be **in work, in review or released** –
  prototypes are often built before the release. An **obsolete** design
  revision is refused for a new unit or a new build record ("record the unit
  against a current revision of the design"). A unit that was already built
  to a revision that later became obsolete keeps its record and may still
  correct serial, date and note.
* The as-built item may live in another project group than the design.
  Copying the BOM out of a restricted group needs a manager of that group,
  as for *Save as*.

## Deviations and their disposition

Each deviation has a **kind** (dimension, material, component, process,
other), the **feature** (e.g. *Bore Ø12 H7*), what was **designed** and what
was **built** (text, e.g. `12.000 +0.018/0` → `12.035`), a note and the **disposition**:

| Disposition | Meaning                                                 |
|-------------|---------------------------------------------------------|
| Open        | not decided yet – blocks the release                    |
| Use as is   | accepted as built (usually an engineering/MRB decision) |
| Rework      | brought back to the design                              |
| Repair      | made usable, but not to the design                      |
| Scrap       | the unit (or the affected part) is not used             |

An as-built revision holds at most 1000 deviations.

## Release

A change order with an as-built revision cannot be submitted while that
revision has **no build record** or an **open deviation**. The engineering
decision on the deviations is the review of the change order itself.

A project group can ask for more: with *A reviewer confirms each "use as is"
deviation* in its review policy, a reviewer of the change order confirms every *use as is* concession on its own
(*Confirm* in the deviation table) before
their approval counts, and the person who recorded the deviation cannot
confirm it. See [Read permissions and review policies](../admin/permissions.md).

## The design side

The **Built units** card of a part, assembly or software revision lists the
units built to any revision of the item – the latest revision of each unit,
with serial number, the design revision it was built to, open deviations and
a *Newer design* marker when a later design revision was released after the
one the unit was built to. Units in groups you may not read are only
counted. A design item that units were built to cannot be deleted.

When you may read the unit but not the design (the design is in a restricted
group), the build record shows a "restricted item" placeholder instead of the
design's number, name and revision.

## What is not included

* **Pack and Go** (`…/package`) of an as-built revision contains its files,
  BOM and manifest like any other revision, but **not** the build record and
  the deviations; read those with `GET …/as-built` (REST) or `get_revision`
  (MCP) and keep the export next to the ZIP if another system needs them.
* Not yet: retrofit campaigns (change propagation to built units), serial
  numbers on as-built BOM lines, numeric tolerance checks and the import of
  measurement reports.

## REST API and assistants

The endpoints are listed in the [REST API](../reference/api.md#as-built-records)
reference. The MCP tool `get_revision` returns `as_built` for an as-built
revision and `built_units` for a design ([MCP server](../reference/mcp.md)).
