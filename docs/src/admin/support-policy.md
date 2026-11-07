# Support policy

This page says which versions of TeamCraft receive fixes, what a version number
promises, and where to report a problem. It applies to the server, the web UI,
the REST API, the MCP server, the command line and the desktop app.

**Status.** TeamCraft has no stable release yet. It is a work in progress and
comes without warranty. Until the first stable release, the rules below for
`0.x` apply, and security fixes go into `main` only, as described in
[SECURITY.md](https://github.com/rudalabs/TeamCraft/blob/main/SECURITY.md#supported-versions).

## Version numbers

TeamCraft uses [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`.

- **Before 1.0** (`0.x`), any release may contain breaking changes, including
  changes to the configuration, the REST API, the database schema and the
  command line. Each release note says what the upgrade needs.
- **From 1.0 on**:
  - a **patch** release (`1.0.1`) fixes bugs and security problems and does
    not change behaviour that users rely on;
  - a **minor** release (`1.1.0`) adds features and stays compatible: an
    upgrade within the major version does not need manual changes beyond the
    release notes;
  - a **major** release (`2.0.0`) may break compatibility. Its release notes
    list every breaking change and the steps for the upgrade.
- A **pre-release** (`1.0.0-rc.1`) is for testing. It gets no fixes beyond the
  next candidate or the final release.

Database migrations are part of the upgrade. A release never changes an
existing migration; a new migration is added instead. Back up the database and the vault before any upgrade, as described in
[Operations](operations.md#backups).

## Which versions receive fixes

| Version                          | Receives                                            |
|----------------------------------|-----------------------------------------------------|
| Latest minor (`1.x`)             | all fixes: bugs, security problems                  |
| Previous minor                   | security fixes, for six months after the next minor |
| Older minors                     | none; upgrade to a supported minor                  |
| `0.x` (before 1.0)               | only `main`                                         |

From 1.0 on, when a new minor version is released, the previous minor version
keeps receiving **security fixes for six months** after that release. After
the six months, it is no longer supported. A patch release is made for the
latest minor version only; a fix for an older minor is made only when it is a
security fix.

Fixes are published as new releases, not as edits to old ones. The release
notes and the security advisory name the versions that are affected.

## Reporting a bug

- **Bugs and feature requests** that are not security problems: open an issue
  at [github.com/rudalabs/TeamCraft/issues](https://github.com/rudalabs/TeamCraft/issues).
  Include the version (the image tag or the release, or the commit from
  `git rev-parse --short HEAD`), the platform, the steps to reproduce, what you
  expected and what happened. Remove passwords, tokens and other secrets from
  logs before posting them.
- **Security problems**: do not open a public issue. Report them privately as
  described in [SECURITY.md](https://github.com/rudalabs/TeamCraft/blob/main/SECURITY.md#reporting-a-vulnerability)
  through the GitHub security advisory form. The policy there sets out what
  is in scope and how a fix and its disclosure date are agreed.
- **Questions about installation or operation**: check this documentation first,
  the [configuration reference](configuration.md) and the
  [operations guide](operations.md). Then open an issue.

Before you report a bug, check the release notes of the version you run. The
problem may be fixed in a newer release of your supported version.

## What is not promised

- Response times. TeamCraft is maintained by volunteers.
- Support for a version that is no longer in the table above.
- Fixes for deployments that ignore the documentation, for example a server that
  is exposed without TLS.

## Desktop app and server versions

The desktop app checks the server's version on the sign-in screen and when its
main window opens, and shows a warning once per server when the two do not fit:

| App | Server that works |
|-----|-------------------|
| `0.x.y` | the same `0.x` (any patch version) |
| `1.a.y` and later | the same major version and a minor version `a` or newer |

Patch versions never matter. The message does not block the sign-in; it tells
the user whether to upgrade the server or to install the matching app.
