-- Scopes of personal access tokens (threat model F2): what a token may do
-- beyond the rights of its user. 'full' = everything the user may do (the
-- behaviour of every token created before: read, write and approve),
-- 'write' = everything but approving (reviewing changes, deciding change
-- requests, making revisions obsolete), 'read_only' = GET/HEAD requests
-- and the read-only MCP tools, 'mcp_only' = only POST /api/v1/mcp.
ALTER TABLE api_tokens
    ADD COLUMN scope TEXT NOT NULL DEFAULT 'full'
        CHECK (scope IN ('full', 'write', 'read_only', 'mcp_only'));

-- A sliding lifetime: a token with idle_days expires after that many days
-- without use; every use (at most once a minute) moves expires_at forward.
-- The desktop app's tokens use it, so an unused copy on a disk dies soon.
ALTER TABLE api_tokens
    ADD COLUMN idle_days INTEGER CHECK (idle_days BETWEEN 1 AND 3650);
