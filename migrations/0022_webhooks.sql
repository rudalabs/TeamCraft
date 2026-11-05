-- Outgoing event webhooks (services/webhooks.rs, src/webhooks.rs): an
-- administrator registers a URL for some events ("revision.released", …),
-- optionally limited to one project group. The services write one row per
-- matching webhook into webhook_deliveries in the transaction of the event
-- (the outbox); a background task POSTs it as signed JSON and retries with
-- exponential backoff.
CREATE TABLE webhooks (
    id                UUID PRIMARY KEY,
    name              TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    url               TEXT        NOT NULL CHECK (url ~* '^https?://' AND length(url) <= 2000),
    -- HMAC-SHA256 key of the X-TeamCraft-Signature header. The server needs
    -- the key itself to sign, so it is stored as generated (unlike password
    -- and token hashes); it is shown once after creation or rotation and is
    -- never returned again – the UI and the API show secret_hint only.
    -- Whoever can read this table or a database backup can sign payloads.
    secret            TEXT        NOT NULL CHECK (length(secret) >= 32),
    secret_hint       TEXT        NOT NULL,
    events            TEXT[]      NOT NULL CHECK (
        cardinality(events) > 0 AND events <@ ARRAY[
            'revision.released', 'revision.obsoleted',
            'change.submitted', 'change.approved', 'change.rejected', 'change.released',
            'change_request.created', 'change_request.decided',
            'simulation.result_added'
        ]::TEXT[]
    ),
    -- Only events that concern items of this project group (NULL = all).
    project_id        UUID        REFERENCES projects (id),
    active            BOOLEAN     NOT NULL DEFAULT TRUE,
    created_by        UUID        REFERENCES users (id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    secret_rotated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The outbox. `id` is sent as X-TeamCraft-Delivery and stays the same over
-- retries (receivers deduplicate on it); `event_id` is the payload's `id`,
-- shared by the deliveries of one event to several webhooks. `attempts`
-- counts the attempts started: the worker claims a row by incrementing it
-- and moving next_attempt_at past a lease, so a crashed sender's row is
-- retried later and two servers never send the same attempt
-- (FOR UPDATE SKIP LOCKED). Response bodies are not stored.
CREATE TABLE webhook_deliveries (
    id               UUID PRIMARY KEY,
    webhook_id       UUID        NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    event_id         UUID        NOT NULL,
    event            TEXT        NOT NULL CHECK (event IN (
        'revision.released', 'revision.obsoleted',
        'change.submitted', 'change.approved', 'change.rejected', 'change.released',
        'change_request.created', 'change_request.decided',
        'simulation.result_added', 'ping'
    )),
    payload          JSONB       NOT NULL,
    status           TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts         INTEGER     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status_code INTEGER,
    last_error       TEXT        NOT NULL DEFAULT '' CHECK (length(last_error) <= 500),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at     TIMESTAMPTZ
);
-- The worker: due rows in order.
CREATE INDEX webhook_deliveries_due_idx ON webhook_deliveries (next_attempt_at)
    WHERE status = 'pending';
-- The admin page: the latest deliveries of a webhook.
CREATE INDEX webhook_deliveries_webhook_idx ON webhook_deliveries (webhook_id, created_at DESC);
-- Housekeeping: finished rows by age.
CREATE INDEX webhook_deliveries_done_idx ON webhook_deliveries (updated_at)
    WHERE status <> 'pending';
