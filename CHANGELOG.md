# Changelog

All notable changes to TeamCraft are recorded in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the version numbers follow [Semantic Versioning](https://semver.org/).
While the version is `0.x`, any release may contain breaking changes. The
policy for supported versions is in the
[support policy](https://rudalabs.github.io/TeamCraft/admin/support-policy.html).

TeamCraft has no stable release yet. The project status is work in progress:
see the notices in the README.

## [Unreleased]

Changes on `main` since the `v0.2.0` tag.

### Added

- The desktop app tells the user when its version does not fit the server
  (version handshake, see the support policy).
- Administrators can replace the wide logo (top left) and the square logo
  (bottom left, tab icon) under *Admin → Branding*; the desktop app shows them
  too. See [Branding](https://rudalabs.github.io/TeamCraft/admin/branding.html).
- Sign-in, failed sign-in and sign-out are recorded in the audit log, with how
  the user signed in (password, SSO or first-start setup).
- A sign-in throttle slows down repeated failed sign-ins.
- Desktop app: an update notice. At most once a day the app asks GitHub for
  the latest release and shows a link to it in the status bar; it never
  downloads or installs anything. *Settings → Updates* switches it off.

### Changed

- Accessibility: an automated check runs on every rendered page; keyboard
  flows were completed and the Escape key closes panels.
- The MCP tools are split by area (`src/mcp/tools.rs`); no behaviour change.
- The service functions for requests are split into smaller modules; no
  behaviour change.
- The invariants of the remaining mutating service functions are documented.

### Fixed

- Desktop app: the assistant answer hung when a whole line looked like an item
  number to the folder import (two variables of the same name), and item
  numbers in answers were never links. Simulation numbers (`SIM-…`) are links
  now as well, in the web UI too.

- The reference scan records the names of STEP exports as a second name, and
  no longer reports noise for JT files.
- The duplicate-files report counts the files of a hidden revision correctly.
- Two administrators can no longer remove each other at the same time.
- The command line commands write their log output to standard error.

### Security

- Web requests that change data are refused when they carry the session cookie
  but neither an `Origin` nor a `Referer` header (cross-site request forgery).
- `Strict-Transport-Security` is sent when the public URL uses `https://`.
- Changing one's own password ends the other sessions of the user.
- The file readers for untrusted uploads (CAD, mesh, point-cloud and BOM
  table files) no longer panic on malformed input.
- The child processes that convert files start with a cleared environment.
- Desktop app: the sign-in token is kept in the OS keychain (Windows
  Credential Manager, macOS Keychain, Secret Service on Linux); the token file
  of earlier versions moves there at the next start. Without a keychain the
  app keeps the private file and shows a warning. Server addresses with plain
  `http://` to another computer get a warning.

[Unreleased]: https://github.com/rudalabs/TeamCraft/compare/v0.2.0...HEAD
