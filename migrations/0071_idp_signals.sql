-- Offboarding driven by the identity provider (issue #173, step 9):
-- when the provider last logged the user out over the back channel
-- (OpenID Connect Back-Channel Logout). Shown in the report of single
-- sign-on users who stopped signing in; nothing else depends on it.
ALTER TABLE users ADD COLUMN idp_logout_at TIMESTAMPTZ;
