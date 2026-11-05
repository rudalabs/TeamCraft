# Research notes

Findings of investigations behind design decisions: how other systems solve
a problem, what is possible for an open-source project (licences, documented
formats), the decision and the sources. Every investigation gets its own page
here, written and pushed as soon as the findings are in – before the
implementation starts.

| Topic                                                                     | Decision                                                                                                                                                               | Date       |
|---------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------|
| [One provider interface for the assistant](agent-interface.md) | one provider-neutral API layer (`src/llm/`): Anthropic Messages and OpenAI-compatible Chat Completions now, stateless Responses later; the key stays on the server (desktop *Ask…* and ACP unchanged); base URL set by the operator, internal hosts allowed, no redirects; next: budgets, progress streaming, threads, context quick actions | 2026-11-05 |
| [Supersede, replace and alternate parts](supersede-alternates.md) | global two-way alternates per item, supersession with one successor, replace in assemblies from the where-used page (in-work parents only, released ones listed for a new revision); local substitutes per BOM line later | 2026-11-05 |
| [Jira connector for change requests and change orders](jira-connector.md) | one-way TeamCraft → Jira first (ECR/ECO as issues, status mapping, remote link back, outbox like webhooks, explicit project-group scope per connection); Jira webhooks later, never approvals | 2026-11-05 |
| [Previews of native CAD files (Inventor, Solid Edge)](cad-previews.md)    | embedded preview image: implement · 3D from the native file on the server: skip (licensing) · 3D through the user's own CAD installation in the desktop app: implement | 2026-11-04 |
| [Importing existing CAD folder trees](cad-folder-import.md)               | scan with a duplicate report first, one item per model with its drawings and exports, check-in at the end, BOM from references optional, tutorial in the desktop app   | 2026-11-04 |
| [Popular features of Teamcenter](teamcenter.md)                           | implement favorites, saved searches, Save As and BOM comparison; numbering schemes, custom attributes, transmittals and redlining stay planned                         | 2026-11-04 |
| [Openly licensed CAD files for end-to-end tests](cad-test-files.md)       | NIST test models (no restrictions): parts in Inventor, NX, SolidWorks, Creo, CATIA V5, STEP, NX and SolidWorks assemblies; Solid Edge: none found                      | 2026-11-04 |
| [Read permissions for project groups](read-permissions.md)                | restricted project groups: members and admins read, everybody else gets 404; placeholders for links across the boundary; enforcement in the services, no row-level security | 2026-11-05 |
