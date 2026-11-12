# ADR 0002 – Notifications and webhooks are an outbox written in the event's transaction

**Status:** accepted – notifications with e-mail and Mattermost delivery (migration `0009_notifications.sql`), event
webhooks (migration
`0022_webhooks.sql`, commit *Event webhooks: signed JSON for releases,
changes, requests and simulations*).

## Context

Events such as a submitted change, a release or a flagged dependent have to
reach people (in-app, e-mail, Mattermost) and other systems (webhooks). A mail
server or a webhook receiver can be slow, down or misconfigured; the event
itself must neither wait for them nor be lost or announced twice.

## Decision

* **Rows in the same transaction as the event.** The services create
  notifications (`services::notifications`) and webhook deliveries (`services::webhooks::emit`, one row per matching
  webhook in
  `webhook_deliveries`) inside the transaction that changes the data. A
  notification or delivery exists exactly when the event was committed.
* **Background senders read the rows.**
    * `src/delivery.rs`: the `notifications` table is the outbox for e-mail and
      Mattermost (`email_state` / `chat_state` = `pending`); a task sends the
      pending ones every few seconds, retries failures and marks them `failed`
      after `MAX_ATTEMPTS` (5); without configuration they are marked
      `skipped`. One instance delivers (no row locking). A run serves a few
      recipients at the same time (`PARALLEL_RECIPIENTS`, small because mail
      services limit the connections of a sender); the notifications of one
      recipient keep their order.
    * `src/webhooks.rs`: rows are claimed with `FOR UPDATE SKIP LOCKED` and a
      lease, signed with HMAC-SHA256, retried with exponential backoff and
      jitter up to `MAX_ATTEMPTS` (8); redirects are never followed and
      response bodies are not stored; finished deliveries are purged.
* The in-app view (bell, page, API, desktop) reads the same notification rows.

## Consequences

* A failing mail server or webhook receiver never fails or slows down the
  event; it is retried and finally marked failed.
* A rolled-back event leaves no notification and no delivery behind – proven
  by injected failures in `tests/atomic_processes.rs` (change order release:
  a failing notification insert or webhook delivery insert leaves nothing).
* Delivery is at-least-once for webhooks (the `X-TeamCraft-Delivery` id is the
  same for every retry, so receivers can de-duplicate).
* E-mail and Mattermost delivery assume one sending instance; several servers
  would need row locking there (the webhook sender already has it).

## Alternatives turned down

Not documented in the repository: no research note, commit message or
`OPEN_WORK.md` entry records which alternatives (sending synchronously in the
request, a message broker, a separate queue service) were considered.

## Where it is held

| What                                                                        | Where                                                            |
|-----------------------------------------------------------------------------|------------------------------------------------------------------|
| Notifications created in the event's transaction                            | `src/services/notifications.rs` (module documentation)           |
| E-mail / Mattermost sender                                                  | `src/delivery.rs`                                                |
| Webhook outbox writer, sender, SSRF guard                                   | `src/services/webhooks.rs` (`emit`), `src/webhooks.rs`           |
| Deliveries, signatures, retries, SSRF                                       | `tests/webhooks.rs`                                              |
| No notification or delivery survives a rolled-back release                  | `tests/atomic_processes.rs`                                      |
| Writing webhook rows needs no audit entry of its own (the event is audited) | `NOT_AUDITED` in `tests/architecture_guards.rs`, proposed in #96 |
