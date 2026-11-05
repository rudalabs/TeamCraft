# Concepts

## Items and revisions

An **item** is a part, assembly, document, software component or simulation
with a unique number – generated from the numbering scheme of its project
group (built in: `PRT-000042`, `ASM-…`, `DOC-…`, `SW-…`, `SIM-…`) or typed in
by hand. Its state of development is kept in **revisions** `A, B, …, Y,
AA, …` (I, O, Q, S, X and Z are skipped, as in ASME Y14.35). A revision
carries the attributes (name, description, material, mass, unit cost), the
files and the bill of materials.

## Lifecycle

```text
 WIP ──submit──▶ In Review ──all reviewers approve──▶ Released ──▶ Obsolete
  ▲                 │
  └──reject/withdraw┘
```

| Transition           | How                                                                               | Who                                                                      |
|----------------------|-----------------------------------------------------------------------------------|--------------------------------------------------------------------------|
| WIP → In Review      | submit a change order containing the revision                                     | the change's author (CAD editor of the item's project group) or an admin |
| In Review → Released | automatically when every reviewer approved – all revisions of the change together | the assigned reviewers, never the author (four-eyes principle)           |
| In Review → WIP      | a reviewer rejects with a reason, or the author withdraws                         | reviewers; author or admin                                               |
| Released → Obsolete  | *Mark obsolete* (final)                                                           | CAD editors of the project group, admins                                 |

Only WIP revisions change, and only while they are checked out. Released
data never changes – further work happens in a new revision (*Revise*).

## Project groups and roles

| Role       | May                                                                                                 |
| Viewer     | read internal project groups and the restricted groups they belong to, raise and discuss change requests |
| Viewer     | read everything, raise and discuss change requests                                                  |
| CAD editor | create and change items of the project groups they belong to, create project groups, review changes, triage and decide change requests on their groups |
| Admin      | everything, plus users, all project groups and lock overrides                                       |

Every item belongs to a **project group** – a product or product line, a
machine, a customer project, a building, a vehicle or a rover. CAD editors start their own groups and become their **managers**:
managers edit the group and add or remove members. A group can also be bound
to a group of the identity provider (administrators only); its members are
then synchronised at every single sign-on.

A group is **internal** (everybody reads it) or **restricted**: then only its
members – viewers included – and administrators see the group, its items,
files and the changes about them; for everybody else it does not exist.
Links from other groups show a *Restricted item* placeholder. Managers switch
this on the group's page; see
[Read permissions and restricted groups](../admin/permissions.md).

## Item numbers

Each project group has a **numbering scheme** for the numbers TeamCraft
generates when the number field is left empty – in the web UI, the REST API,
the BOM import and *Save as*. Managers set it on the group's page, where
*Preview* shows the next number per item type before anything is saved.

| Placeholder | Becomes                                                                 |
|-------------|-------------------------------------------------------------------------|
| `{PROJECT}` | the project code, e.g. `ROVER`                                          |
| `{TYPE}`    | `PRT`, `ASM`, `DOC` or `SW`                                             |
| `{YYYY}`    | the year of creation (UTC); `{YY}` its last two digits, `{MM}` the month |
| `{SEQ:n}`   | the sequence, zero-padded to *n* digits (1–12); exactly once            |

Everything else is literal text from `A–Z 0–9 . _ -`. The built-in scheme is
`{TYPE}-{SEQ:6}`; `{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}` gives `ROVER-PRT-2026-0007`.
Every distinct expansion counts on its own: a scheme with `{YYYY}` starts at 1
each year, one without `{TYPE}` shares the sequence between all item types,
and groups with the built-in scheme share its counters. Changing the padding
width keeps counting. When the next number is already taken (typed in by
hand or imported), the counter continues after the highest existing number
of the same pattern – so a new scheme can take over an existing numbering
convention – and existing items never change their numbers.

## Attributes and classification

Besides name, description, material, mass and unit cost, a revision can
carry **custom attributes** that administrators define under
*Admin → Attributes*: a label, a key for the API (`vendor`, `length_mm`), a
type – text, number, yes/no or a choice list – an optional unit, the item
types it applies to and its position in the forms. Values are edited with
the other attributes while the revision is in work, frozen on release and
copied by *Revise* and *Save as*. Key and type are fixed once created;
deactivating an attribute stops offering it, stored values stay visible.

Every item also has a **classification** in its master data: *in-house*
(the default), *purchased* or *standard part* – it can change at any time,
like the project group.

The item list filters by classification and by one attribute: text and
choices match a part of the value, yes/no attributes `yes` or `no`, numbers
`12`, `>=10`, `<=20` or `10..20`; an empty value finds every item that has
the attribute. The search box also finds attribute values.

The **BOM import** fills both for the items it creates: a *BOM structure* or
procurement column sets the classification (*Normal*, *Phantom* → in-house;
*Purchased*, *Kaufteil* → purchased; *Content Center*, *Normteil* → standard
part), and a column whose header is the label or key of an active attribute
(*Vendor*, `length_mm`) fills that attribute. A value that does not fit – text
in a number column, a choice that does not exist, an attribute that does not
apply to the item type – is left out with a warning. The preview lists which
columns become attributes and which are not imported; existing items are not
changed.

## Audit log

Every modification is recorded with user, time and details – shown on every
revision page and, for admins, under *Audit log*.
