-- Review rounds (#152 step 5): a withdrawn, rejected or reopened change starts
-- a new round on the next submit; the decisions of the earlier rounds stay.
ALTER TABLE changes ADD COLUMN review_round INTEGER NOT NULL DEFAULT 1 CHECK (review_round >= 1);

CREATE TABLE change_review_rounds (
    change_id    UUID        NOT NULL REFERENCES changes (id) ON DELETE CASCADE,
    round        INTEGER     NOT NULL CHECK (round >= 1),
    reviewer_id  UUID        NOT NULL REFERENCES users (id),
    decision     TEXT        NOT NULL CHECK (decision IN ('pending', 'approved', 'rejected')),
    comment      TEXT        NOT NULL DEFAULT '',
    decided_at   TIMESTAMPTZ,
    closed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (change_id, round, reviewer_id)
);
CREATE INDEX change_review_rounds_reviewer_idx ON change_review_rounds (reviewer_id);
