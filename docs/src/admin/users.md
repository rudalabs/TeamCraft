# Users, roles and single sign-on

## Local accounts

Administrators create accounts under *Users* or on the command line
(`teamcraft user create --username erika --role editor --project PUMP`).
Passwords are hashed with Argon2id; sessions and API tokens are stored as
SHA-256 hashes only.

## Roles

| Role       | Stored as | May                                                                                     |
|------------|-----------|-----------------------------------------------------------------------------------------|
| Viewer     | `viewer`  | read internal project groups and the restricted groups they are a member of             |
| CAD editor | `editor`  | read like a viewer; change items of their project groups, create project groups, review |
| Admin      | `admin`   | everything, including restricted groups, plus users, all groups, lock overrides         |

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
manually added members stay. A synchronised membership stays one when its
holder is made a manager (*Demote* lowers it again), and it cannot be
removed in TeamCraft while the user is active (it would return at the next
sign-in): remove the user from the IdP group, unlink the group or deactivate
the user instead. A deactivated user's synchronised membership can be
removed (see [When someone leaves](#when-someone-leaves)).

## Item owners and handing over a leaving user's work

Every item has an **owner**, the person responsible for it. It starts as the
creator and is shown in the master data of the item page. The managers of
the item's project group and administrators change it (*Change* next to the
owner); the new owner must be an active CAD editor or administrator who may
read the item. Every change is audited (`item.owner`).

When somebody leaves, an administrator opens the user and uses **Hand over
work**. In one transaction TeamCraft moves to the chosen person:

| What                                 | Moved when                                      |
|--------------------------------------|-------------------------------------------------|
| item ownerships                      | always                                          |
| change orders the user authored      | draft or rejected                               |
| change requests assigned to the user | not closed (new, in triage, accepted, deferred) |
| pending reviews of the user          | the change order is a draft or in review        |

A message after the run gives the counts and the first objects that stayed with
the old user and why (numbers of groups the administrator may not read are
shown as "restricted item"): the receiver cannot read the object (restricted
project group), already reviews the change order, or may not review it under
the four-eyes principle (author, preparer or someone who changed the
revisions). Every moved object gets its own audit entry (`item.owner`,
`change.handover`, `change_request.handover`, `change.review_handover`) plus
one `user.handover` entry with the counts. Decisions already made, released
changes and closed requests keep the original names. One run moves at most
1000 objects of each kind; the message says when to run it again.

## When someone leaves

Teams with a high turnover (student teams, early-stage startups) lose
members every season. Deactivating a user ends their sessions and disables
their API and desktop tokens; their check-outs, change orders, reviews and
memberships stay. Work through these steps, in this order:

1. **Keep at least two active administrators.** TeamCraft refuses to
   deactivate or demote the last one. If nobody with the admin role is left,
   whoever has a shell on the server creates one:
   `teamcraft user create --username rescue --role admin` (break glass).
2. **Before they go**, the leaver uses *Check in all* on the dashboard and
   checks in the local changes of their desktop working folder. Work that
   was never checked in exists only on their computer.
3. **Hand over their work** (administrators): *Users → user → Hand over
   work* moves item ownerships, draft and rejected change orders, open
   change requests and pending reviews (see above).
4. **Release the remaining check-outs.** Administrators use *Users → user →
   Check-outs → Release all*. The managers of a project group use *Release N
   check-outs* in the group's member list – for members who are deactivated
   and, under *Former members still holding check-outs*, for people who are
   no longer members. Each release is audited (`revision.checkout_forced_release`); one refused release does not stop
   the others. Revisions of a need-to-know group that hides itself from
   administrators (`admin_read = none`) are left out for administrators: a
   manager of that group releases them.
5. **Pending reviews:** a change order cannot be submitted while one of its
   reviewers is deactivated – remove them and add someone else. For a change
   already in review the author (or an administrator) replaces the pending
   reviewer (*Substitute for*), which keeps the other decisions.
6. **Change orders of a deactivated author:** besides administrators, a
   manager of *every* project group the change touches may withdraw, reopen
   or cancel it, or **take it over** (*Take over* on the change page; audited
   as `change.handover`). Whoever takes it over is its author and can no
   longer review it (four-eyes principle).
7. **Group managers:** *Demote* in the member list lowers a manager to a
   plain member, also for memberships synchronised from the identity
   provider. At least one *active* manager must remain (only
   administrators may demote or remove the last one); deactivated managers
   do not count.
8. **Revoke their tokens** on the user page.
9. **Deactivate the user.** For SSO users both are needed: remove them at
   the identity provider **and** deactivate them in TeamCraft. A synchronised
   membership of a deactivated user can then be removed by the group's
   managers (SSO refuses deactivated users, so it cannot come back); an
   active user's synchronised membership still can only end at the identity
   provider.
