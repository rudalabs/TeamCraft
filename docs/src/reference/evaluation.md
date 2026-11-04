# Evaluation of other open-source PLM systems

Evaluated on 2026-11-03 before TeamCraft was extended beyond its first
milestone. Points that could not be verified are marked **[unverified]**.

## Overview

|          | TurboPLM                                                          | OdooPLM                                             | DocDokuPLM                                                   | GitPLM                         |
|----------|-------------------------------------------------------------------|-----------------------------------------------------|--------------------------------------------------------------|--------------------------------|
| What     | web PLM for hardware teams                                        | 36+ PLM modules inside the Odoo ERP                 | Java EE PLM                                                  | Go CLI/TUI for CSV BOMs in Git |
| License  | AGPL-3.0 (FSL-1.1-MIT until 2026-08-01)                           | core LGPL-3, modules AGPL-3; CAD client proprietary | AGPL-3.0                                                     | Apache-2.0                     |
| Stack    | React, Node/Express, Prisma, PostgreSQL, three.js, occt-import-js | Python, Odoo 19, PostgreSQL                         | Java 8 / Java EE 8, Payara, PostgreSQL, Elasticsearch, Kafka | Go, CSV, YAML                  |
| Activity | pre-1.0, single vendor                                            | active since 2017                                   | dormant (main repo last pushed 2021)                         | v0.10.1 (2026-09)              |
| Hosting  | SaaS or Docker Compose (4 services)                               | full Odoo instance                                  | 11 containers                                                | local tool                     |

## Feature coverage (Y / P = partial / N)

| Feature                             | TurboPLM                                | OdooPLM                           | DocDoku                     | GitPLM  |
|-------------------------------------|-----------------------------------------|-----------------------------------|-----------------------------|---------|
| Roles + group access control        | P                                       | P                                 | Y                           | N       |
| Check-in/check-out locking          | Y                                       | Y (proprietary CAD client)        | Y                           | N       |
| File versions / rollback            | P                                       | P                                 | P                           | Y (Git) |
| Audit log                           | P                                       | P                                 | P                           | Y (Git) |
| Metadata + full-text search         | P (metadata only)                       | P                                 | Y (incl. file contents)     | P       |
| STEP/STL web preview                | Y (browser + server conversion to glTF) | Y (server conversion)             | Y (FreeCAD conversion)      | N       |
| Lifecycle with transition rules     | Y                                       | Y                                 | P                           | N       |
| Change propagation to dependents    | P (release gate)                        | P                                 | Y (changelog to verify)     | P       |
| Review/approval + notifications     | Y                                       | P                                 | Y                           | P       |
| Multi-level BOM, where-used, export | Y                                       | Y                                 | P                           | P       |
| ECR/ECO with impact analysis        | Y (where-used only)                     | P                                 | P                           | N       |
| API / webhooks / SSO                | REST + webhooks, no OIDC/LDAP           | XML-RPC/REST, OIDC via OCA module | REST, OIDC **[unverified]** | Git     |

## Why TeamCraft is its own system

* **TurboPLM** has the closest feature set but is young, from a single vendor
  that already changed the license once, without versioned migrations and
  without OIDC.
* **OdooPLM** is mature but requires a full Odoo ERP and a proprietary CAD
  client – heavy to operate for a small team.
* **DocDokuPLM** has the best data model but is dormant on old Java,
  Elasticsearch and Kafka versions across 11 containers.
* **GitPLM** targets electronics BOMs, not CAD data; it complements TeamCraft
  for KiCad projects.

TeamCraft therefore is a small, single-binary PLM: one process plus
PostgreSQL, a pure-Rust build, and an MPL-2.0 licence that allows embedding.

## Ideas adopted

| Idea                                                                        | Source                                  | TeamCraft                        |
|-----------------------------------------------------------------------------|-----------------------------------------|----------------------------------|
| Released revisions immutable, all revisions of a change released atomically | TurboPLM                                | implemented                      |
| Admin can break a lock (audit-logged)                                       | TurboPLM                                | implemented                      |
| File versions within a revision (check-in = new version)                    | DocDoku (master → revision → iteration) | implemented                      |
| Assembly changelog: dependents flagged until acknowledged                   | DocDoku                                 | implemented (change propagation) |
| Pack-and-go: an assembly with all files as ZIP                              | OdooPLM                                 | implemented                      |
| Server-side conversion of CAD files for the web preview                     | TurboPLM, DocDoku                       | implemented, in pure Rust        |
| Numbering masks per item type                                               | DocDoku                                 | implemented (per project group)  |
| HMAC-signed webhooks with delivery history                                  | TurboPLM                                | planned                          |
| KiCad HTTP library endpoint                                                 | GitPLM                                  | planned                          |

## Sources

TurboPLM: github.com/horatiumm-creator/turboplm, turboplm.com ·
OdooPLM: github.com/OmniaGit/odooplm, github.com/OCA/server-auth ·
DocDokuPLM: github.com/docdoku/docdoku-plm, docdoku-plm-conversion-service,
docdokuplm.com user guide 2.5 · GitPLM: github.com/git-plm/gitplm
