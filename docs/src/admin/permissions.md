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

## Need to know: administrator access

By default an administrator reads every restricted group. A restricted group
can limit that (*Project groups → the group → Need to know*, managers and
administrators; `PUT /api/v1/projects/{code}/policy`
`{"admin_read": "implicit" | "with_reason" | "none"}`):

* **implicit** – today's behaviour, the default (also when no policy was ever
  set);
* **with a reason** – an administrator who is not a member sees the group as
  "not found" until they open an **elevation** with a written reason (10 to
  1000 characters; `POST /api/v1/projects/{code}/elevations`, or the form on
  the group page). It lasts 60 minutes, can be ended early
  (`DELETE …/elevations`) and is audited (`project.elevate`, with the reason).
  The group's managers get a notification, and the group page lists every
  elevation with administrator, reason and time;
* **none** – administrators still manage the group (members, policy, the
  settings on the group page) but read nothing in it. An administrator cannot
  add themselves as a member or make themselves a manager: a manager of the
  group does that. There is no elevation under `none`.

Every change of the policy is audited (`project.policy`, with the previous
value) and notifies the other managers. The policy applies to restricted
groups only and needs at least one member who is a manager, so the group can
be recovered; opening the group to all users makes the policy inert.
While a policy is in force, an administrator who is no manager member can
still add or remove other members, tighten the policy and rename the group,
but cannot loosen the policy, open the group to all users, link another
identity-provider group, add themselves or become a manager (403): those
would undo the policy in one call. What an administrator can still do is
limited by design: add *another* user (a manager sees it in the audit log),
see that the group exists (code and name), and change operator-level things
(database, backups, the command line) outside the application. A group
linked to an identity-provider group before the policy was set keeps that
link, so an administrator in that IdP group is a member. Webhooks cannot be
scoped to, or redirected for, a hidden group; their deliveries are not shown.
The elevation history keeps the username when an account is deleted;
deleting the group deletes its history (the audit log keeps the records). The rule
is part of the actor's hidden groups (`access::hidden_of_user`), so every
guard, the audit feed, change orders, notifications and the assistant honour
it without a check of their own. This is step 1 of
[#151](https://github.com/rudalabs/TeamCraft/issues/151); conditions on
membership, an access review and token scopes are not built yet (see
`docs/OPEN_WORK.md`); steps 3 and 4 follow below.

### Interface switches and the read log (need to know, steps 3 and 4)

A restricted group can also close channels through which its data could
leave the web UI, and can log who reads it (*Project groups → the group →
Need to know: interfaces and read log*, managers and administrators;
`PUT /api/v1/projects/{code}/policy`). Everything is allowed and nothing is
logged by default; the switches build on a restricted group and are inert
when the group is opened.

| Switch | Denied: the group looks non-existent to … |
|---|---|
| `api_tokens` | REST requests, MCP and the desktop app that sign in with a personal access token (the browser session still works) |
| `mcp` | MCP tools (also through `teamcraft mcp`, which uses a token) |
| `assistant` | the built-in assistant, including the page context of a question |
| `webhooks` | outgoing webhooks: no webhook receives events of the group, scoped or not; an event that mentions the group in a change order is sent without the change |
| `packages` | Pack and Go and BOM downloads (CSV, Excel): the root item's group answers 403, items of such groups inside a structure are left out like restricted items of non-members |

`{"interfaces": {"mcp": false, "webhooks": false}, "read_log": true,
"log_retention_days": 365}` – every field is optional. Switching an
interface off or the log on is open to managers and administrators; allowing
an interface again, switching the log off or changing the retention is, while
administrators are limited by the administrator policy above, for the
managers (like loosening that policy). Every change is audited
(`project.interfaces`, old and new values) and notifies the other managers.
Not covered yet: e-mail and chat notification content, an external LLM
provider list (`external_llm`) and the desktop working folder as separate
switches (the desktop app follows `api_tokens`).

**Read and download log.** With `read_log` on, the group writes `access_log`
entries (separate from the audit log): who (user name kept as text), when,
channel (`web` or `api`), kind (`view` of an item or revision page,
`download` of a file or file version, `package` for Pack and Go, `export` for
a BOM CSV or Excel file), the object (number or file name, kept as text) and
the client address (with the same reverse-proxy rule as the sign-in
throttle). Repeated reads of the same object by the same user through the
same channel within five minutes are one entry, so a resumed or ranged
download is logged once. A read that cannot be logged does not happen. Groups
that do not log write nothing. A Pack and Go or BOM export writes one
entry for every logging group inside the structure, not only for the root
item's group; a refused one writes nothing. The managers and administrators
read the log, but only while they may read the group (an administrator shut
out by `admin_read` gets "not found"; the labels are item content). They read the
newest entries on the group page, or `GET
/api/v1/projects/{code}/access-log?user=&kind=&since=&until=&limit=`. Not
logged yet: MCP and assistant tool reads, desktop Get, thumbnails and
previews.

**Retention and purge.** `log_retention_days` (1 to 36500, empty = keep) is
applied by `teamcraft access-log purge` (run it from cron or a timer):
without options each group's own retention, `--project CODE` limits it to one
group, `--older-than-days N` deletes everything older than that instead.
Every run is audited (`access_log.purge`, with the count). The database
backup contains the log.

Moving an item out of a restricted group, or copying it with *Save As*
into another group, needs a manager of the source group or an administrator
– both declassify the item.

## What outsiders see of links

A visible item can point to a restricted one, and the other way round:

| Link | What a non-member sees |
|---|---|
| BOM line, item reference, simulation basis to a restricted item | a **placeholder row** "Restricted item": find number, quantity, reference kind and basis role stay; number, name, revision, link and the notes (they describe the hidden item) are removed, and the line is never expanded. A hidden simulation basis never says whether it is out of date (that would tell when the hidden item is released or its file checked in). Placeholders are listed after the visible rows of their kind, so their position tells nothing about the hidden number. Editing such a line keeps its stored notes. When a non-member removes or edits it, the history names the real item for members (and is redacted for everybody else) |
| Mass and cost roll-ups | leave restricted lines out; the BOM says "mass and cost exclude N restricted lines" (`restricted_lines`, `rollup_incomplete` in the API) |
| Where-used, referenced-by, simulations based on an item, impact analysis | restricted entries are left out and counted: "+N … in groups you cannot see" |
| CAD file-name matches (file references, suggestions, working sets) | matches in restricted groups are left out without a count; the name shows as not in TeamCraft |
| Similar parts (shape search) | restricted items are left out without a count, also when their file content is identical |
| Release readiness of a change | a restricted unreleased dependency still **blocks** the release and shows as "restricted dependency not released – ask a member of its group or an admin" |
| Pack and Go, desktop Get | restricted items contribute no files; the manifest and the report only count them |
| Change propagation (impacts) | a check on a visible item caused by a restricted release says "a restricted item this depends on was released"; the releasing change order is named only to readers who may see it (a change that also holds restricted items is left out). The same applies to the answer of "Acknowledge all" |
| Audit history | an entry about a link to a restricted item reads "… changed a link to a restricted item"; an entry whose text names restricted items or a change order the reader cannot see (the revisions of a submitted, rejected or released change, flagged dependents, a request's items, a linked change) reads "… (the details name restricted items)" without details. Flagged dependents are listed per group, so the entries about visible dependents stay readable |
| Readiness of a change | the depending revision of a hidden item shows as "restricted item"; another open change the reader cannot see shows as "another open change order" (also in the error when an item is already part of one). Restricted dependencies follow the visible ones of their revision, and rows of hidden depending revisions come last – also in the error of a refused submit – so their place tells nothing about a hidden number |
| Change request items, change order items | a restricted affected item is a placeholder row without its note, listed last |
| *Save As* out of a restricted group | the copy's history entry ("Saved … as a new item") is redacted for readers who may not see the source; the copied file versions say "Copied from a restricted item" |
| Vault explorer folder of a restricted group | "Folder not found", exactly like an unknown folder id |

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
withdraw or cancel, but not approve blind. Adding a reviewer, assigning a
request and the reviewer suggestions need it as well: whether somebody can
see a hidden item would tell who belongs to its group, so an author who
cannot see every item gets no suggestions and cannot add reviewers.

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
  the access. Pending e-mails and chat posts to a user deactivated since
  are not sent either. The groups of the items a notification names are
  stored with it (migration 0026), so it stays hidden from non-members
  after such an item is deleted.
* With `TEAMCRAFT_MATTERMOST_DIRECT=false` (posts in a shared channel),
  notifications about restricted groups show only "new TeamCraft
  notification" and the link – decided when the post is sent, so a group
  restricted after the notification was queued counts. E-mail and
  Mattermost direct messages go to one recipient and carry the full text.
* A webhook sees its own group and the internal groups; a webhook for all
  groups receives **no** events of restricted groups. Scope a webhook to
  the group to get them (see [Event webhooks](webhooks.md)). The
  `revision.released` event of a visible revision released by a change
  that also holds restricted items comes without the `change` object. A
  queued delivery whose group is restricted before it is sent is not sent
  (status `failed`, "Not sent: a project group of the event was restricted
  after it was queued").
* The MCP server and the built-in assistant use the user's rights, so they
  see the same as the user. The assistant sends what a member reads to the
  configured model provider. It reads the user's rights again before every
  tool call, so a membership removed during a run applies at once.
* Text other users typed (request comments, names, notes) reaches the
  assistant and may try to make it copy restricted content into a comment
  or a draft that those users can read. Once a run has read something that
  a reader outside the user's restricted groups would not get (that reader
  is also somebody else: what the user sees only as author, reviewer,
  requester, assignee or admin counts, and so does their worklist – also
  for a user who is in no restricted group), the page the question was
  asked on is such an item or change order, or a
  draft the assistant created or extended holds such content, the
  assistant's text-writing tools (`create_change_draft`,
  `create_change_request`, `comment_change_request`) refuse for the rest of
  the run. External MCP clients act for the user without this check – the
  same caution applies as to any agent working with the user's rights.

## When changes take effect

Memberships and roles are loaded on every request (web, API tokens,
desktop, MCP): removing a member takes effect on the next request. Nothing
is deleted – watches and favorites of the lost items are hidden and come
back with the access, check-outs stay (the removal message says how many).
A manager of the group (or an admin) cancels them on the revision page or
with `POST /api/v1/items/{number}/revisions/{code}/cancel-checkout`:
managers may release the check-outs of users who are no longer members of
the group.

Memberships synchronised from the identity provider change only at the
user's next SSO login; a user removed from the IdP group keeps access until
then, and indefinitely with an API token. Revoke the user's tokens or
deactivate the user as well. Such a membership stays synchronised when its
holder is made a manager, and TeamCraft refuses to remove it (it would
come back at the next SSO login): remove the user from the IdP group, or
have an administrator unlink the group or deactivate the user. A manually
added member who is also in the linked IdP group comes back (synchronised)
at their next SSO login after being removed.

Files already copied to a desktop working folder stay on that computer;
the desktop app marks such revisions as "No longer accessible" and never
syncs them again.

## Upgrading

Migrations 0024 to 0028 run on start and need no action: 0024 adds the
visibility (every existing group stays **internal**, so nothing changes
until a group is restricted), 0025 to 0028 add and back-fill what audit
entries and notifications name. Take a backup first, as before every
upgrade. Behaviour that changes for everybody, also without a restricted
group:

* Placeholder rows ("Restricted item") exist only once a group is
  restricted; for internal groups BOMs, references and notes look as
  before.
* An item of a restricted group that is listed on a change request cannot
  be deleted until it is removed from the request. Deleting an item of an
  internal group removes it from its requests and records that in their
  history.
* A webhook for all groups stops receiving events of a group once it is
  restricted – scope a webhook to the group before restricting it if a
  receiver needs them.
* Before restricting a group that shares the default numbering scheme,
  give it its own scheme (see below), or the shared counter keeps telling
  how many items it creates.

## What stays visible

These are inherent to a shared instance and documented on purpose:

* Item numbers, project group codes and change/request numbers are unique
  across the instance: choosing a taken one says so ("already in use", the
  BOM import says "not available").
* The shared default number counter shows how many items other groups
  create. Give restricted groups their own scheme, e.g.
  `{PROJECT}-{TYPE}-{SEQ:4}`. A restricted group with a scheme of its own
  counts in a private counter: another group whose scheme expands to the
  same text (also one saved before the restriction, or for a later year or
  month) counts on its own, its numbering preview never shows the
  restricted group's numbers, and no scheme is refused because of a
  restricted group. The private counter starts where the shared one stood
  when the group was restricted (numbers everybody could see then) and is
  merged back when the group is opened again. A generated number that is
  already taken by an item the user may see is skipped; one taken by a
  hidden item stops the creation ("already in use; change the scheme or
  enter a number") instead of skipping on, so another group's scheme tells
  of one existing number at a time, like typing a number in does – never
  how many there are, and it does not use up the restricted group's
  numbers. (The built-in shared scheme keeps skipping: its counter is past
  every generated number, so it only meets typed-in numbers.)
* A BOM line that would close a cycle is refused even if the cycle runs
  through restricted assemblies ("would create a circular BOM structure"):
  the structure must stay a tree for everybody.
* Change and request numbers are one sequence for all groups.
* Where-used and referenced-by counts show that restricted items use an
  item – its owner needs to know before making it obsolete or deleting it.
* Audit entries written before this feature (migration 0024) are shown as
  stored; their numbers were visible to everyone at the time. Migrations
  0025 and 0026 mark the older impact, change and request entries with the
  items and changes they name (also items listed in the details that have
  left the change or request since), so these are redacted like new ones.
  Older notifications get their subject (item, change, request) from their
  link (migration 0027) and the items and change orders their text names
  from the numbers in it (migration 0028, e.g. the released source of an
  impact notice), and are hidden like new ones; only those whose subject
  and named objects have been deleted since stay as they are.
* Response times and caches: the preview meshes and shape signatures are
  stored per file content (SHA-256) for the whole instance. Somebody who
  uploads a file with exactly the same bytes as one in a restricted group
  can tell from how fast its preview and its similar-parts card are ready
  that the content exists already – they hold the file themselves, and no
  hidden item is named.
* Audit entries and notifications remember the groups of the items they
  name when they are written (migration 0026). They stay redacted for
  non-members of those groups after the item is deleted or moved out of
  the group. An item of a restricted group that is an affected item of a
  change request cannot be deleted (remove it from the request first), so
  deleting it never makes a hidden request visible; a member who cannot
  see that request learns that one exists. Items of internal groups hide
  no request and are deleted with their request rows; each such request
  records the removal (with the item's note) in its history and tells its
  people, without telling the deleter about requests they cannot see.
* Response times of hidden and missing objects are not equalised.
* Backups and the command line contain everything (operators). `/admin/audit`
  omits and redacts entries about groups hidden from the administrator by a
  need-to-know policy.
