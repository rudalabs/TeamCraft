-- Sign-off of review decisions (#152 step 4).
--
-- With TEAMCRAFT_APPROVAL_REAUTH a decision in the browser needs a fresh
-- sign-in: the account password with the decision, or signing in again with
-- single sign-on shortly before (`sessions.reauth_at`, set by an OIDC login
-- with prompt=login that names the session in `oidc_states.reauth_session`).
ALTER TABLE sessions ADD COLUMN reauth_at TIMESTAMPTZ;
ALTER TABLE oidc_states ADD COLUMN reauth_session TEXT;

-- How each decision was signed off, and what it means ("approved as design
-- owner"); kept with the earlier review rounds as well.
ALTER TABLE change_reviews
    ADD COLUMN sign_off_method TEXT
        CHECK (sign_off_method IN ('session', 'password', 'sso', 'token')),
    ADD COLUMN sign_off_meaning TEXT NOT NULL DEFAULT '';
ALTER TABLE change_review_rounds
    ADD COLUMN sign_off_method TEXT
        CHECK (sign_off_method IN ('session', 'password', 'sso', 'token')),
    ADD COLUMN sign_off_meaning TEXT NOT NULL DEFAULT '';
