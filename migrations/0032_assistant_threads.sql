-- The conversation of "Ask TeamCraft": one thread per user, kept for the
-- retention period (TEAMCRAFT_ASSISTANT_RETENTION_DAYS). Only the text of the
-- questions and answers and where a question was asked are stored – never
-- tool results. Only the owner reads a thread.
CREATE TABLE assistant_threads (
    id          UUID        PRIMARY KEY,
    user_id     UUID        NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_threads_updated_idx ON assistant_threads (updated_at);

CREATE TABLE assistant_messages (
    id             BIGSERIAL   PRIMARY KEY,
    thread_id      UUID        NOT NULL REFERENCES assistant_threads (id) ON DELETE CASCADE,
    seq            INTEGER     NOT NULL,
    role           TEXT        NOT NULL CHECK (role IN ('user', 'assistant')),
    text           TEXT        NOT NULL,
    -- the quick action of a user message ('ask' = free question)
    action         TEXT        NOT NULL DEFAULT 'ask',
    -- where the question was asked: the resolved page context
    context_kind   TEXT        NOT NULL DEFAULT '',
    context_label  TEXT        NOT NULL DEFAULT '',
    -- the run read restricted content: a follow-up starts tainted
    tainted        BOOLEAN     NOT NULL DEFAULT false,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (thread_id, seq)
);
