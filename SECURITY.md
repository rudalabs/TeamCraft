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
* read visibility of [restricted project groups](https://rudalabs.github.io/TeamCraft/admin/permissions.html):
  any way for a non-member to read an item, file, preview, BOM line,
  change, request, notification, webhook payload or assistant answer of a
  restricted group, or to tell a hidden object from a missing one beyond the
  residual oracles listed there (unique numbers and codes, the shared number
  counter, where-used counts, response times);
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
* denial of service by plain request flooding;
* restricted data that a member already read: files copied to a desktop
  working folder stay there after access ends, audit entries from before
  migration 0024 are shown as written, backups and the command line contain
  everything, and the assistant sends what a member reads to the configured
  model provider.
* what a user's own AI agent does with their token: a local agent (the
  desktop app's *Agent* view, an MCP client set up by hand) sends what it
  reads to its own provider. `TEAMCRAFT_LOCAL_AGENTS=off` closes the MCP
  endpoint for every token and the desktop's *Agent* view then runs on the
  server-side assistant, so only that remains; the REST API stays reachable
  with a user's own token until scoped tokens exist (see
  [Local agents](https://rudalabs.github.io/TeamCraft/admin/configuration.html#local-agents)).
  A call to `POST /api/v1/mcp` that gets through while the setting is
  `off` is in scope.
