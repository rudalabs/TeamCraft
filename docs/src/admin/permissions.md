# Read permissions and restricted project groups

Every item belongs to a project group (or to none). A group is either

* **internal** – the default: every signed-in user reads it, its members
  edit it; or
* **restricted** – only its **members** (of any role, viewers included) and
  **administrators** see the group and anything in it: items, revisions,
  files and their versions, previews, BOMs, references, simulations,
  change orders and change requests about them. For everybody else it does
  not exist: they get exactly the "not found" a missing object gives.

The background and the alternatives that were considered are in the
research note [Read permissions for project groups](../research/read-permissions.md).

## Effective rights

| Global role | Member of the group | Internal group | Restricted group |
|---|---|---|---|
| Admin | any or none | read + edit | read + edit |
| CAD editor | yes | read + edit | read + edit |
| CAD editor | no | read | – (not found) |
| Viewer | yes | read | read |
| Viewer | no | read | – (not found) |

Items without a group are readable by everyone and editable by every CAD
editor (unchanged). A read-only member of a restricted group is a member
with the global **viewer** role; there is no per-group read/write level.

## Restricting a group

*Project groups → the group → Details → Visibility*: **Restrict to members**
or **Open to all users**. Managers of the group and administrators may do
this; the change is recorded in the audit log (`project.visibility`). A
group can also start restricted (checkbox when it is created, or
`"visibility": "restricted"` in `POST /api/v1/projects`). Add viewers as
members to give read-only access. Linking a group to an identity-provider
group (`oidc_group`) stays with administrators, because it grants read
access.

Moving an item out of a restricted group, or copying it with *Save As*
into another group, needs a manager of the source group or an administrator
– both declassify the item.

## What outsiders see of links

A visible item can point to a restricted one, and the other way round:

| Link | What a non-member sees |
|---|---|
| BOM line, item reference, simulation basis to a restricted item | a **placeholder row** "Restricted item": find number, quantity and the notes of the line stay, number, name, revision and link are removed, and the line is never expanded |
| Mass and cost roll-ups | leave restricted lines out; the BOM says "mass and cost exclude N restricted lines" (`restricted_lines`, `rollup_incomplete` in the API) |
| Where-used, referenced-by, simulations based on an item, impact analysis | restricted entries are left out and counted: "+N … in groups you cannot see" |
| CAD file-name matches (file references, suggestions, working sets) | matches in restricted groups are left out without a count; the name shows as not in TeamCraft |
| Similar parts (shape search) | restricted items are left out without a count, also when their file content is identical |
| Release readiness of a change | a restricted unreleased dependency still **blocks** the release and shows as "restricted dependency not released – ask a member of its group or an admin" |
| Pack and Go, desktop Get | restricted items contribute no files; the manifest and the report only count them |
| Change propagation (impacts) | a check on a visible item caused by a restricted release says "a restricted item this depends on was released" |
| Audit history | an entry about a link to a restricted item reads "… changed a link to a restricted item"; an entry whose text names restricted items or a change order the reader cannot see (the revisions of a submitted, rejected or released change, flagged dependents, a request's items, a linked change) reads "… (the details name restricted items)" without details. Flagged dependents are listed per group, so the entries about visible dependents stay readable |
| Readiness of a change | the depending revision of a hidden item shows as "restricted item"; another open change the reader cannot see shows as "another open change order" (also in the error when an item is already part of one) |
| Change request items | a restricted affected item is a placeholder row without its note |

## Change orders and change requests

They have no group of their own. A **change order** is visible to
administrators, its author, its reviewers and to everybody who can see
*every* affected item (a draft without items: author, reviewers, admins).
A **change request** is visible to administrators, the requester, the
assignee and to everybody who can see every affected item (a request
without items: everybody). Inside a visible change or request, rows of
items the reader cannot see are placeholders.

Reviewers and assignees must be able to see every affected item;
TeamCraft refuses others – also when an item is added later to a change
with reviewers or to a request with an assignee. Submitting, reviewing and reopening a change
needs the same – an author or reviewer who lost membership can still
withdraw or cancel, but not approve blind.

## Notifications, e-mail, chat, webhooks, assistant

* Notifications are created only for recipients who may read their
  subject, evaluated per recipient. A text that also names other items or
  a change order (the revisions of a change, the change that released an
  item or implemented a request) is replaced by a shorter text without
  them for recipients who may not see all of them.
* Notifications are checked again when they are read: after a group was
  restricted or a membership removed, the older notifications about it
  disappear from the list and the unread count, and pending e-mails and
  chat posts about it are not sent (marked `skipped`). They come back with
  the access. Notifications written before migration 0025 are shown as
  stored.
* With `TEAMCRAFT_MATTERMOST_DIRECT=false` (posts in a shared channel),
  notifications about restricted groups show only "new TeamCraft
  notification" and the link. E-mail and Mattermost direct messages go to
  one recipient and carry the full text.
* A webhook sees its own group and the internal groups; a webhook for all
  groups receives **no** events of restricted groups. Scope a webhook to
  the group to get them (see [Event webhooks](webhooks.md)).
* The MCP server and the built-in assistant use the user's rights, so they
  see the same as the user. The assistant sends what a member reads to the
  configured model provider.

## When changes take effect

Memberships and roles are loaded on every request (web, API tokens,
desktop, MCP): removing a member takes effect on the next request. Nothing
is deleted – watches and favorites of the lost items are hidden and come
back with the access, check-outs stay (the removal message says how many;
a manager or admin cancels them).

Memberships synchronised from the identity provider change only at the
user's next SSO login; a user removed from the IdP group keeps access until
then, and indefinitely with an API token. Revoke the user's tokens or
remove the membership in TeamCraft as well.

Files already copied to a desktop working folder stay on that computer;
the desktop app marks such revisions as "No longer accessible" and never
syncs them again.

## What stays visible

These are inherent to a shared instance and documented on purpose:

* Item numbers, project group codes and change/request numbers are unique
  across the instance: choosing a taken one says so ("already in use", the
  BOM import says "not available").
* The shared default number counter shows how many items other groups
  create. Give restricted groups their own scheme, e.g.
  `{PROJECT}-{TYPE}-{SEQ:4}`. The numbering preview of a scheme typed in
  (not saved) counts on only for counters that a scheme of a group the
  user sees uses; otherwise it starts at 1.
* A BOM line that would close a cycle is refused even if the cycle runs
  through restricted assemblies ("would create a circular BOM structure"):
  the structure must stay a tree for everybody.
* Change and request numbers are one sequence for all groups.
* Where-used and referenced-by counts show that restricted items use an
  item – its owner needs to know before making it obsolete or deleting it.
* Audit entries written before this feature (migration 0024) are shown as
  stored; their numbers were visible to everyone at the time. Migration
  0025 marks the older impact, change and request entries with the items
  and changes they name, so these are redacted like new ones.
* Response times of hidden and missing objects are not equalised.
* Backups, the command line and `/admin/audit` contain everything
  (operators and administrators).
