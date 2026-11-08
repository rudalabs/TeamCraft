-- Need-to-know project groups, steps 3 and 4 (issue #151).
--
-- Interface switches: each defaults to TRUE (allowed), so existing groups
-- behave as before. FALSE makes a restricted group look non-existent to that
-- channel (api_tokens: REST and MCP requests with a personal access token;
-- mcp: MCP tools; assistant: the built-in assistant; webhooks: no webhook
-- receives events of the group; packages: Pack and Go and BOM downloads).
ALTER TABLE project_policies
    ADD COLUMN api_tokens BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN mcp BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN assistant BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN webhooks BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN packages BOOLEAN NOT NULL DEFAULT TRUE,
    -- read and download log of the group, off by default
    ADD COLUMN read_log BOOLEAN NOT NULL DEFAULT FALSE,
    -- days the log of this group is kept by `teamcraft access-log purge`;
    -- NULL keeps it until purged by hand with --older-than-days
    ADD COLUMN log_retention_days INTEGER
        CHECK (log_retention_days IS NULL OR log_retention_days BETWEEN 1 AND 36500);

-- Who read or downloaded what. Only groups with `read_log` write here. The
-- user name and the label are copies: the entry outlives a renamed or
-- deleted user, item or file.
CREATE TABLE access_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    username TEXT NOT NULL,
    channel TEXT NOT NULL CHECK (channel IN ('web', 'api')),
    kind TEXT NOT NULL CHECK (kind IN ('view', 'download', 'package', 'export')),
    object_type TEXT NOT NULL,
    object_id UUID,
    label TEXT NOT NULL DEFAULT '',
    client_address TEXT
);
CREATE INDEX access_log_project ON access_log (project_id, at DESC);
CREATE INDEX access_log_dedupe ON access_log (user_id, kind, object_id, at DESC);
