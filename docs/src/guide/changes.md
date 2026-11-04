# Change orders, reviews and notifications

## Change orders

A change collects one or more revisions *in work*, a title, description,
priority and reviewers.

1. **Draft** – the author adds revisions (*Release via new change…* on a
   revision page) and reviewers.
2. **Submit** – the revisions move to *In Review*; they are frozen.
3. Each reviewer **approves** or **rejects** with a reason. The author never
   reviews their own change.
4. When everybody approved, **all revisions are released together**.
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
released there first. The desktop app does the same with *Include
unreleased dependencies* in the *Release…* dialog (on by default).

## Notifications

The bell lists review requests, decisions, releases, checks needed and news
about **watched** items and changes (*Watch* on any item or change page).
Optionally TeamCraft sends them by e-mail (SMTP) and to Mattermost (mention
or direct message). Every user chooses channels and mutes kinds on the
account page; delivery runs from an outbox, so a slow mail server never
delays a request.
