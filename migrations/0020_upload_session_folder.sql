-- The folder a chunked upload goes to inside its project group (like
-- files.folder from 0018, normalised by domain::normalize_folder). NULL keeps
-- the folder of an existing file ('' for a new one); a `folder`/`folders`
-- field of the multipart check-in that takes the upload applies then.
ALTER TABLE upload_sessions ADD COLUMN folder TEXT
    CHECK (folder IS NULL
           OR (length(folder) <= 400
               AND folder NOT LIKE '/%'
               AND folder NOT LIKE '%/'
               AND position(E'\\' IN folder) = 0
               AND folder !~ '(^|/)\.\.?(/|$)'));
