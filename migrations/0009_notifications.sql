-- Notifications: in-app (bell, page, API, desktop app) and – optionally – by
-- e-mail (SMTP) and Mattermost. The table is the outbox for both channels:
-- a background task delivers the rows whose email_state / chat_state is
-- 'pending' (see src/delivery.rs).
CREATE TABLE notifications (
    id              UUID PRIMARY KEY,
    user_id         UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind            TEXT        NOT NULL CHECK (kind IN ('review_requested', 'change_update', 'impact',
                                                         'item_update', 'unlock', 'test')),
    title           TEXT        NOT NULL,
    body            TEXT        NOT NULL DEFAULT '',
    -- Path in the web UI (e.g. /changes/<id>) and the object for the desktop
    -- app: subject_type 'change' (subject_ref = change number) or 'revision'
    -- (subject_ref = item number / revision code).
    link            TEXT        NOT NULL DEFAULT '',
    subject_type    TEXT        NOT NULL DEFAULT '' CHECK (subject_type IN ('', 'change', 'revision')),
    subject_ref     TEXT        NOT NULL DEFAULT '',
    actor_name      TEXT        NOT NULL DEFAULT '',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at         TIMESTAMPTZ,
    email_state     TEXT        NOT NULL DEFAULT 'none' CHECK (email_state IN ('none', 'pending', 'sent', 'failed', 'skipped')),
    chat_state      TEXT        NOT NULL DEFAULT 'none' CHECK (chat_state IN ('none', 'pending', 'sent', 'failed', 'skipped')),
    delivery_attempts INTEGER   NOT NULL DEFAULT 0,
    delivery_error  TEXT        NOT NULL DEFAULT ''
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC);
CREATE INDEX notifications_unread_idx ON notifications (user_id) WHERE read_at IS NULL;
CREATE INDEX notifications_outbox_idx ON notifications (created_at)
    WHERE email_state = 'pending' OR chat_state = 'pending';

-- Users follow items and changes ("watch").
CREATE TABLE watches (
    user_id      UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    entity_type  TEXT        NOT NULL CHECK (entity_type IN ('item', 'change')),
    entity_id    UUID        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, entity_type, entity_id)
);
CREATE INDEX watches_entity_idx ON watches (entity_type, entity_id);

-- Per-user settings: channels and muted kinds. Mattermost users are found by
-- their TeamCraft username unless a different one is set.
ALTER TABLE users ADD COLUMN notify_email BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE users ADD COLUMN notify_mattermost BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE users ADD COLUMN mattermost_username TEXT;
ALTER TABLE users ADD COLUMN notify_muted TEXT[] NOT NULL DEFAULT '{}';
