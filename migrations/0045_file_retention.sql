-- Retention of old file versions: 'all' keeps every version (the behaviour so
-- far), 'latest' lets the housekeeping remove versions that a newer version
-- replaced (after a grace period, never for a released revision). Meant for
-- large result files that change with every run.
ALTER TABLE files ADD COLUMN retention TEXT NOT NULL DEFAULT 'all'
    CHECK (retention IN ('all', 'latest'));
