-- Chunked uploads for the web "New item" form: the item and its revision do
-- not exist yet while the files are sent, so such a session has no revision
-- (revision_id IS NULL). It can only join the creation of a new item
-- (services::items::create_with_files); it never joins a revision.
ALTER TABLE upload_sessions ALTER COLUMN revision_id DROP NOT NULL;
