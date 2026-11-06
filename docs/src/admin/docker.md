# Install with Docker

The quickest way to run TeamCraft on a Linux server: one script sets up the
TeamCraft container and a PostgreSQL container with Docker Compose. Nothing
is built on your machine – the image `ghcr.io/rudalabs/teamcraft` comes from
GitHub's container registry for `linux/amd64` and `linux/arm64`.

> TeamCraft has no release yet (version 0.x). Until there is one, the
> installer uses the development image `edge`, built from the `main` branch.
> Keep backups (see below).

## Install

You need Docker Engine with the Compose v2 plugin (`docker compose version`).

```sh
curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash
```

The script asks for `sudo` where it needs it and then:

1. checks Docker and Compose (it never installs Docker on its own; with
   `--install-docker` it offers to install the distribution's packages);
2. picks the version – the newest release, or `edge` while there is none;
3. writes the installation to `/opt/teamcraft`:

   | File                  | Contents                                                       |
   |-----------------------|----------------------------------------------------------------|
   | `docker-compose.yml`  | the two services, replaced on every upgrade                    |
   | `.env`                | `TEAMCRAFT_VERSION` and `TEAMCRAFT_BIND` for Compose            |
   | `teamcraft.env`       | the server configuration – created once, then yours (mode 600) |
   | `secrets/db_password` | the generated database password – never printed                |

4. pulls the images, starts them and waits until `/readyz` answers;
5. offers to create the first administrator on the command line.

To read the script first, download it and run it from the file – the same
options work in both cases (`… | bash -s -- --user`):

```sh
curl -fsSLO https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh
less install.sh
bash install.sh --dry-run      # prints every step, changes nothing
bash install.sh
```

From a checkout, `./deploy/install.sh` uses the compose file of the checkout.

| Option                   | Effect                                                                         |
|--------------------------|--------------------------------------------------------------------------------|
| `--version 0.2.0`/`edge` | image version (default: the newest release; `edge` installations stay `edge`) |
| `--user`                 | install into `~/teamcraft` without `sudo` (you must be in the `docker` group)  |
| `--dir DIR`              | install into `DIR`                                                             |
| `--bind ADDR`            | published address, default `127.0.0.1:8080`; `0.0.0.0:8080` serves the network |
| `--public-url URL`       | the address users open, written to `teamcraft.env`                             |
| `--upgrade`              | upgrade (the default when an installation is found)                            |
| `--uninstall [--purge]`  | remove the containers; `--purge` also deletes all data – asks first            |
| `--install-docker`       | install Docker and Compose with `apt`, `dnf`, `pacman` or `zypper` – asks first |
| `--yes`                  | unattended: answer yes to every question                                       |
| `--dry-run`              | show what would be done                                                        |

## The first administrator

The installer offers to create it right away and asks for the password.
Later, or after an unattended installation:

```sh
cd /opt/teamcraft
sudo docker compose exec teamcraft teamcraft user create --username admin --role admin
```

The *Create administrator* form on the login page of a local installation
(`cargo run`) is not offered here: inside the container the server listens on
all interfaces, so it cannot tell whether the page is opened from the same
computer. Alternatively set `TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD` in
`teamcraft.env` for the first start (used only while there are no accounts),
or sign in with [single sign-on](users.md).

Every other `teamcraft` command works the same way:
`sudo docker compose exec teamcraft teamcraft user list`, `… vault check`.

## Configuration

Edit `teamcraft.env` (every variable: [Configuration](configuration.md)) and
apply it with `sudo docker compose up -d`. The database connection, the
listen address inside the container and the vault directory are set in
`docker-compose.yml` and take precedence.

The published port is `TEAMCRAFT_BIND` in `.env`. The default
`127.0.0.1:8080` reaches only the server itself – put a reverse proxy in
front (below), or publish the port to the network and tell TeamCraft its
address, which every form is checked against:

```sh
sudo bash install.sh --bind 0.0.0.0:8080 --public-url http://plm.example.org:8080
```

## Reverse proxy and TLS

For anything beyond a test, serve TeamCraft over HTTPS through a reverse proxy
on the same host, keep the default `127.0.0.1:8080` binding, and set
`TEAMCRAFT_PUBLIC_URL=https://plm.example.org` (that also turns on `Secure`
cookies). With Caddy, which gets the certificate itself:

```text
plm.example.org {
    reverse_proxy 127.0.0.1:8080
}
```

With nginx, allow large uploads (`client_max_body_size 512m;`, matching
`TEAMCRAFT_MAX_UPLOAD_MB`) and pass `Host` and `X-Forwarded-Proto`.

The cross-site check of web forms accepts the host in `X-Forwarded-Host` as
this instance, so the proxy must set that header itself or drop what the
client sent – never pass a client's value through (Caddy sets it; with
nginx, `proxy_set_header X-Forwarded-Host $host;`). A form post that carries
the session cookie but neither `Origin` nor `Referer` is refused; current
browsers always send `Origin`, so this only stops forged requests and
scripts that reuse a browser cookie.

## Upgrade

Run the installer again – it downloads the compose file of the new version,
pulls the image and restarts; configuration, password and data stay:

```sh
curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash
# or a specific version:  … | bash -s -- --version 0.3.0
```

Database migrations are applied on start. While TeamCraft is 0.x, read the
release notes before an upgrade and take a backup first.

## Backup and restore

The data lives in two Docker volumes: `teamcraft-server_db` (PostgreSQL) and
`teamcraft-server_vault` (every uploaded file; `vault/previews/` is a cache).
Back up both together, plus the installation directory (it holds the
database password). The image has no PostgreSQL client tools, so
`teamcraft backup` ([Operations](operations.md#backups)) does not run inside
the container; dump through the database container instead (`sh -c`: the
dump is written as root, because `/opt/teamcraft` belongs to root):

```sh
cd /opt/teamcraft
sudo sh -c 'docker compose exec -T db pg_dump -U teamcraft -Fc teamcraft > teamcraft-db.dump'
sudo docker run --rm -v teamcraft-server_vault:/vault:ro -v "$PWD":/backup alpine \
  tar -czf /backup/teamcraft-vault.tar.gz -C /vault --exclude=./vault/previews .
```

Restore into a fresh installation (same or newer version):

```sh
cd /opt/teamcraft
sudo docker compose stop teamcraft
sudo docker compose exec -T db pg_restore -U teamcraft -d teamcraft --clean --if-exists --single-transaction < teamcraft-db.dump
sudo docker run --rm -v teamcraft-server_vault:/vault -v "$PWD":/backup alpine \
  tar -xzf /backup/teamcraft-vault.tar.gz -C /vault
sudo docker compose start teamcraft
sudo docker compose exec teamcraft teamcraft vault check
```

The vault is content-addressed and append-only, so incremental file backups
of the volume directory (`docker volume inspect teamcraft-server_vault`) work
well too.

## Image tags

| Tag              | Built from                                                         |
|------------------|--------------------------------------------------------------------|
| `edge`           | every push to `main`                                               |
| `0.2.0`, `0.2`   | the release tag `v0.2.0`                                           |
| `latest`         | the newest release without a suffix such as `-rc.1` – also 0.x     |

The image is built from the repository's `Dockerfile` by GitHub Actions only
(`.github/workflows/release.yml`): a small distroless runtime (glibc, CA
certificates, time zones, no shell) that runs as an unprivileged user,
with a `HEALTHCHECK` through `teamcraft healthcheck`. The compose file also
makes its root file system read-only and drops all capabilities.

## Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash -s -- --uninstall
```

This removes the containers and keeps the volumes and `/opt/teamcraft`, so
running the installer again brings everything back. `--uninstall --purge`
deletes the volumes and the directory as well, after a confirmation.

## Troubleshooting

| Symptom                                  | Check                                                                          |
|------------------------------------------|--------------------------------------------------------------------------------|
| the installer waits and then stops       | its output shows `docker compose ps` and the last log lines                    |
| `Forbidden` / origin error on every form | `TEAMCRAFT_PUBLIC_URL` must be exactly the address in the browser               |
| `password authentication failed`         | `secrets/db_password` was changed after the first start – the database keeps the first one |
| `the database volume … already exists`   | the installation directory was deleted without `--purge` (or another directory is used): restore its `secrets/db_password`; the Compose project name `teamcraft-server` allows one installation per host |
| port 8080 is taken                       | `--bind 127.0.0.1:8081`                                                         |
| a STEP file has no or only a partial 3D preview | run the converter by hand: `docker compose exec teamcraft teamcraft preview-mesh --format step --input <file> --output /tmp/x.tcm` prints the triangle count, `partial preview: N of M faces` or the reason; see [STEP in the 3D preview](../reference/formats.md#step-in-the-3d-preview). Failures are cached per file – after an update that bumps the preview cache version they are retried |
