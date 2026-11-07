# Supersede, replace and alternate parts

Feature 6 of the work plan asks for *supersede/replace* and *alternate parts*
next to the item copy and the multi-level where-used that are already done.
This page records how other systems model these relations and what TeamCraft
adopts. Date: 2026-11-05.

## How other systems do it

| System | Interchangeable parts | Replacement of an outdated part | Notes |
|---|---|---|---|
| PTC Windchill | *alternate* (global: B may be used instead of A in every assembly) and *substitute* (local: only on one BOM usage) | *replacement* links and mass change "replace in where-used" | alternates can be one-way or two-way |
| Aras Innovator | *Part Alternate* relationship on the part, *BOM Substitute* on the BOM line | supersession through a new part plus change process | alternates are not revisioned with the BOM |
| ERPNext | *Item Alternative* document with a *two-way* flag; a BOM line opts in with *Allow Alternative Item* | none built in | the alternative is offered when material is short |
| SAP S/4HANA (MM/PP) | alternative items in the BOM (groups with priorities and usage probability) | *discontinuation* with a *follow-up material*: MRP switches to the successor when the stock is used up | discontinuation is a property of the outdated material |

Common ground:

- **Two different relations.** *Alternate* means "form, fit and function are
  the same, either may be built in"; it is a statement about the two parts and
  does not change any BOM. *Supersede* means "do not use A for new designs any
  more, use B"; it has a direction, and an outdated part has at most one
  successor.
- **Global vs. local.** A global alternate is valid wherever A is used; a
  substitute is valid only in one assembly. Global alternates cover the
  common case (second source of a purchased part, an equivalent standard part)
  and are cheap to keep; local substitutes need a relation per BOM line.
- **Replacing in assemblies is a mass change on the where-used list.**
  Assemblies that are still in work are changed directly; released
  assemblies keep their history and need a new revision (usually through a
  change order).
- Alternates and supersession are **item-level** and not frozen with a
  revision: the knowledge "B can replace A" stays true for old assemblies too.

## What TeamCraft adopts

- **Alternates** as an item-level relation `item_alternates` (A → B, with a
  note). Adding one creates both directions by default ("two-way"); a one-way
  alternate is possible. Shown on the revision page of both items, in the REST
  API and the MCP item details. Editing needs edit rights in the group of A.
- **Supersession** as `item_supersessions` (A superseded by B, note, who,
  when): at most one successor, no chains that lead back to A. The pages of A
  and B show it, the BOM marks superseded children, and adding a superseded
  item to a BOM warns. Superseding does not change the lifecycle state of A –
  making its revisions obsolete stays the change process's job.
- **Replace in assemblies**: from the where-used page of A, replace A by B in
  every parent that is in work and not checked out by somebody else (one
  transaction, quantity, find number and notes kept, audit per line). Released
  parents and parents in review are listed as "needs a new revision"; parents
  checked out by others and parents in hidden groups are reported, not changed.
  A preview shows the plan before anything is written.
- **Multi-level where-used in the web UI**: a where-used page with a level
  selector (the API and MCP had `levels=N` already), the starting point for
  the replace action.

## Not adopted (possible follow-ups)

- Local substitutes per BOM line, priorities and usage probabilities of
  alternative groups (SAP).
- Automatic switching to the successor when stock runs out – TeamCraft keeps
  no stock.
- Replacing inside released assemblies by revising them automatically; this
  stays a decision of the change process.

## Sources

- ERPNext manual, *Item Alternative* (docs.erpnext.com, manufacturing module).
- PTC Windchill help, *Alternate and substitute parts* and *replacement parts*.
- Aras Innovator documentation, ItemTypes *Part Alternate* and *BOM Substitute*.
- SAP Help Portal, *Discontinuation of materials* / *follow-up material* in MRP.
