-- Need-to-know project groups, step 2 (issue #151): conditions on membership.
-- Without them (the defaults) membership alone decides, as before.
--   required_claims       [{"claim": "site", "values": ["main-office"]}, …]:
--                         every condition must hold, each with one of its
--                         values, in the claims of the user's last single
--                         sign-on (lower case)
--   allowed_networks      client address ranges (CIDR); empty = any address
--   max_signin_age_hours  the group is visible only so many hours after
--                         signing in (web session start / last sign-in)
ALTER TABLE project_policies
    ADD COLUMN required_claims JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(required_claims) = 'array'),
    ADD COLUMN allowed_networks TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN max_signin_age_hours INTEGER
        CHECK (max_signin_age_hours IS NULL OR max_signin_age_hours BETWEEN 1 AND 8760);

-- The claims of the user's last single sign-on that a group's conditions
-- name (only those are kept), as claim → list of lower-case values.
ALTER TABLE users
    ADD COLUMN idp_claims JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN idp_claims_at TIMESTAMPTZ;
