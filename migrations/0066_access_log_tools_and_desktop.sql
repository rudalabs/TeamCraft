-- Need-to-know project groups, step 4 (issue #151): the read log also records
-- reads through MCP tools and the built-in assistant, and the desktop app's
-- Get (working set).
--   channel  mcp        a tool called by an MCP client (token)
--            assistant  a tool called by the built-in assistant
--            desktop    the desktop app's Get
--   kind     workset    the working set of a Get (one entry per group)
--   detail   what read it: the tool name (and the file of read_file_text);
--            empty for the older kinds
ALTER TABLE access_log DROP CONSTRAINT access_log_channel_check;
ALTER TABLE access_log ADD CONSTRAINT access_log_channel_check
    CHECK (channel IN ('web', 'api', 'mcp', 'assistant', 'desktop'));
ALTER TABLE access_log DROP CONSTRAINT access_log_kind_check;
ALTER TABLE access_log ADD CONSTRAINT access_log_kind_check
    CHECK (kind IN ('view', 'download', 'package', 'export', 'workset'));
ALTER TABLE access_log ADD COLUMN detail TEXT NOT NULL DEFAULT '';
