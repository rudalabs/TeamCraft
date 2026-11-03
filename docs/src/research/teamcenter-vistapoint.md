# Popular features of Teamcenter and VistaPoint

TeamCraft is meant as an open-source alternative to Siemens Teamcenter (PLM)
and Open Domain VistaPoint (engineering document management). This page
lists the features their users rely on most, what TeamCraft already has and
what is implemented next. Date: 2026-11-04.

## The two systems

| | Siemens Teamcenter | Open Domain VistaPoint |
|---|---|---|
| Kind | PLM for large manufacturers (rich client, Active Workspace web client) | engineering document management (EDM) for small and mid-size engineering firms |
| Core objects | items, item revisions, datasets (files), BOM lines, change objects | folders and records (documents with metadata), file versions |
| Typical users | design, manufacturing and change engineers | engineers, document controllers, project teams |

## Most used features

| Feature | Teamcenter | VistaPoint | TeamCraft |
|---|---|---|---|
| Check-out / check-in with locks | ✓ | ✓ | implemented |
| Versions and revisions, rollback | ✓ (revisions, dataset versions) | ✓ | implemented |
| Lifecycle states, release workflow, reviews | ✓ (workflows, signoffs) | ✓ (approvals) | implemented (change orders, four-eyes review) |
| Search by attributes and full text | ✓ | ✓ | implemented |
| **Saved searches** | ✓ (Saved Queries) | ✓ | **implemented now** |
| **Favorites / personal folder** | ✓ (Favorites, Home, "Newstuff") | ✓ (my documents) | **implemented now** |
| Inbox / worklist | ✓ (My Worklist, tasks) | ✓ | implemented (dashboard, notifications) |
| Subscriptions / notifications | ✓ | ✓ | implemented (watch, bell, e-mail, Mattermost) |
| Multi-level BOM, revision rules, where-used | ✓ (Structure Manager) | – | implemented |
| **BOM comparison between revisions** | ✓ (structure compare) | – | **implemented now** |
| **Save As (copy with files and BOM)** | ✓ | ✓ (copy record) | **implemented now** |
| References between CAD files (xrefs) | ✓ | ✓ | implemented (reference scan, change propagation) |
| Impact analysis of changes | ✓ (Change Manager) | – | implemented |
| 3D viewing | ✓ (JT visualisation) | preview | implemented (STEP, meshes, point clouds; embedded previews of native files) |
| Export (Excel/CSV, packages) | ✓ | ✓ (transmittals) | implemented (CSV, Pack and Go ZIP) |
| Classification / custom attributes | ✓ (Classification) | ✓ (configurable records) | planned (OPEN_WORK #5) |
| Configurable numbering schemes | ✓ | ✓ | planned (OPEN_WORK #4) |
| Transmittals (issue documents to externals with a record) | – | ✓ | planned |
| Markup / redlining | ✓ | ✓ | planned |
| Effectivity, multi-site, ERP integration | ✓ | – | not planned for now |

## Decision

Implement the four most used features TeamCraft is missing and that fit its
data model without new concepts: **favorites**, **saved searches**,
**Save As** (a new item from an existing revision: attributes, files and BOM)
and **BOM comparison** between two revisions. Numbering schemes, custom
attributes, transmittals and redlining stay in `docs/OPEN_WORK.md`.

## Sources

VistaPoint: vendor listings on Capterra, GetApp and Software Advice (features:
check-out/check-in tracking, version control, file reference management for
CAD and Office files, configurable folders and records, change management,
archiving, web access, API). The VistaPoint product pages and Teamcenter
documentation were not read in this investigation; the Teamcenter column
reflects the generally documented standard functions of its rich client and
Active Workspace (items, datasets, Structure Manager, Saved Queries,
Favorites, workflows, Change Manager, Classification).
