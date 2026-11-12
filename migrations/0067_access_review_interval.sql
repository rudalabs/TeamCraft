-- Need-to-know project groups, step 5 (issue #151): the access review of a
-- restricted group is due every `review_interval_days` days (NULL = no
-- schedule, as before). A review is due when none was confirmed within the
-- interval; the managers are reminded once when it becomes due and again
-- after each further interval without a review (`review_reminded_at`).
ALTER TABLE project_policies
    ADD COLUMN review_interval_days INTEGER
        CHECK (review_interval_days IS NULL OR review_interval_days BETWEEN 1 AND 3650),
    ADD COLUMN review_reminded_at TIMESTAMPTZ;
