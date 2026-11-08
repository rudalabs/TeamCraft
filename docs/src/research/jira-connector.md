# Jira connector for change requests and change orders

Many engineering teams plan and track their work in Jira. This page looks at
what Jira's REST APIs offer, how other PDM/PLM systems connect to Jira, and
how TeamCraft could mirror its engineering change requests (ECR) and change
orders (ECO) as Jira issues. Date: 2026-11-05.

## Summary

* Both Jira Cloud (REST API v3) and Jira Data Center (REST API v2) offer
  everything a connector needs: create and update issues, run workflow
  transitions, add comments, labels and **remote links** back to TeamCraft.
  A plain HTTPS client is enough (`reqwest` + `serde`, which TeamCraft
  already uses). There is no SDK to license, and no Atlassian app is needed.
* Authentication: Cloud uses an e-mail address plus an API token (HTTP Basic),
  ideally the token of a dedicated service account. Data Center uses a
  personal access token (Bearer). Both are a single secret per connection.
* **Recommended: one-way first** (TeamCraft → Jira), sent from an outbox
  shaped like `webhook_deliveries`. In TeamCraft the change process (reviews,
  four-eyes release) stays the single source of truth. Jira only shows it.
  Jira → TeamCraft (webhooks) comes later and is limited to comments and
  links, never approvals or releases.
* Each connection gets an explicit **project-group scope**. Only changes and
  requests of the allowed groups go to that Jira project, so data from
  restricted groups never leaves TeamCraft by accident.

## Jira REST APIs

| Topic                              | Jira Cloud (v3)                                                                                                                                              | Jira Data Center (v2)                                                                                        |
|------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------|
| Base URL                           | `https://<site>.atlassian.net/rest/api/3/…`; scoped API tokens go through `https://api.atlassian.com/ex/jira/<cloudId>/…`                                    | `https://jira.example.internal/rest/api/2/…`                                                                 |
| Auth for a server-side integration | Basic `e-mail:API token` (passwords are deprecated); OAuth 2.0 (3LO) or Forge/Connect apps are meant for distributed apps                                    | personal access token as `Authorization: Bearer` (Jira 8.14+), optional expiry, can be disabled by the admin |
| Create / edit issue                | `POST /issue`, `PUT /issue/{key}`                                                                                                                            | same paths under `/rest/api/2`                                                                               |
| Status change                      | `GET /issue/{key}/transitions` (IDs depend on the workflow), then `POST …/transitions {"transition":{"id":…}}`; 204 on success; screen fields go in `fields` | same                                                                                                         |
| Link back                          | `POST /issue/{key}/remotelink` with `globalId`. A second POST with the same `globalId` **updates** the link (upsert), `status.resolved` shows it as done     | same (available since Jira 5.0)                                                                              |
| Rich text                          | description, comments and textarea custom fields in **ADF** (JSON document). API v2 on Cloud still takes wiki markup                                         | wiki markup strings                                                                                          |
| Search                             | `GET/POST /search/jql` with `nextPageToken`. The old `/search` (offset paging, `total`) has been removed                                                     | `/search` with `startAt`/`maxResults`                                                                        |
| Custom fields                      | `customfield_NNNNN`; IDs and required fields differ per project and issue type (`GET /issue/createmeta/{project}/issuetypes/{id}`)                           | same                                                                                                         |
| Own metadata on an issue           | issue entity properties (`PUT /issue/{key}/properties/{k}`, up to 32 KB of JSON; searchable by JQL only when indexed by a Connect/Forge app)                 | same                                                                                                         |
| Idempotency                        | no idempotency key on `POST /issue`; a retry after a timeout can create a duplicate                                                                          | same                                                                                                         |

**Rate limits (Cloud).** Jira Cloud enforces three independent limits: an
hourly points quota (1 point per request plus points per returned object),
per-second burst limits per endpoint (100 GET/POST, 50 PUT/DELETE per second),
and **per-issue write limits** (20 writes per 2 s, 100 per 30 s). The quota
enforcement that started on 2026-03-02 applies to Forge, Connect and OAuth
apps, *not* to API-token traffic. API-token traffic is still subject to the
burst and per-issue limits. A 429 response carries `Retry-After` and
`RateLimit-Reason`. Atlassian recommends waiting at least `Retry-After`,
then exponential backoff with jitter, and retrying only idempotent requests.
At TeamCraft's volume (a few events per change) this is not a real limit.
The outbox just has to honour `Retry-After`.

**Webhooks from Jira.**

* *Cloud admin webhooks:* a Jira administrator registers them in the UI or
  through `/rest/webhooks/1.0/webhook`. They can carry a JQL filter and an
  optional secret. Requests are then signed with
  `X-Hub-Signature: sha256=<HMAC of the raw body>`, the same scheme
  TeamCraft uses for its own webhooks.
* *Dynamic webhooks* (`/rest/api/3/webhook`) are available only to OAuth and
  Connect apps. They are limited to 5 per OAuth app and user, and expire
  after 30 days unless refreshed.
* Jira retries failed deliveries up to 5 times, roughly 5–15 minutes apart.
  Duplicates can happen; deduplicate them with `X-Atlassian-Webhook-Identifier`.
* *Data Center:* an administrator registers webhooks under *System → WebHooks*,
  also with an optional secret (HMAC-SHA256 in `X-Hub-Signature`). Since
  Jira 10 they are sent asynchronously.

Either way, a Jira administrator has to set up the incoming direction by hand.
An API token alone cannot do it.

**Lifecycle of Data Center.** Atlassian ends Jira Data Center on 2029-03-28 (end of sale 2028). Self-hosted users will
nevertheless run it for years, and
the API is almost the same. Supporting both costs one `flavor` switch (v2/v3 path, wiki markup vs ADF, Basic vs Bearer,
search paging).

## How other PDM/PLM systems integrate

| System               | What is synced                                                                                                                            | Direction                                           | How                                                                                                                        |
|----------------------|-------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| Duro                 | creates a Jira ticket for every new change order                                                                                          | one-way (Duro → Jira)                               | built-in, via the integration platform Prismatic                                                                           |
| Arena (PTC)          | ECOs and quality processes; Jira issues are created and updated as the workflow progresses                                                | Arena → Jira (templates in "Arena Connect", 2026.2) | built-in connector; also the commercial PROSTEP OpenPDM connector (two-way)                                                |
| PTC Windchill        | change requests, problem reports, change notices ↔ issues                                                                                 | two-way                                             | commercial third-party tools: OpsHub (copies fields, comments, attachments) or Sodius OSLC Connect (live links, no copies) |
| PTC Codebeamer (ALM) | work items ↔ issues, polling as often as every 2 minutes                                                                                  | two-way                                             | PTC integration plus third-party connectors (OpsHub, Nanga)                                                                |
| Aras Innovator       | items ↔ issues with field mapping, comments, attachments                                                                                  | two-way                                             | commercial OpsHub app; community posts describe custom code                                                                |
| Onshape              | no documented native Jira integration; release candidates can wait for open tasks, webhooks notify external apps of release state changes | –                                                   | public webhooks and REST API for custom integrations                                                                       |
| OpenBOM              | no documented Jira integration found; change requests and change orders are built in                                                      | –                                                   | –                                                                                                                          |

Observations:

* The built-in integrations (Duro, Arena) are **one-way**: the PLM system's
  change object creates and updates a Jira issue. Two-way sync is mostly
  sold as separate middleware (OpsHub, PROSTEP), with conflict policies per
  field. That alone shows how much work it is.
* The link-based approach (Sodius/OSLC) avoids duplicate data. Jira shows a
  link and a status, and the details stay in the PLM system.
* Pitfalls reported or implied by these tools are workflow transition IDs that
  differ per project, required custom fields per issue type, duplicate issues
  after retries, conflicting edits in two-way sync, and permission leaks when
  Jira users see data they could not see in the PLM system.

## Licensing and feasibility

* The REST APIs are publicly documented. Calling them requires no Atlassian
  licence or SDK beyond the customer's own Jira. An MPL-2.0 client of a
  documented HTTP API raises no licence question.
* No new heavy dependencies. TeamCraft already has `reqwest`, `serde_json`
  and HMAC-SHA256 (`webhooks::hmac_sha256` / `verify_signature`, which can
  verify Jira's `X-Hub-Signature` directly). ADF for plain paragraphs, a link
  and a short bullet list is a few dozen lines of `serde_json::json!`.
* OAuth 2.0 (3LO) would require every self-hosting admin to register their own
  app in the Atlassian developer console, and to handle a callback and rotating
  refresh tokens. That is not worth it for a server-to-server connection.
  The connector uses an API token or PAT instead. Atlassian recommends OAuth
  for distributed apps, but TeamCraft is software the customer runs
  themselves, with a token the customer's own admin issues for a service
  account. Forge/Connect apps are hosted on or registered with Atlassian, so
  they are out of scope.
* Testing needs no live Jira: a fake HTTP server in the tests (like the
  existing webhook delivery tests) is enough.

## Recommended design for TeamCraft

### What maps to what

| TeamCraft                                            | Jira                                                                                                                                                           |
|------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| change request `ECR-000123` (`services/requests.rs`) | issue of a configurable type (e.g. *Task* or a custom *ECR* type)                                                                                              |
| change order `ECO-000045` (`services/changes.rs`)    | issue of a configurable type; optionally linked to the request's issue (issue link "relates to"), or a sub-task                                                |
| title, description, reason, priority, requester      | summary `ECR-000123: <title>`, description (ADF / wiki markup), priority (mapping), label `teamcraft` + `teamcraft-ecr-000123`                                 |
| affected items / revisions                           | bullet list in the description (number, revision, name), each linking to TeamCraft. Only the items of the allowed groups are listed                            |
| TeamCraft page                                       | remote link with `globalId = teamcraft:<instance>:change/<uuid>`, title = number + status, `status.resolved` when released, implemented, rejected or cancelled |
| status change                                        | Jira transition by **target status name**, mapped per connection                                                                                               |
| reviews, decisions                                   | a comment ("Approved by …", "Rejected: <reason>")                                                                                                              |

Default status mapping, editable per connection:

| TeamCraft state                    | Jira status (default)                                             |
|------------------------------------|-------------------------------------------------------------------|
| ECR `new`, `in_triage`, `deferred` | To Do                                                             |
| ECR `accepted`                     | In Progress                                                       |
| ECR `implemented`                  | Done                                                              |
| ECR `rejected`, `withdrawn`        | Done (+ resolution "Won't Do" if the transition screen allows it) |
| ECO `draft`, `rejected`            | To Do / In Progress                                               |
| ECO `in_review`                    | In Review (falls back to In Progress)                             |
| ECO `released`                     | Done                                                              |
| ECO `cancelled`                    | Done ("Won't Do")                                                 |

The connector looks up the available transitions of the issue and picks the
one whose target status has the mapped name. This keeps it independent of
workflow-specific transition IDs. If none exists (the workflow is different),
it writes a comment and records a warning on the delivery instead of failing
forever.

### Events

These are the existing `WebhookEvent`s, so the same `emit` call sites are
reused:

* `change_request.created` → create the issue + remote link.
* `change_request.decided` → transition + comment.
* `change.submitted` / `change.approved` / `change.rejected` /
  `change.released` → create the issue on first contact, transition and
  comment.
* Still missing for complete mirroring (to be added as webhook events too):
  ECR comment added, request linked to a change order, change created, change
  withdrawn, reopened or cancelled, request implemented. Adding them also
  benefits the generic webhooks.

### Delivery: building on the webhook outbox

* New tables `jira_connections` (name, flavor `cloud|datacenter`, base URL,
  Jira project key, issue types for ECR/ECO, status and priority mapping,
  label, active flag, **project-group scope**) and `jira_links`
  (connection, TeamCraft object id, Jira issue id + key, last synced status).
* `jira_deliveries`, written in the event's transaction like
  `webhook_deliveries` (same claim with `FOR UPDATE SKIP LOCKED`, lease,
  exponential backoff with jitter, `MAX_ATTEMPTS`, retention). Deliveries of **one object are processed in order**:
  claim only the oldest open delivery
  per (connection, object), so a transition never overtakes the create. The
  per-issue write limit makes this sensible anyway.
* The sender does not post the stored payload. It **reads the current state**
  when it sends, so retries and late deliveries converge on the latest state,
  and an item moved to a group outside the scope is no longer sent.
* Idempotent create:
    1. Before `POST /issue`, look up `jira_links`.
    2. After a timeout or 5xx, search by the unique label (`labels = "teamcraft-eco-000045"`) before creating again.
    3. Remote links are upserts by `globalId`.
* A 429 response reschedules the delivery after `Retry-After`. A 401/403
  response pauses the connection and notifies the admins (the existing error
  notification). A 400 response (missing required field) is a permanent error
  with Jira's message stored, and admins can retry it as with webhooks today.
* **SSRF guard:** reuse `TargetPolicy` and the checking resolver. An on-prem
  Jira is usually on a private address, so the admin adds its host to
  `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS` (or a separate
  `TEAMCRAFT_JIRA_ALLOWED_HOSTS`, an open question). Redirects are not
  followed, and the connection base URL is fixed per connection, never taken
  from payloads.

### Configuration and secrets

* The connection is configured by an admin in the UI and stored in the
  database. The **secret is not stored there**. Each connection names an
  environment variable (via `config.rs`, also as `NAME_FILE`, e.g.
  `TEAMCRAFT_JIRA_<NAME>_TOKEN_FILE=/run/secrets/jira_token`, plus
  `…_EMAIL` for Cloud). This fits the rule that secrets live only in
  `secrets/`, and nothing secret is ever shown in the UI or written to the
  audit log.
* An alternative is to store the token encrypted in the database (like the
  webhook secret). That needs a key in `secrets/`, so it only moves the
  problem. The env/file approach is simpler for the MVP.
* Document everything in `docs/src/admin/configuration.md` and a new
  `docs/src/admin/jira.md`.

### Admin UI

The page `/admin/jira` lists the connections. An admin can:

* edit a connection, with the project-group scope as checkboxes;
* use **Test connection**, which calls `GET /myself` and the project's
  createmeta and shows missing required fields and unknown status names;
* see each connection's deliveries with status and error, and retry them (same layout as the webhook deliveries);
* use **Sync now** for one ECR or ECO;
* read the Jira issue key, which is shown as a link on the ECR and ECO pages.

### Read permissions and project-group scope

* Each connection holds an explicit list of project groups, with no "all"
  default. A change request or order is synced only when **all** its
  affected items belong to allowed groups. Mixed changes are either
  skipped (the default) or synced with only the allowed items listed. This
  is a per-connection setting, and the open question is which default to use.
* A request without items (possible for an ECR) belongs to the groups chosen
  by the requester or is not synced until it has items.
* The rule is checked again at send time, because items can move to other
  groups and the read-permission feature being added can change who may see
  what. Once an object falls out of scope, no more updates are sent. The
  existing Jira issue stays, and the admin is told about it.
* Payload contents are deliberately small: number, title, description,
  status, item numbers and names, links. No file contents, no file names of
  other groups, no user e-mail addresses.

## Phases

**MVP (one-way, TeamCraft → Jira):**

1. `jira_connections` (Cloud and Data Center), secrets via `*_FILE`, admin
   page with *Test connection*.
2. Explicit project-group scope per connection, checked at queue time and at
   send time.
3. ECR and ECO → issue creation (configurable issue types), summary with
   number, short description with affected items and link.
4. Remote link back to TeamCraft, upserted by `globalId`, and the Jira key
   shown in TeamCraft.
5. Status mapping by target status name, configurable per connection.
6. Comments for decisions, approvals and rejections.
7. Outbox `jira_deliveries` with SKIP LOCKED, ordered per object, backoff,
   `Retry-After`, SSRF allow-list, idempotent create via label.
8. Audit records for connection changes; delivery list and retry in the UI.
9. Tests with a fake Jira HTTP server, plus docs.

**Later:**

* Incoming Jira webhooks (admin-registered, HMAC-verified,
  deduplicated): Jira comments appear on the ECR, and resolving the Jira
  issue is shown as a hint. Jira never approves or releases anything.
* Raise an ECR from Jira (a webhook on a label or issue type, or a link in
  Jira opening the TeamCraft form prefilled).
* Link existing Jira issues to an ECR by key; JQL-based import.
* Custom field mapping (e.g. ECO number into a custom field).
* Sync attachments only on explicit request.
* Several Jira projects per group, e.g. routed by item type.
* OAuth 2.0 (3LO) only if a hosted or multi-tenant use case appears.

## Open questions for the user

* Cloud, Data Center or both from the start?
* Should change orders become their own issues, or sub-tasks or comments of
  the request's issue?
* What should happen to changes that mix allowed and restricted groups: skip
  them, or sync only the allowed items?
* Should the Jira host go into the existing webhook allow-list or a separate
  one?
* Is one-way enough for the MVP, or are Jira comments in TeamCraft needed
  early?

## Sources

* [Jira Cloud: Basic auth for REST APIs](https://developer.atlassian.com/cloud/jira/platform/basic-auth-for-rest-apis/)
* [Jira Cloud REST API v3](https://developer.atlassian.com/cloud/jira/platform/rest/v3/intro/)
* [Jira Cloud: rate limiting](https://developer.atlassian.com/cloud/jira/platform/rate-limiting/)
* [Jira Cloud: webhooks](https://developer.atlassian.com/cloud/jira/platform/webhooks/)
* [Jira Cloud: entity properties](https://developer.atlassian.com/cloud/jira/platform/jira-entity-properties/)
* [Atlassian Document Format](https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/)
* [Run JQL search query using Jira Cloud REST API (KB, `/search` removed,
  `/search/jql`)](https://confluence.atlassian.com/display/JIRAKB/Run+JQL+search+query+using+Jira+Cloud+REST+API)
* [Atlassian Community: scoped API tokens via api.atlassian.com/ex/jira](https://community.atlassian.com/forums/Jira-questions/Jira-Token-With-Scopes/qaa-p/3081310)
* [Jira Data Center: personal access tokens](https://confluence.atlassian.com/enterprise/using-personal-access-tokens-1026032365.html)
* [Jira Data Center: managing webhooks](https://confluence.atlassian.com/adminjiraserver112/managing-webhooks-1688896467.html)
* [Jira Data Center: REST API for remote issue links](https://developer.atlassian.com/server/jira/platform/jira-rest-api-for-remote-issue-links/)
* [Atlassian: Data Center end of life](https://www.atlassian.com/licensing/data-center-end-of-life)
* [Duro changelog: Jira integration](https://next.durolabs.co/changelog/2514d5e9-8263-4e90-a466-6e4ca75ead30), [Duro product update (Prismatic)](https://durolabs.co/webinar/product-updates-integrations/)
* [PTC: Arena Connect announcement](https://www.ptc.com/en/news/2026/ptc-launches-arena-connect), [PROSTEP: Arena and Jira](https://www.prostep.com/en/solutions/solutions-for-ptc/integrate-arena-with-jira)
* [OpsHub: PTC Windchill integration for Jira](https://marketplace.atlassian.com/apps/1236668), [Sodius OSLC Connect for Windchill and Jira](https://www.sodiuswillert.com/en/products/oslc-connect-for-windchill/atlassian-jira-software)
* [PTC Codebeamer integrations](https://www.ptc.com/en/products/codebeamer/integrations), [Codebeamer and Jira (Marketplace)](https://marketplace.atlassian.com/apps/1234937)
* [OpsHub: Aras integration for Jira](https://marketplace.atlassian.com/apps/1235048)
* [Onshape: tasks and release management](https://www.onshape.com/en/resource-center/tech-tips/tasks-release-management), [Onshape: sync releases and revisions](https://onshape-public.github.io/docs/tutorials/releases)
* [OpenBOM: change management](https://www.openbom.com/blog/change-management-in-openbom-revision-change-request-and-change-orders)
