# Deprovisioning through the identity provider (SCIM 2.0)

Question: how TeamCraft can notice that someone was removed at the identity
provider (IdP) and start their offboarding, without an administrator having to
remember it. Issue #173, step 9 (d). Date: 2026-11-12.

## What exists today (step 9 a–c)

| Signal                                                     | What TeamCraft does                                                                                                               |
|------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|
| The user stops signing in                                  | *SSO users without sign-in* on the administrators' *Users* page: active SSO users without sign-in or token use for 90 days who still hold memberships, tokens or check-outs |
| The user's tokens                                          | `TEAMCRAFT_OIDC_TOKEN_IDLE_DAYS`: new API and desktop tokens of SSO users expire after that many days without use                  |
| The IdP ends the session or removes the user               | OpenID Connect Back-Channel Logout 1.0 at `/auth/oidc/backchannel-logout`: the signed logout token ends every browser session of that user, audited `user.idp_logout`, time shown in the report |
| The IdP removes the user from a group                      | the role and the IdP-synced memberships follow at the next SSO sign-in – a leaver never signs in again                             |

None of these **deactivates** anybody or hands their work over. The
administrator still runs *Offboard and deactivate* (step 4). SCIM is the
standard way for an IdP to tell an application "this account is disabled".

## How SCIM deprovisioning works

SCIM 2.0 ([RFC 7643](https://www.rfc-editor.org/rfc/rfc7643) schema,
[RFC 7644](https://www.rfc-editor.org/rfc/rfc7644) protocol): the application
serves `/scim/v2/Users` (and optionally `/Groups`), the IdP calls it with a
bearer token. The IdP creates users (`POST`), looks them up
(`GET ?filter=userName eq "…"`), changes them (`PATCH`/`PUT`) and in theory
deletes them (`DELETE`).

What the common providers send when someone leaves:

| Provider              | Leaver                                                                                                                                         | Sources |
|-----------------------|------------------------------------------------------------------------------------------------------------------------------------------------|---------|
| Microsoft Entra ID    | `PATCH` with `active: false` when the user is unassigned, blocked or soft-deleted; `DELETE` only after the hard delete (30 days later by default). Non-gallery apps may send the string `"False"` instead of a boolean | [Entra SCIM requests](https://ssojet.com/blog/entra-id-scim-requests-what-entra-sends), [Microsoft Q&A](https://learn.microsoft.com/en-au/answers/questions/1837583/deprovisioning-not-working-in-azure-ad) |
| Okta                  | never `DELETE` on users: deactivation is `PATCH` (or `PUT` for older custom apps) with `active: false`, reversible                               | [Okta support](https://support.okta.com/help/s/question/0D54z0000AIb5FTCQZ/question-about-scim-user-deactivation), [WorkOS](https://workos.com/blog/okta-entra-id-scim-requests) |
| authentik             | a SCIM provider (outbound, as a back-channel provider next to the OIDC provider), static token or OAuth (enterprise); `PATCH` where supported | [authentik docs](https://docs.goauthentik.io/add-secure-apps/providers/scim/) |
| Keycloak              | no outbound SCIM (only an experimental inbound SCIM API, and third-party add-ons)                                                               | [Keycloak survey feedback](https://www.keycloak.org/2026/02/scim-support-survey-feedback) |
| University SSO (SAML / Shibboleth federations) | no SCIM to applications at all; accounts end at the IdP and that is all an application learns                                         | – |

So the deprovisioning signal that matters is `active: false` (boolean or
string), through `PATCH` or `PUT`; `DELETE` may never arrive. Every guide
recommends a soft delete: keep the record, revoke sessions and tokens.

## What implementing it would take

* `/scim/v2/Users` with `GET` (filter `userName eq` and `externalId eq` only),
  `POST`, `PATCH` (`replace` of `active`, with and without `path`), `PUT`,
  `DELETE`; `ServiceProviderConfig`, `ResourceTypes` and `Schemas` discovery;
  the SCIM error format. No crate needed (serde); the dialects of each IdP
  (Entra's string booleans, Okta's `PUT`) need their own tests.
* A separate bearer credential per IdP (`TEAMCRAFT_SCIM_TOKEN`, outside the
  personal access tokens), rate limited.
* Matching SCIM users to TeamCraft users (`oidc_subject` or `userName`), and
  what `POST` of a user who never signed in means (create an inactive account
  or refuse).
* `active: false` → *Offboard and deactivate* (step 4) with a configured
  default: release check-outs or keep them, revoke tokens; hand-over needs a
  successor and stays manual. `DELETE` → the same (TeamCraft never deletes
  users). `active: true` → reactivation without old tokens.

## Decision

Not now. The target audience – student teams and early-stage startups – mostly
signs in through a university SAML/OIDC proxy or a small Keycloak/authentik
instance; of these only authentik sends SCIM, and universities do not offer it
to team applications. Steps 9 (a)–(c) cover what such an IdP can actually
signal: the idle report finds the leavers, idle tokens expire on their own,
and back-channel logout ends the sessions at once.

Revisit when a team on Entra ID, Okta or authentik asks for automatic
deprovisioning. The mapping above (`active: false` → step 4 with a configured
default, soft delete only, string booleans accepted) is then the plan.
