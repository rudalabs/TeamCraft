-- Sign-in of the desktop app through the browser (loopback redirect, RFC 8252).
-- The browser confirms the sign-in and is redirected to
-- http://127.0.0.1:<port>/callback?code=…; the app exchanges the one-time code
-- together with the PKCE verifier for a personal access token.
CREATE TABLE desktop_auth_codes (
    code_hash   TEXT        PRIMARY KEY,            -- SHA-256 of the one-time code
    user_id     UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    challenge   TEXT        NOT NULL,               -- base64url(SHA-256(verifier))
    device      TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX desktop_auth_codes_expires_idx ON desktop_auth_codes (expires_at);
