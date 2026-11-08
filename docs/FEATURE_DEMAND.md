# PLM/PDM Feature Demand List

Features users ask for in PLM/PDM systems, with the demand and TeamCraft's state. Implemented features are removed
from this list (see `OPEN_WORK.md` and the git history for what was done).

## Core data, CAD and vault behavior

- **Modern, intuitive UI — Very high demand — TeamCraft: partial.** Reduce clicks, clutter, modal dialogs and legacy
  interaction patterns; Teamcenter users repeatedly identify UI complexity and unintuitive navigation as major pain
  points.
  ([G2](https://ai.g2.com/product/teamcenter); [G2](https://www.g2.com/products/teamcenter/reviews?page=7); [Reddit](https://www.reddit.com/r/engineering/comments/vysfex); [TrustRadius](https://www.trustradius.com/products/siemens-plm-teamcenter/reviews?industry=professional-services))
- **Fast UI response on large datasets — Very high — TeamCraft: unproven at scale.** Large assemblies, searches and
  metadata pages should remain responsive; Teamcenter performance complaints recur across independent reviews, while
  TeamCraft still lists 1M-file/100k-item/10k-line-BOM scale validation as open work.
  ([G2](https://ai.g2.com/product/teamcenter); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md); [Capterra](https://www.capterra.com/p/217609/Teamcenter/reviews/); [TrustRadius](https://www.trustradius.com/products/siemens-plm-teamcenter/reviews?industry=professional-services))
- **Faster file open/download — High — TeamCraft: partial.** Minimize latency opening managed CAD files and large
  assemblies; Teamcenter reviewers specifically complain about slow file opening.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [Capterra](https://www.capterra.com/p/217609/Teamcenter/reviews/))
- **Simple check-out/check-in — Very high — TeamCraft: present.** Exclusive locking, visible lock ownership and
  uncomplicated check-in are foundational recurring PDM expectations; Vistapoint markets these directly and TeamCraft
  already provides them.
  ([Vistapoint](https://getvistapoint.com/); [Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Offline/local working folder — High — TeamCraft: present/partial.** Let engineers work locally with managed
  synchronization instead of constantly streaming files; TeamCraft has a desktop working folder, while mature PDM
  systems typically rely heavily on local caches.
  ([Vistapoint](https://getvistapoint.com/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Local CAD cache — High — TeamCraft: partial.** Cache downloaded project/assembly files and invalidate intelligently
  to accelerate repeated opens; Vistapoint explicitly advertises a local cache.
  ([Vistapoint](https://getvistapoint.com/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Atomic multi-file check-in — High — TeamCraft: partial.** Check in an assembly and dependent files together without
  leaving half-completed states; Vistapoint supports multi-file check-in, while TeamCraft still prioritizes proof of
  atomic multi-row processes.
  ([Vistapoint](https://getvistapoint.com/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Resumable large-file upload — High — TeamCraft: present.** Chunk and resume large CAD/CAE uploads after
  interruptions rather than restarting. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Content deduplication — Medium — TeamCraft: present.** Store identical binary files once while preserving logical
  versions. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Full version history — Very high — TeamCraft: present.** Preserve every check-in and make prior states inspectable;
  both Vistapoint and TeamCraft treat this as core functionality.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **One-click rollback/restore — High — TeamCraft: present.** Restore an earlier file version without overwriting
  history.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Distinct version vs revision concepts — Very high — TeamCraft: present.** Separate working iterations from formal
  released revisions; this is a recurring expectation in Vistapoint, Teamcenter and open-source PLM implementations.
  ([Vistapoint](https://getvistapoint.com/features/); [GitHub](https://github.com/Dokuly-PLM/dokuly); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Immutable released revisions — Very high — TeamCraft: present.** Prevent silent edits after release and force
  controlled revision/change processes.
  ([GitHub](https://github.com/horatiumm-creator/turboplm); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Configurable lifecycle states — Very high — TeamCraft: partial.** Organizations want configurable rather than
  hard-coded WIP/review/released/obsolete lifecycles; TeamCraft currently exposes a defined lifecycle, while Vistapoint
  promotes configurable workflows.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Custom lifecycle transitions — High — TeamCraft: missing/limited.** Allow site-specific states and allowed
  transitions rather than a single built-in state machine.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Bulk lifecycle/revision operations — High — TeamCraft: unclear.** Revise, release, obsolete or update many objects
  in one operation; bulk actions reduce the repetitive interaction burden frequently criticized in Teamcenter.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [G2](https://www.g2.com/products/teamcenter/reviews?page=7); [Reddit](https://www.reddit.com/r/engineering/comments/vysfex))
- **Multi-CAD support — Very high — TeamCraft: partial.** Manage files and relationships from SolidWorks, NX, Creo,
  CATIA, Inventor, Revit, AutoCAD and other authoring tools in one vault; Vistapoint explicitly positions multi-CAD as a
  differentiator.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Native CAD add-ins — Very high — TeamCraft: missing.** Engineers repeatedly prefer PLM actions inside CAD rather
  than switching applications; TeamCraft explicitly lists CAD add-ins as not required for 1.0.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md); [Capterra](https://www.capterra.com/p/217609/Teamcenter/reviews/))
- **SolidWorks add-in — Very high — TeamCraft: missing.** Check in/out, search, revise and manage BOM/reference
  operations directly inside SolidWorks.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **NX managed-mode integration — Very high — TeamCraft: missing.** Deep NX integration comparable to Teamcenter managed
  mode, including Save As, revise, structure and numbering behavior.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D54O000061xtOlSAI/import-assembly-into-teamcenter-now-different-in-nx-11-); [Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00001CjcmWKAR/sort-the-nx-cloning-dialog-as-assembly-structure-instead-of-alphabetic-); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Creo integration — High — TeamCraft: missing native add-in.** Direct managed workflows and reference synchronization
  for Creo.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **CATIA integration — High — TeamCraft: missing native add-in.** Manage CATIA references, metadata and neutral exports
  directly from the authoring environment.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Inventor integration — High — TeamCraft: missing native add-in.** Native check-in/out, metadata mapping and
  dependency handling.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Solid Edge integration — High — TeamCraft: missing native add-in.** Preserve Solid Edge occurrence behavior and
  CAD/PDM metadata semantics.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5KZ000006nQ6m0AE/attach-solid-edge-files-to-workflow-using-bom-expansion-rules); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **AutoCAD integration — High — TeamCraft: missing native add-in.** Manage drawings, xrefs and metadata without leaving
  AutoCAD.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Revit/BIM integration — Medium-high — TeamCraft: missing native add-in.** Extract and manage BIM metadata and
  references alongside mechanical CAD.
  ([Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **EPLAN/ECAD integration — High — TeamCraft: missing native integration.** Manage electrical design files and BOM
  connectivity rather than treating ECAD as generic attachments.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00001jbEBMKA2/what-solid-edge-electrical-routing-data-is-transferred-to-and-managed-in-teamcenter); [Vistapoint](https://getvistapoint.com/integrations/))
- **Electrical-routing data in PLM — Medium-high — TeamCraft: missing/unclear.** Carry routing, harness and associated
  electrical structure information into the managed product definition.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00001jbEBMKA2/what-solid-edge-electrical-routing-data-is-transferred-to-and-managed-in-teamcenter))
- **Reference extraction from native CAD — Very high — TeamCraft: partial.** Detect assembly/component/file references
  automatically rather than asking users to maintain links manually.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Reference repair after rename/move — High — TeamCraft: unclear.** Keep CAD references valid as managed objects
  change location or identity; reference integrity is a central PDM expectation.
  ([Vistapoint](https://getvistapoint.com/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Reference replacement with preview — High — TeamCraft: present for BOM replacements.** Preview downstream impact
  before substituting components. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Pack-and-Go assembly package — High — TeamCraft: present.** Export a complete assembly plus dependencies for
  suppliers, offline work or archival. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Folder/project import wizard — High — TeamCraft: present/being hardened.** Bulk-import unmanaged engineering
  directories while preserving references and avoiding duplicates.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Idempotent import after network failure — High — TeamCraft: implementation still being verified.** Retrying an
  interrupted import must not duplicate parts or silently skip files.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Multi-file document upload — Medium-high — TeamCraft: likely present via file workflows but not clearly
  document-specific.** Dokuly has an open request with multiple comments for uploading several documents at once.
  ([githubhelp.com](https://githubhelp.com/Dokuly-PLM/dokuly))
- **Automatic neutral-file generation — High — TeamCraft: missing/limited.** Generate STEP, PDF, DXF, XT and other
  derivatives automatically when source CAD changes or releases. Vistapoint exposes this prominently.
  ([Vistapoint](https://getvistapoint.com/features/); [Vistapoint](https://getvistapoint.com/integrations/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Automatic drawing PDF generation — High — TeamCraft: missing.** Produce controlled PDFs during check-in/release
  without requiring manual CAD exports.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Automatic DXF generation — High — TeamCraft: missing.** Generate manufacturing DXFs from relevant CAD files on
  check-in/release.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Server-side CAD conversion queue — High — TeamCraft: partial.** Offload preview/neutral conversion from user
  workstations and manage retries/failures centrally.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Native CAD thumbnails — Medium-high — TeamCraft: present.** Show embedded Inventor, Solid Edge, NX and SolidWorks
  previews without opening CAD. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Broad file-format recognition — High — TeamCraft: present.** Recognize mechanical CAD, BIM, DCC, mesh, point-cloud
  and neutral formats instead of limiting the vault to a small supported set.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Native-file browser preview — High — TeamCraft: partial.** Users want useful previews without local applications;
  TeamCraft supports several neutral/embedded previews but not full native geometry for every CAD format.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **STEP browser viewer — Very high — TeamCraft: present.** Interactive 3D viewing of STEP without plugins or CAD
  licenses. ([Vistapoint](https://getvistapoint.com/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **JT browser viewer — High — TeamCraft: recognized but viewer capability unclear/missing.** JT remains important in
  Teamcenter environments, and Teamcenter users report visualization issues when JT content is unavailable.
  ([Siemens Community](https://community.sw.siemens.com/s/article/jt-files-not-visible-in-active-workspace-viewer); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **IFC viewer — Medium-high — TeamCraft: recognition present; full viewing unclear.** Vistapoint advertises web viewing
  for IFC and other neutral models.
  ([Vistapoint](https://getvistapoint.com/features/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Point-cloud viewer — Medium — TeamCraft: present.** Support engineering scan data such as E57/LAS alongside product
  models. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **PMI display in 3D — High — TeamCraft: missing/unclear.** Users want Product Manufacturing Information visible in PLM
  visualization; Teamcenter has had several enhancement requests specifically for assembly-level PMI.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00000ZJvbvKAD/how-can-i-see-pmi-in-3d-view-that-has-been-created-on-assembly-level-not-component-level); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Assembly-level PMI — High — TeamCraft: missing.** Display PMI authored on assemblies, not only component-level
  annotations; Siemens explicitly references several ERs including ER 10730834.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00000ZJvbvKAD/how-can-i-see-pmi-in-3d-view-that-has-been-created-on-assembly-level-not-component-level))
- **Configurable measurement precision — Medium-high — TeamCraft: unclear.** Teamcenter 2606 users lost adjustable 3D
  measurement precision and Siemens opened PR 11459057/enhancement work.
  ([Siemens Community](https://community.sw.siemens.com/s/article/Unable-to-configure-measurement-decimal-precision-in-Active-Workspace-2026-3D-Visualization))
- **3D sectioning/clipping — High — TeamCraft: unclear.** Mature PLM visualization workflows commonly need internal
  inspection rather than view-only rendering; TeamCraft's documented viewer features do not list section planes.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **3D markup/annotations — High — TeamCraft: missing/unclear.** Reviewers need graphical annotations attached to models
  during design review/change workflows.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D5Vb00000ZJvbvKAD/how-can-i-see-pmi-in-3d-view-that-has-been-created-on-assembly-level-not-component-level); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **3D measurement tools — High — TeamCraft: unclear.** Dimensioning and inspection within the browser are expected in
  enterprise visualization; Teamcenter's precision ER demonstrates active demand around measurement behavior.
  ([Siemens Community](https://community.sw.siemens.com/s/article/Unable-to-configure-measurement-decimal-precision-in-Active-Workspace-2026-3D-Visualization); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Graphical revision comparison — High — TeamCraft: partial.** Show geometric changes between CAD revisions,
  complementing textual/BOM compare; Teamcenter users reference graphical BOM/model comparison alongside BOM compare.
  ([Siemens Community](https://community.sw.siemens.com/s/question/0D54O00007lWtH9SAK/compare-models); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Duplicate-part detection by metadata — High — TeamCraft: partial.** Warn users when creating a likely duplicate
  part; Dokuly has an open request specifically for duplicate detection.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [githubhelp.com](https://githubhelp.com/Dokuly-PLM/dokuly))
- **Duplicate-part detection by shape — High — TeamCraft: present.** Find geometrically similar existing parts to
  improve reuse; TeamCraft already includes shape/size similarity search.
  ([Siemens Blog](https://blogs.sw.siemens.com/teamcenter/product/teamcenter-community/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **CAD shape search — High — TeamCraft: present.** Search by 3D similarity rather than only attributes/text; Siemens
  promotes 3D shape search as a reuse capability.
  ([Siemens Blog](https://blogs.sw.siemens.com/teamcenter/product/teamcenter-community/); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **File-content indexing — High — TeamCraft: present.** Search text inside managed files, not just object metadata.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft))
- **Virus/malware scanning for uploads — Medium-high — TeamCraft: missing/unclear.** Enterprise vaults benefit from
  server-side file scanning, while TeamCraft's published security work focuses on parsers, limits and integrity rather
  than malware scanning. ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Vault integrity checker — High — TeamCraft: planned.** Detect mismatches between database records and physical
  blobs; TeamCraft lists `vault check` as part of its GA criteria.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Encrypted backups — High — TeamCraft: missing/planned.** Optional backup encryption and signed manifests are
  explicitly still open in TeamCraft.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
- **Point-in-time backup/restore drills — High — TeamCraft: partial.** Automated backups should be routinely
  restore-tested across releases; TeamCraft has commands but still lists CI restore proof as a GA requirement.
  ([TeamCraft GitHub](https://github.com/rudalabs/TeamCraft); [TeamCraft GitHub](https://github.com/rudalabs/TeamCraft/blob/main/docs/OPEN_WORK.md))
