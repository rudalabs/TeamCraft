# Popular features of Teamcenter

TeamCraft is meant as an open-source alternative to Siemens Teamcenter. This
page lists the features its users rely on most, what TeamCraft already has
and what is implemented next. Date: 2026-11-04.

## Siemens Teamcenter

| | |
|---|---|
| Kind | PLM for large manufacturers (rich client, Active Workspace web client) |
| Core objects | items, item revisions, datasets (files), BOM lines, change objects |
| Typical users | design, manufacturing and change engineers |

## Most used features

| Feature | Teamcenter | TeamCraft |
|---|---|---|
| Check-out / check-in with locks | ✓ | implemented |
| Versions and revisions, rollback | ✓ (revisions, dataset versions) | implemented |
| Lifecycle states, release workflow, reviews | ✓ (workflows, signoffs) | implemented (change orders, four-eyes review) |
| Search by attributes and full text | ✓ | implemented |
| **Saved searches** | ✓ (Saved Queries) | **implemented now** |
| **Favorites / personal folder** | ✓ (Favorites, Home, "Newstuff") | **implemented now** |
| Inbox / worklist | ✓ (My Worklist, tasks) | implemented (dashboard, notifications) |
| Subscriptions / notifications | ✓ | implemented (watch, bell, e-mail, Mattermost) |
| Multi-level BOM, revision rules, where-used | ✓ (Structure Manager) | implemented |
| **BOM comparison between revisions** | ✓ (structure compare) | **implemented now** |
| **Save As (copy with files and BOM)** | ✓ | **implemented now** |
| References between CAD files (xrefs) | ✓ | implemented (reference scan, change propagation) |
| Impact analysis of changes | ✓ (Change Manager) | implemented |
| 3D viewing | ✓ (JT visualisation) | implemented (STEP, meshes, point clouds; embedded previews of native files) |
| Export (Excel/CSV, packages) | ✓ | implemented (CSV, Pack and Go ZIP) |
| Classification / custom attributes | ✓ (Classification) | planned (OPEN_WORK #5) |
| Configurable numbering schemes | ✓ | planned (OPEN_WORK #4) |
| Transmittals (issue documents to externals with a record) | – (common in document management systems) | planned |
| Markup / redlining | ✓ | planned |
| Effectivity, multi-site, ERP integration | ✓ | not planned for now |

## Decision

Implement the four most used features TeamCraft is missing and that fit its
data model without new concepts: **favorites**, **saved searches**,
**Save As** (a new item from an existing revision: attributes, files and BOM)
and **BOM comparison** between two revisions. Numbering schemes, custom
attributes, transmittals and redlining stay in `docs/OPEN_WORK.md`.

## Sources

The Teamcenter column reflects the generally documented standard functions
of its rich client and Active Workspace (items, datasets, Structure Manager,
Saved Queries, Favorites, workflows, Change Manager, Classification); the
Teamcenter documentation itself was not read in this investigation.
