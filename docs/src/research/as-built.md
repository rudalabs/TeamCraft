# As-built records: hardware that differs from the CAD

Real hardware is rarely built exactly to the released design: a bore ends up
0.03 mm too large and is accepted, a purchased part is replaced by a
second-source part, a weld is reworked, a different alloy is used because the
specified one was not in stock. TeamCraft so far only knows the *as-designed*
side (items, revisions, BOMs). This page records how PLM and adjacent systems
model the *as-built* side and what TeamCraft adopts. Date: 2026-11-06.

## How other systems do it

| System / practice                        | Physical unit                                                          | Link to the design                                                              | Departures from the design                                                                                                                                                               |
|------------------------------------------|------------------------------------------------------------------------|---------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| PTC Windchill (MPMLink, Service)         | serialised *part instance* / *as-built* and *as-maintained* structures | instance of a part version; the as-built structure starts from the designed BOM | deviations and waivers as change objects; actual installed parts recorded in the instance structure                                                                                      |
| Siemens Teamcenter (Manufacturing / MRO) | *physical part* with serial or lot number                              | *as-designed* revision the unit was built to                                    | as-built structure records the actually installed physical parts; *deviation* and *nonconformance* objects with disposition                                                              |
| Aras Innovator                           | *Product Instance* / serialised part (as-built, as-maintained)         | refers to the part revision and its BOM                                         | quality items (nonconformance, CAPA) linked to the instance                                                                                                                              |
| ERP (SAP, Odoo, ERPNext)                 | serial numbers and lots on stock, installed base                       | material number, sometimes revision level                                       | quality notifications; not a structure comparison                                                                                                                                        |
| AS9100 / ISO 9001 practice               | serial-numbered article with a build record (traveller)                | drawing and revision built to                                                   | nonconformance report (NCR) with a **disposition**: *use as is*, *rework*, *repair*, *scrap* (often also *return to supplier*); *repair* and *use as is* need engineering (MRB) approval |

Common ground:

- **The unit is its own object**, identified by item and serial (or lot)
  number, not a revision of the design. Many units point to one design
  revision; a unit keeps pointing to the revision it was built to after the
  design moves on.
- **The as-built structure starts as a copy of the designed BOM** and is
  then edited to what was really installed; comparing the two shows the
  differences.
- **Each departure is a record of its own** with what was designed, what was
  built and a disposition. A unit with an undecided departure is not
  accepted/delivered.
- **As-maintained** (repairs and replacements in service) is the same
  structure over time: a new state of the unit's record, the old state stays
  for traceability.

## What TeamCraft adopts

- **Item type `as_built`** (numbers `AB-000001`): one item is one physical
  unit (or one lot). It has the normal lifecycle, revisions, files (photos,
  measurement reports, a 3D scan, the traveller) and change orders. A new
  revision of the as-built item is a new state of the same unit (rework,
  repair, retrofit = *as-maintained*); the old revision stays.
- **Build record per as-built revision:** the design revision it was built
  to (a part, assembly or software revision), serial or lot number, build
  date, note. The serial number is unique per design item. The design
  revision may be in work, in review or released; an obsolete one is refused.
  Like the basis links of a simulation, the record belongs to the revision:
  editable only while it is checked out, copied by *Revise*, frozen at
  release.
- **As-built BOM** = the normal BOM of the as-built revision. *Record a built
  unit* on a part or assembly creates the as-built item with the design's BOM
  copied in; the existing cross-item BOM comparison shows *design → as-built*.
- **Deviations** per as-built revision: kind (dimension, material,
  component, process, other), feature, *as designed*, *as built*,
  disposition (*open*, *use as is*, *rework*, *repair*, *scrap*), note.
  Values are text (`12.000 +0.018/0` → `12.035`), because tolerance
  notations vary too much for one numeric model.
- **Release rule:** a change order with an as-built revision cannot be
  submitted while that revision has no build record or an *open* deviation.
  The engineering decision (MRB) is the change order's review itself.
- **Design side:** a *Built units* card on the design revision lists the
  units built to any of its revisions, with serial, built-to revision, open
  deviations and a marker when a newer design revision is released. A design
  item that units were built to cannot be deleted.
- Later, not now: change propagation to built units (retrofit campaigns),
  serial numbers on as-built BOM lines (installed serialised components),
  numeric tolerance checks, import of measurement reports.

## Sources

- AS9100D, clause 8.7 *Control of nonconforming outputs* (dispositions
  use-as-is, repair, rework, scrap; customer approval of use-as-is/repair).
- Vendor documentation of PTC Windchill, Siemens Teamcenter and Aras
  Innovator on serialised/as-built/as-maintained product structures (public
  product overviews; no licensed material was used).
