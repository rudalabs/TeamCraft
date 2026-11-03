# Evaluation of existing open-source PLM tools

Evaluated on 2026-11-03 against WARR's milestone list before extending
TeamCraft further. Sources are listed at the end; points that could not be
verified are marked **[unverified]**.

## Overview

| | TurboPLM | OdooPLM | DocDokuPLM | GitPLM |
|---|---|---|---|---|
| What | full web PLM for hardware teams | 36+ PLM modules inside the Odoo ERP | full Java EE PLM | Go CLI/TUI for CSV-based BOMs in Git |
| License | AGPL-3.0 (FSL-1.1-MIT until 2026-08-01) | core LGPL-3, other modules AGPL-3; CAD client proprietary | AGPL-3.0 | Apache-2.0 |
| Stack | React, Node/Express, Prisma, PostgreSQL, three.js, occt-import-js | Python, Odoo 19, PostgreSQL | Java 8 / Java EE 8, Payara, PostgreSQL, Elasticsearch, Kafka | Go, CSV, YAML |
| Activity | repo from 2026-07, pre-1.0, 16 commits, single vendor | active since 2017, v19.0.1.0.32 | effectively dormant (main repo last pushed 2021) | v0.10.1 (2026-09) |
| Hosting | SaaS or Docker Compose (4 services) | full Odoo instance (2.5–4.4 GB images) | 11 containers | local tool |

## Feature coverage (Y / P = partial / N)

| Feature | TurboPLM | OdooPLM | DocDoku | GitPLM |
|---|---|---|---|---|
| Roles + group access control | P | P | Y | N |
| Check-in/check-out locking | Y | Y (proprietary CAD client) | Y | N |
| File versions / rollback | P | P | P | Y (Git) |
| Audit log | P | P | P | Y (Git) |
| Metadata + full-text search | P (metadata only) | P | Y (incl. file contents) | P |
| STEP/STL web preview | Y (browser + server conversion to glTF) | Y (server conversion) | Y (FreeCAD conversion) | N |
| Lifecycle with transition rules | Y | Y | P | N |
| Configurable numbering | P | P | Y (masks) | Y (fixed scheme) |
| Change propagation to dependents | P (release gate) | P | Y (changelog to verify) | P |
| Review/approval + notifications | Y | P | Y | P |
| Custom metadata per type | Y | P | Y | P |
| Multi-level BOM, where-used, compare, export | Y | Y | P | P |
| ECR/ECO with impact analysis | Y (where-used only) | P | P | N |
| Redlining | Y | Y | P | N |
| API / webhooks / SSO | REST + webhooks, **no OIDC/LDAP** | XML-RPC/REST, OIDC via OCA module | REST, OIDC **[unverified]** | Git |

## Why not adopt one of them

* **TurboPLM** has the closest feature set but is three months old, from a
  single vendor that already changed the license once, without versioned
  migrations and without OIDC (no Authentik login without patching).
* **OdooPLM** is mature but requires running a full Odoo ERP and a proprietary
  CAD client – too heavy to operate for a student team.
* **DocDokuPLM** has the best data model but is dormant on old Java/Elasticsearch/
  Kafka versions across 11 containers – high operating effort and security risk.
* **GitPLM** targets electronics BOMs, not CAD data; it could complement
  TeamCraft for the KiCad team.

TeamCraft therefore stays a small, single-binary PLM that fits WARR IT's
deployment model (one container + PostgreSQL, Authentik groups).

## Ideas adopted or planned for TeamCraft

| Idea | Source | TeamCraft |
|---|---|---|
| Released revisions immutable, release of all change revisions atomically, one open change per revision | TurboPLM | implemented |
| Admin can break a lock (recorded in the audit log) | TurboPLM | implemented |
| File versions within a revision (check-in = new version), revision letters for releases | DocDoku (master → revision → iteration) | milestone 1 |
| Release gate: an assembly cannot be released while a child part has no released revision | TurboPLM | milestone 2 |
| Assembly changelog: parents list changed children until acknowledged | DocDoku | milestone 2 (change propagation) |
| Numbering masks per item type | DocDoku | milestone 2 |
| Pack-and-go: download an assembly with all files as ZIP | OdooPLM | later |
| HMAC-signed webhooks with delivery history, notification outbox table | TurboPLM | later |
| KiCad HTTP library endpoint for the parts table | GitPLM | later |

## STEP/STL preview with a strict CSP

* **STL** is simple triangle data and can be parsed and rendered in the
  browser without libraries (TeamCraft uses its own small WebGL viewer).
* **STEP** needs OpenCascade to tessellate B-rep geometry.
  `occt-import-js` 0.0.23 (LGPL-2.1, 7.6 MB WASM) works in the browser but needs
  `'wasm-unsafe-eval'` **and** `'unsafe-eval'` (embind uses `new Function`).
  Options: run it in a dedicated same-origin worker with its own CSP, or convert
  on the server when files are checked in (e.g. `cascadio`, MIT, STEP → GLB) and
  cache the mesh in the vault. Both TurboPLM and DocDoku convert on the server.

## Sources

TurboPLM: github.com/horatiumm-creator/turboplm, turboplm.com ·
OdooPLM: github.com/OmniaGit/odooplm (README, wiki), github.com/OCA/server-auth ·
DocDokuPLM: github.com/docdoku/docdoku-plm, docdoku-plm-server, docdoku-plm-docker,
docdoku-plm-conversion-service, docdokuplm.com user guide 2.5 ·
GitPLM: github.com/git-plm/gitplm ·
Preview: registry.npmjs.org/three, registry.npmjs.org/occt-import-js,
github.com/kovacsv/occt-import-js, MDN (CSP script-src, import maps, web workers),
emscripten.org settings reference, pypi.org/project/cascadio
