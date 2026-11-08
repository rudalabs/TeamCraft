-- Need-to-know project groups, step 3, remaining interface switches (issue #151).
--
-- Each defaults to TRUE (allowed), so existing groups behave as before.
--   notification_content: FALSE = e-mail and chat notifications that touch the
--     group carry only a generic text and the link (no titles, numbers, text).
--   external_llm: FALSE = the built-in assistant sees the group only when the
--     configured provider base URL is on the administrator's list of internal
--     URLs (TEAMCRAFT_ASSISTANT_INTERNAL_URLS).
--   desktop_sync: FALSE = the desktop app's Get (working set) leaves the group
--     out; the group looks non-existent to it.
ALTER TABLE project_policies
    ADD COLUMN notification_content BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN external_llm BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN desktop_sync BOOLEAN NOT NULL DEFAULT TRUE;
