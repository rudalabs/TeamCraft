# Security policy

## Supported versions

TeamCraft has **no release yet**. Security fixes go into the `main` branch
only; there are no maintained release branches.

| Version                         | Supported             |
|---------------------------------|-----------------------|
| `main` (0.1.x, pre-release)     | yes – fixes land here |
| older commits, forks, snapshots | no                    |

TeamCraft has **not had an independent security review**. Until there is a
stable release:

* run it only on trusted networks, or behind a reverse proxy with TLS;
* keep it up to date with `main`;
* keep backups of the database and the vault, as described in
  [Operations](docs/src/admin/operations.md#backups).

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report them
privately on GitHub:
**[Report a vulnerability](https://github.com/rudalabs/TeamCraft/security/advisories/new)**
(*Security* tab → *Advisories* → *Report a vulnerability*).

Please include:

* the affected part: web UI, REST API, desktop app, file parsers or packaging;
* the commit you tested (`git rev-parse --short HEAD`);
* steps to reproduce or a proof of concept;
* the impact you expect.

Do not include real secrets or other people's data.

TeamCraft is a volunteer project, so there is no guaranteed response time.
Every report is answered as soon as possible. We agree on a fix and a
disclosure date with you, and credit you in the advisory unless you prefer
otherwise.

## Scope

In scope:

* authentication: passwords, sessions, API tokens, OpenID Connect;
* permission checks: roles, project groups, check-out locks, released data;
* CSRF, XSS and the Content Security Policy;
* uploads and the parsers for CAD, mesh, point-cloud and BOM table files.
  This includes files that crash or hang the server, or that get around the
  time limit of the preview process;
* path traversal in the vault or in the desktop workspace;
* event webhooks that reach a target the
  [target guard](https://rudalabs.github.io/TeamCraft/admin/webhooks.html#where-webhooks-may-send-to)
  should refuse, or signatures that can be forged without the secret.

Out of scope:

* attacks that need administrator rights, or access to the server's file
  system or database;
* deployments that ignore the documentation, for example ones that expose the
  server without TLS;
* denial of service by plain request flooding.
