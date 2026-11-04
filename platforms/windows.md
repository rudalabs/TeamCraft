# TeamCraft on Windows

Local build and test from PowerShell (Windows 10 / 11, x64 or ARM64).

## 1. Prerequisites

```powershell
winget install Rustlang.Rustup                       # Rust; installs the MSVC build tools on request
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools"   # if rustup asks for them
winget install Git.Git
```

PostgreSQL 16 or newer – either [Docker Desktop](https://www.docker.com/products/docker-desktop/)
(WSL 2 backend) or the native installer (`winget install PostgreSQL.PostgreSQL.17`).
With the native installer, create the role in *SQL Shell (psql)* as `postgres`:

```sql
CREATE ROLE teamcraft LOGIN CREATEDB PASSWORD 'choose-one';
CREATE DATABASE teamcraft OWNER teamcraft;
```

and tell TeamCraft about it (same PowerShell session):

```powershell
$env:TEAMCRAFT_DATABASE_URL = "postgres://teamcraft:choose-one@localhost:5432/teamcraft"
```

## 2. Run

```powershell
git clone https://github.com/rudalabs/TeamCraft.git; cd TeamCraft
docker compose up -d db     # skip when you use the native PostgreSQL
cargo run --release         # first build takes a few minutes
```

Open <http://localhost:8080>, create the administrator on the login page,
then a project group (*Projects → New project group*) and your first item.

## 3. Test

```powershell
cargo test                                   # unit + end-to-end tests (needs the database)
cargo clippy --all-targets -- -D warnings
Invoke-RestMethod http://localhost:8080/readyz
```

A guided tour through every feature is in the documentation:
[Manual test tour](https://rudalabs.github.io/TeamCraft/guide/tour.html).

## 4. Desktop app (optional)

WebView2 is part of Windows 10/11, nothing else to install:

```powershell
cd desktop; cargo run                        # connect to localhost:8080
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| `linker 'link.exe' not found` | install the VS Build Tools (C++ workload), open a new terminal |
| `cannot connect to PostgreSQL` after 20 s | Docker Desktop started? Windows service `postgresql-x64-17` running? |
| `password authentication failed` | check `$env:TEAMCRAFT_DATABASE_URL` (special characters in the password must be URL-encoded) |
| Windows Defender asks about network access | allow *private networks* – TeamCraft only listens on 127.0.0.1 |
