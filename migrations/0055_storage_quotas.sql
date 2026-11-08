-- Optional storage quotas: a limit per user and one for the whole vault, both
-- counted as the sum of the sizes of the stored file versions. NULL = no
-- limit (the default). Set with `teamcraft user quota` / `teamcraft vault quota`.
ALTER TABLE users ADD COLUMN storage_quota_bytes BIGINT
    CHECK (storage_quota_bytes IS NULL OR storage_quota_bytes >= 0);

CREATE TABLE vault_quota (
    id          BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    total_bytes BIGINT CHECK (total_bytes IS NULL OR total_bytes >= 0)
);
INSERT INTO vault_quota (id) VALUES (TRUE);

-- The per-user usage is summed over the user's versions.
CREATE INDEX file_versions_uploaded_by_idx ON file_versions (uploaded_by);
