# Read permissions for project groups

Until now every signed-in TeamCraft user could read every item, file and
change. Project groups only limited *editing*. This page records how other
systems scope *read* access and what TeamCraft adopts. Date: 2026-11-05.

## How other systems do it

| System                                                      | Unit of access                                                                                | Default                                                                          | Read grant                                                                                                    | Notes                                                                                                |
|-------------------------------------------------------------|-----------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| Onshape                                                     | document (and folders); teams and the company/enterprise as grantees                          | private to the owner                                                             | share a document with users, teams or the whole company; permission levels view / copy / export / edit        | a document shared "view only" can still forbid export or copy; enterprise admins see everything      |
| GitHub                                                      | repository                                                                                    | public or private chosen at creation; organisation base permission (read / none) | membership of the organisation or a team that has the repository                                              | a private repository answers **404**, never 403, to outsiders, so its existence is not confirmed     |
| Typical PDM/PLM (Teamcenter, Windchill, Vault Professional) | project, context, folder or library; access control lists per object type and lifecycle state | everything inside a context is visible to its members                            | context membership plus role; rules per state ("released documents visible to all, in-work only to the team") | powerful, but rule sets become hard to audit; most installations use "project = visibility boundary" |

Common ground:

- The boundary is a **container** (document, repository, project), not the
  individual object. Objects inherit it.
- Outsiders get "not found" – the existence of private containers is hidden.
- Administrators bypass the rules.
- Links from a visible object into a hidden one (an assembly using a part of
  another project, a fork of a private repository) are shown without the
  hidden content, or not at all.

## What TeamCraft adopts

- **Project groups are the boundary.** A group is either `internal`
  (today's behaviour: everybody reads, members edit) or `restricted`:
  only its members – of any role, viewers included – and admins read
  anything in it.
- **No per-object ACLs and no per-membership access levels** for now.
  A read-only member is a member with the global viewer role. Per-object
  rules (Teamcenter-style) are hard to audit and not needed for the
  request "not everybody may see every repository".
- **404 like GitHub.** A hidden item, file, change or group answers exactly
  like a missing one.
- **Links across the boundary** follow the PDM practice: an outgoing link (BOM line, reference, simulation basis) stays
  as a *placeholder* row ("restricted item", quantity and find number kept, identity removed);
  incoming links (where-used, referrers) are dropped and only counted.
- **Change orders and requests** have no group of their own. They are visible
  to their author, reviewers or assignee, admins and anyone who can see every
  affected item, because title and description are free text about all of
  them.
- **Side channels** – notifications, e-mail, Mattermost, webhooks, the
  assistant and the MCP server – apply the same rule per recipient.
- **Enforcement in the service layer**, not in PostgreSQL row-level security:
  RLS can only hide rows, while TeamCraft needs placeholders and counts, and
  per-request session state on a shared connection pool is risky. Every read
  service takes the actor; a single module writes the SQL guard; a
  route-complete leak test checks every web route, REST route and MCP tool.

## Not adopted (possible follow-ups)

- Onshape's view / copy / export levels and per-membership read/write levels.
- Per-item ACLs and field-level rules.
- Token scopes.
- Keeping a group's data away from the assistant's model provider.

## Sources

- Onshape help: "Sharing documents", "Teams", "Company and Enterprise
  permissions" (cad.onshape.com/help).
- GitHub docs: "About repositories – repository visibility", "Repository
  roles for an organization", and the 404 behaviour for private repositories
  in the REST API documentation.
- Teamcenter "Access Manager" and Windchill "context and access control"
  documentation (vendor manuals, conceptual overview only).
