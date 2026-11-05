# Getting started

## Install on Linux with Docker

```sh
curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash
```

TeamCraft and PostgreSQL run as containers; the script offers to create the
administrator at the end. See [Install with Docker](../admin/docker.md).

## Build and run

You need Rust (stable, via [rustup](https://rustup.rs)) and PostgreSQL 16 or newer.
The repository's `docker-compose.yml` starts a development database that
needs no password:

```sh
git clone https://github.com/rudalabs/TeamCraft.git && cd TeamCraft
docker compose up -d db
cargo run --release
```

TeamCraft applies its database migrations on start and listens on
<http://localhost:8080>. The platform guides cover native PostgreSQL
installations and the desktop app:
[Linux](https://github.com/rudalabs/TeamCraft/blob/main/platforms/linux.md) ·
[macOS](https://github.com/rudalabs/TeamCraft/blob/main/platforms/macos.md) ·
[Windows](https://github.com/rudalabs/TeamCraft/blob/main/platforms/windows.md).

## First start

1. Open <http://localhost:8080>. A server without accounts that listens on
   localhost offers **Create the administrator** – pick a username and a
   password (10+ characters). The form disappears afterwards.
2. **Projects → New project group**: every item belongs to one. The creator
   manages the group and adds the team members.
3. **Admin → Users** (admins): create accounts for your team, or connect single
   sign-on (see [Users, roles and SSO](../admin/users.md)).
4. **Items → New item**: choose the CAD file (s) first – the model, its
   drawing and a STEP export for the 3D preview. Name and type are taken from
   the first file (an assembly for `.iam`, `.asm`, `.sldasm`, …). The item is
   created together with its files; its first revision *A* is *in work* and
   checked out to you.
5. **Check in**, then release it with a change order.

**Appearance:** TeamCraft follows the light or dark appearance of your
operating system. The menu under your name (top right) and *Account →
Appearance* switch between *System*, *Light* and *Dark*; choose *Dark* to keep
the dark look of earlier versions. The distribution skins (Arch, CachyOS,
EndeavourOS, KDE, Kubuntu, openSUSE, Ubuntu) are dark. The choice is personal
and also styles the login page in that browser.

**Keyboard:** `/` or `Ctrl+K` (`⌘K`) jumps to the search field. The single-key
`/` can be switched off under *Account → Appearance → Keyboard* (per browser).

Keyboard: `/` jumps to the search field (on the vault explorer: to its
filter); in menus, `Esc` closes and the arrow keys move.

On a server that is reachable from other machines, create the first
administrator from the command line instead:

```sh
teamcraft user create --username admin --role admin      # prompts for the password
```

## Command line

| Command                                                                            | Purpose                                      |
|------------------------------------------------------------------------------------|----------------------------------------------|
| `teamcraft` / `teamcraft serve`                                                    | run the web server (applies migrations)      |
| `teamcraft migrate`                                                                | apply migrations and exit                    |
| `teamcraft user create --username U --role viewer\|editor\|admin [--project CODE]` | local account                                |
| `teamcraft user set-password --username U`                                         | reset a password                             |
| `teamcraft user list`                                                              | all accounts                                 |
| `teamcraft vault check [--verify-hashes]`                                          | every stored file present (and intact)?      |
| `teamcraft rescan-references [--format id]`                                        | read stored CAD file references again        |
| `teamcraft vault signatures [--convert]`                                           | shape signatures for the similar part search |
| `teamcraft healthcheck`                                                            | exit 0 when the server answers on `/healthz` |
