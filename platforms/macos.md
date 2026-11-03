# TeamCraft on macOS

Local build and test from the command line (macOS 12 or newer, Apple
silicon or Intel).

## 1. Prerequisites

```sh
xcode-select --install                                           # compiler and linker
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust (stable)
```

PostgreSQL 14 or newer – either [Docker Desktop](https://www.docker.com/products/docker-desktop/)
or Homebrew:

```sh
brew install postgresql@17 && brew services start postgresql@17
createuser --createdb teamcraft && createdb -O teamcraft teamcraft
export TEAMCRAFT_DATABASE_URL=postgres://teamcraft@localhost/teamcraft
```

## 2. Run

```sh
git clone https://github.com/rudalabs/TeamCraft.git && cd TeamCraft
docker compose up -d db     # skip when you use Homebrew's PostgreSQL
cargo run --release         # first build takes a few minutes
```

Open <http://localhost:8080>, create the administrator on the login page,
then a project group (*Projects → New project group*) and your first item.

## 3. Test

```sh
cargo test                                   # unit + end-to-end tests (needs the database)
cargo clippy --all-targets -- -D warnings
curl -s http://localhost:8080/readyz         # {"status":"ok",…}
```

A guided tour through every feature is in the documentation:
[Manual test tour](https://rudalabs.github.io/TeamCraft/guide/tour.html).

## 4. Desktop app (optional)

No extra packages – macOS ships the web view:

```sh
cd desktop && cargo run                     # connect to localhost:8080
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| `cannot connect to PostgreSQL` after 20 s | Docker Desktop running? `brew services list` shows `postgresql@17` started? |
| `role "teamcraft" does not exist` | run the `createuser` / `createdb` lines above |
| `xcrun: error: invalid active developer path` | `xcode-select --install` |
| port 8080 in use (AirPlay receiver uses 5000/7000, not 8080) | `TEAMCRAFT_BIND_ADDR=127.0.0.1:8081 cargo run` |
