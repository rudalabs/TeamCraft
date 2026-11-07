# Change requests, change orders, reviews and notifications

## Change requests (ECR) → change orders (ECO)

A **change request** (ECR, `ECR-000001`) is the step *before* a change
order: somebody found a problem or has an idea – on the shop floor, in a
simulation, in a test, in purchasing – and asks for a change. Every
signed-in user may raise one, **viewers included**, and everybody may
comment on it. A **change order** (ECO, `CHG-000001`) is the engineering
work that answers it: it carries the new revisions through review to the
release.

```text
 anybody signed in                     CAD editors of the affected project groups / admins
 ─────────────────                     ──────────────────────────────────────────────────
 Request change… ──▶ New ──▶ In Triage ──▶ Accepted ──▶ change order (ECO): create one with the
  (revision page,     │          │                       in-work revisions, or link an open one
   file row, desktop) │          │     ──▶ Deferred ──▶ In Triage (taken up again)     │
                      │          │     ──▶ Rejected                                    │ released
                      └──────────┴──▶ Withdrawn (by the requester, before the decision) ▼
                                                                                Implemented
```

1. **Raise** – *Request change…* on a revision page, on a file row, in the
   *Requests* list, or in the desktop app on a revision tab or a workspace
   file. Give a title, the problem and the proposed solution, a reason
   (defect, improvement, cost, manufacturing, supplier, safety, customer,
   other), a priority and optionally a date it is needed by. The revision and
   the file name are filled in. Screenshots, mark-ups and PDF reports can be
   attached (PNG, JPEG, GIF, WebP or PDF, up to 10 MiB each – the type is
   checked by the content, not the name). The CAD editors of the project
   group of the item and the watchers of the item are notified; the
   requester watches the request.
2. **Discuss** – everybody comments in the discussion thread; the
   requester, the assignee, everybody who commented and the watchers are
   notified.
3. **Triage** – a CAD editor of an affected project group (or an admin)
   *starts the triage* (and is assigned if nobody is), assigns it to the
   engineer who will do the work and decides:
   * **Accept** – the change will be made;
   * **Defer** – not now (reason required); *take up again* later;
   * **Reject** – no change (reason required).
   While no item is named, any CAD editor may triage; once the request is
   decided, an item is added only by a member of its project group (or an
   admin).
4. **Implement** – an accepted request gets its change order: *Create change
   order* creates a draft with the in-work revisions of the affected items
   (items without a revision in work are listed – revise them and add the
   revision to the draft), or *Link* an existing open change order. The
   change order runs through review as usual.
5. **Implemented** – when a linked change order is released, the request
   becomes *implemented* in the same transaction, and everybody involved is
   told.

The requester can **withdraw** the request while it is *New* or *In Triage*.
The revision page shows the open requests on its item (with their number on
the *Request change…* button); the dashboard and the desktop worklist list
the requests *assigned to me* and *my open requests*. Every step is in the
audit log and in the status history of the request.

## Change orders

A change collects one or more revisions *in work*, a title, description,
priority and reviewers.

1. **Draft** – the author adds revisions (*Release via new change…* on a
   revision page) and reviewers – or the draft is created from an accepted
   change request.
2. **Submit** – the revisions move to *In Review*; they are frozen.
3. Each reviewer **approves** or **rejects** with a reason. Reviewers must be
   independent (see *Separation of duties* below).
4. When everybody approved, **all revisions are released together**, and
   the accepted change requests the change implements become *implemented*.
   A rejection sends them back to *WIP*; fix, *reopen* and resubmit – or *cancel* the change (takes effect at once).

### Release readiness

A change releases a whole product at once. Before it can be submitted,
every revision in it is checked against what it depends on:

| Dependency | Not released and not in this change | Obsolete |
|---|---|---|
| BOM lines | blocks the submit | warning |
| item references (drawing, specification, reference) | blocks the submit | warning |
| CAD file references (found by the file scan) | warning | warning |

The *Release readiness* card of a draft change lists them. *Add* puts a
single in-work revision into the change. *Add all to this change* adds
all of them, and level by level their own unreleased dependencies, in one
step. Dependencies that are in review in another change have to be
released there first. Dependencies of a project group you are not a member
of are skipped by *Add all*; a member has to add or release them. The desktop app does the same with *Include
unreleased dependencies* in the *Release…* dialog (on by default).

### Separation of duties

Nobody reviews work they did themselves. TeamCraft refuses a reviewer who

- created the change order (the author),
- submitted it, or prepared it as an administrator for the author (added
  items or reviewers, edited or submitted the draft – read from the audit log), or
- **changed the revisions under review**: created a revision, uploaded a
  file to it, or has a logged check-in, revision, file, BOM (including BOM
  import and replace) or reference edit on it.

The rule is checked when a reviewer is added (400), when the change is
submitted (409, naming the reviewer to remove) and again when a reviewer
decides (403), because people can edit a draft after they were added. An
administrator can still act for the author, but then cannot also review that
change. The rule is always on; a team needs at least one person who did not
touch the files. The user who submitted a change is stored in
`changes.submitted_by`. Changes that were already in review when the rule
was introduced keep working: only the author is refused on those until they
are withdrawn and submitted again.

### Suggested reviewers

Next to the reviewer picker of a draft change, TeamCraft suggests up to
five people who know the affected items, each with a one-click *Add* and
the reasons in plain text. The suggestions come from the history TeamCraft
already keeps – no AI, the same data always gives the same order:

| Source | Example reason | Points |
|---|---|---|
| edits of an earlier revision of an affected item in the last 90 days (check-ins, uploads, BOM and attribute edits) | edited PRT-000005 3 times in the last 90 days | 3 + one per edit (at most 10 per item) |
| a decision on an earlier change with the same items | reviewed ECO-000012 (same items) | 4 (still pending: 2) |
| owner of a dependent: creator or author of the latest revision of an item that uses, documents or references an affected item | owns ASM-000003, which uses PRT-000005 | 2 |
| manager of the project group of an affected item | manager of ROVER | 3 |
| requester of a change request this change implements | raised ECR-000007, implemented by this change | 1 |

Each source counts at most a few times per person, so one busy source
cannot outweigh the others. Only active CAD editors and admins – the users
who may review – are suggested, never the author, never the
administrator who is preparing the change and never someone already
assigned. People who changed the affected revisions know the item best but cannot
review it: they are listed after the others as *knows the item, cannot review*
without an *Add* button. The desktop app shows the same suggestions on the tab of a draft
change, the REST API has `GET /api/v1/changes/{number}/suggested-reviewers`
and AI assistants the MCP tool `suggest_reviewers`. Suggestions never
assign anybody.

## Notifications

The bell lists review requests, decisions, releases, checks needed, change
requests and news about **watched** items, changes and change requests
(*Watch* on any of their pages).
Optionally TeamCraft sends them by e-mail (SMTP) and to Mattermost (mention
or direct message). Every user chooses channels and mutes kinds on the
account page; delivery runs from an outbox, so a slow mail server never
delays a request.

With e-mail switched on, CAD editors also get a **digest of the change
requests waiting for triage** – one e-mail per project group, by default
once a day: the requests raised since the last digest that are still *New*,
and the numbers of the older ones that still wait. No new request, no
e-mail. The digest also appears in the bell's list, already marked as read.
Mute *E-mail digest of new change requests …* on the account page if the
immediate notifications are enough.

Other systems – CI, ERP, CAM, a team channel – can react to releases,
reviews and change requests through [event webhooks](../admin/webhooks.md),
which administrators set up.
