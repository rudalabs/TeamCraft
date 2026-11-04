-- The folder of a file inside its project group, as on the engineer's
-- computer: 'chassis/brackets' for <workspace>/<GROUP>/chassis/brackets/bracket.prt.
-- '' = the top of the project group. The desktop app mirrors this structure
-- in its working folder; the application normalises the value
-- (domain::normalize_folder: forward slashes, no '..', no leading '/').
ALTER TABLE files ADD COLUMN folder TEXT NOT NULL DEFAULT ''
    CHECK (length(folder) <= 400
           AND folder NOT LIKE '/%'
           AND folder NOT LIKE '%/'
           AND position(E'\\' IN folder) = 0
           AND folder !~ '(^|/)\.\.?(/|$)');
