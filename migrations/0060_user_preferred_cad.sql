-- Preferred CAD program of the user (a format family, see cadformat::families):
-- native files of other programs are shown collapsed in the file lists.
-- '' = show all (default). Unknown values are treated as ''.
ALTER TABLE users ADD COLUMN preferred_cad TEXT NOT NULL DEFAULT '';
