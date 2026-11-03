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

## Notifications

The bell lists review requests, decisions, releases, checks needed and news
about **watched** items and changes (*Watch* on any item or change page).
Optionally TeamCraft sends them by e-mail (SMTP) and to Mattermost (mention
or direct message). Every user chooses channels and mutes kinds on the
account page; delivery runs from an outbox, so a slow mail server never
delays a request.
