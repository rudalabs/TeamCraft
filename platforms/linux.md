# TeamCraft on Linux

## Just run it: Docker

To use TeamCraft on a Linux server or desktop without building anything:

```sh
curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash
```

TeamCraft and PostgreSQL run as containers from `/opt/teamcraft`
(`--user`: `~/teamcraft`); running the script again upgrades. Options,
backups, TLS and uninstalling:
[Install with Docker](https://rudalabs.github.io/TeamCraft/admin/docker.html).
The rest of this page builds TeamCraft from source.

## Build from source

Local build and test from the command line. Tested on Arch/CachyOS, Ubuntu
24.04 and Fedora 41.

## 1. Prerequisites

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust (stable)
```

PostgreSQL 16 or newer – the simplest way is Docker:

```sh
sudo pacman -S docker docker-compose          # Arch / CachyOS
sudo apt install docker.io docker-compose-v2  # Ubuntu / Debian
sudo dnf install docker docker-compose        # Fedora
sudo systemctl enable --now docker
```

## 2. Run

```sh
git clone https://github.com/rudalabs/TeamCraft.git && cd TeamCraft
docker compose up -d db     # PostgreSQL on 127.0.0.1:5432, no password needed
cargo run --release         # first build takes a few minutes
```

Open <http://localhost:8080>, create the administrator on the login page,
then a project group (*Projects → New project group*) and your first item.

Without Docker, use a native PostgreSQL and a role with `CREATEDB`:

```sh
sudo -u postgres createuser --createdb teamcraft
sudo -u postgres createdb -O teamcraft teamcraft
export TEAMCRAFT_DATABASE_URL=postgres://teamcraft@localhost/teamcraft   # add :PASSWORD@ if your server asks
```

## 3. Test

```sh
cargo test                                   # unit + end-to-end tests (needs the database)
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
curl -s http://localhost:8080/readyz         # {"status":"ok",…}
```

A guided tour through every feature is in the documentation:
[Manual test tour](https://rudalabs.github.io/TeamCraft/guide/tour.html).

## 4. Desktop app (optional)

```sh
sudo pacman -S webkit2gtk-4.1 libayatana-appindicator librsvg        # Arch / CachyOS
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev  # Ubuntu
cd desktop && cargo run                     # connect to localhost:8080
```

Without building: the release assets contain an AppImage, `.deb`, `.rpm` and
a Flatpak bundle (`flatpak install --user TeamCraft_<version>_<arch>.flatpak`,
see [packaging/](../packaging/README.md#flatpak)).

## Troubleshooting

| Symptom | Fix |
|---|---|
| `cannot connect to PostgreSQL` after 20 s | `docker compose ps` – is `db` healthy? Port 5432 used by another PostgreSQL? |
| `password authentication failed` | your server needs a password: put it into `TEAMCRAFT_DATABASE_URL` |
| port 8080 in use | `TEAMCRAFT_BIND_ADDR=127.0.0.1:8081 cargo run` |
| start over | `docker compose down -v && rm -rf data/` (deletes all local data) |
