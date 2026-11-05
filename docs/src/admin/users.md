# Users, roles and single sign-on

## Local accounts

Administrators create accounts under *Users* or on the command line
(`teamcraft user create --username erika --role editor --project PUMP`).
Passwords are hashed with Argon2id; sessions and API tokens are stored as
SHA-256 hashes only.

## Roles

| Role       | Stored as | May                                                                                    |
|------------|-----------|----------------------------------------------------------------------------------------|
| Viewer     | `viewer`  | read internal project groups and the restricted groups they are a member of           |
| CAD editor | `editor`  | read like a viewer; change items of their project groups, create project groups, review |
| Admin      | `admin`   | everything, including restricted groups, plus users, all groups, lock overrides        |

A project group can be **restricted** to its members: everybody else –
viewers and CAD editors who are not members – gets "not found" for the
group and everything in it. Viewers who are members read it. See
[Read permissions and restricted groups](permissions.md) for the full
rules, what outsiders see of links into a restricted group, the delay of
memberships synchronised from the identity provider and what stays
visible on a shared instance.

## Single sign-on (OpenID Connect)

Any OIDC provider with the authorization-code flow works (Authentik,
Keycloak, Entra ID, Google, …). Register the redirect URI
`<TEAMCRAFT_PUBLIC_URL>/auth/oidc/callback` and set:

| Variable                                      | Default                | Meaning                            |
|-----------------------------------------------|------------------------|------------------------------------|
| `TEAMCRAFT_OIDC_ISSUER_URL`                   | –                      | issuer; enables SSO                |
| `TEAMCRAFT_OIDC_CLIENT_ID` / `_CLIENT_SECRET` | –                      | client credentials                 |
| `TEAMCRAFT_OIDC_DISPLAY_NAME`                 | `Single Sign-On`       | label of the login button          |
| `TEAMCRAFT_OIDC_SCOPES`                       | `openid profile email` | requested scopes                   |
| `TEAMCRAFT_OIDC_GROUPS_CLAIM`                 | `groups`               | claim with the user's groups       |
| `TEAMCRAFT_OIDC_USERNAME_CLAIM`               | `preferred_username`   | username of new accounts           |
| `TEAMCRAFT_OIDC_ADMIN_GROUP`                  | `teamcraft-admins`     | members become admins              |
| `TEAMCRAFT_OIDC_EDITOR_GROUP`                 | `teamcraft-editors`    | members become CAD editors         |
| `TEAMCRAFT_OIDC_VIEWER_GROUP`                 | –                      | if set, required to sign in at all |

Accounts are created at the first sign-in; the role follows the groups at
every sign-in. A project group bound to an IdP group (*Projects → group →
Edit*, administrators) gets its members synchronised at every sign-in;
manually added members stay.
