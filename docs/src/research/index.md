# Research notes

Findings of investigations behind design decisions: how other systems solve
a problem, what is possible for an open-source project (licences, documented
formats), the decision and the sources. Every investigation gets its own page
here, written and pushed as soon as the findings are in – before the
implementation starts.

| Topic                                                                     | Decision                                                                                                                                                               | Date       |
|---------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------|
| [Previews of native CAD files (Inventor, Solid Edge)](cad-previews.md)    | embedded preview image: implement · 3D from the native file on the server: skip (licensing) · 3D through the user's own CAD installation in the desktop app: implement | 2026-11-04 |
| [Importing existing CAD folder trees](cad-folder-import.md)               | scan with a duplicate report first, one item per model with its drawings and exports, check-in at the end, BOM from references optional, tutorial in the desktop app   | 2026-11-04 |
| [Popular features of Teamcenter](teamcenter.md)                           | implement favorites, saved searches, Save As and BOM comparison; numbering schemes, custom attributes, transmittals and redlining stay planned                         | 2026-11-04 |
| [Openly licensed CAD files for end-to-end tests](cad-test-files.md)       | NIST test models (no restrictions): parts in Inventor, NX, SolidWorks, Creo, CATIA V5, STEP, NX and SolidWorks assemblies; Solid Edge: none found                      | 2026-11-04 |
| [Read permissions for project groups](read-permissions.md)                | restricted project groups: members and admins read, everybody else gets 404; placeholders for links across the boundary; enforcement in the services, no row-level security | 2026-11-05 |
