# ADR 0001 – Read access: deny list on the actor, "not found", enforced in the services

**Status:** accepted – introduced in #52 (*Read access control: restricted
project groups*), review fixes in #55. Research and the full reasoning:
[Read permissions for project groups](../../research/read-permissions.md).

## Context

Until #52 every signed-in user could read every item, file and change; project
groups only limited *editing*. Some groups have to be invisible to
non-members, without making every object carry its own access list. The same
data reaches users through the web UI, the REST API, the MCP server, the
assistant, notifications, e-mail, Mattermost and webhooks.

## Decision

* **The project group is the boundary.** A group is `internal` (everybody
  reads, members edit) or `restricted` (only members – of any role – and
  administrators read). Column `projects.visibility`, migration
  `0024_restricted_projects.sql`.
* **A deny list on the actor.** The signed-in `Actor` (`src/auth.rs`) carries
  `hidden_projects`: the restricted groups it may **not** read, empty for
  administrators. It is never serialized.
* **Hidden looks like missing.** A hidden item, revision, file, change or group
  answers exactly like one that does not exist (404 / "not found"), like a
  private GitHub repository.
* **Links across the boundary** stay visible as placeholders when they go out
  of a visible object (a BOM line, a reference: "restricted item", quantity
  kept) and are dropped and only counted when they come in (where-used,
  referrers: "+N in groups you cannot see").
* **One place writes the SQL guard.** `src/services/access.rs` builds every
  guard (`item_visible`, `project_visible`, `change_visible`, …) that binds
  `hidden_projects`; read services take `&Actor` and use them. The
  `*_unchecked` getters (no read check) are for background and system code.
* **Side channels** – notifications, e-mail, Mattermost, webhooks, the
  assistant, the MCP server – apply the same rule per recipient.

## Consequences

* Every read service needs the actor; a forgotten guard is a leak, so the
  rule is held by tests rather than by review alone (below).
* Lists that leave entries out say how many (`X-Hidden-Count` in the REST API,
  "+N …" in the UI).
* Copying out of a restricted group declassifies content, so Save As across
  the boundary is limited to managers of the source group
  (`items::ensure_may_take_out`, used by `services::save_as`).
* A need-to-know policy (`project_policies.admin_read`, #151 step 1) fills
  the hidden groups of administrators too (`access::hidden_of_user`): the
  guards stay the only place that decides. Administrators keep *maintaining*
  such a group (`projects::get_for_management`, `require_manager`) without
  reading it; elevations are rows in `project_elevations`, never deleted.
* There are no per-object ACLs and no per-membership access levels; a
  read-only member is a member with the global viewer role.

## Alternatives turned down

From the research note:

* **PostgreSQL row-level security** – RLS can only hide rows, while TeamCraft
  needs placeholders and counts; and per-request session state on a shared
  connection pool is risky.
* **Per-object ACLs and field-level rules** (as in large PDM systems) – hard
  to audit, not needed for "not everybody may see every repository".
* **403 for hidden objects** – would confirm that the object exists.
* Onshape-style view / copy / export levels and token scopes are listed as
  possible follow-ups, not adopted.

## Where it is held

| What | Where |
|------|-------|
| Deny list on the actor | `Actor::hidden_projects`, `Actor::can_see_project` in `src/auth.rs` |
| The only SQL guard writer | `src/services/access.rs` |
| Handlers never call `*_unchecked`; nobody but `access` writes a guard | `tests/no_unchecked_in_handlers.rs` |
| Every web route, REST route and MCP tool leaks nothing to an outsider | `tests/read_permissions.rs` (routes read from the router sources) |
| Handlers never touch SQL themselves | proposed in #96 (`tests/architecture_guards.rs`) |
