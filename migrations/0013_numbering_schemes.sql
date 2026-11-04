-- Configurable numbering schemes per project group, e.g.
-- '{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}'. NULL = the built-in '{TYPE}-{SEQ:6}'.
-- The application validates the scheme (domain::NumberScheme).
ALTER TABLE projects ADD COLUMN number_scheme TEXT
    CHECK (number_scheme IS NULL OR length(number_scheme) BETWEEN 1 AND 100);

-- Item counters are now named after the expanded scheme without the sequence
-- ('num:PRT-{SEQ}' for the built-in scheme). Carry the old per-type counters
-- over so generated numbers continue where they stopped.
INSERT INTO counters (name, value)
SELECT 'num:' || CASE name
                     WHEN 'item:part' THEN 'PRT'
                     WHEN 'item:assembly' THEN 'ASM'
                     WHEN 'item:document' THEN 'DOC'
                     WHEN 'item:software' THEN 'SW'
                 END || '-{SEQ}',
       value
FROM counters
WHERE name IN ('item:part', 'item:assembly', 'item:document', 'item:software')
ON CONFLICT (name) DO UPDATE SET value = GREATEST(counters.value, EXCLUDED.value);

DELETE FROM counters
WHERE name IN ('item:part', 'item:assembly', 'item:document', 'item:software');
