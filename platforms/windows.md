# TeamCraft on Windows

Local build and test from PowerShell (Windows 10 / 11, x64 or ARM64). ARM64
was tried in a Parallels VM on an Apple M1 – server and desktop app build and
run natively; the extra steps are marked **ARM64**.

## 1. Prerequisites

```powershell
winget install Rustlang.Rustup                       # Rust; installs the MSVC build tools on request
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Git.Git
```

The Build Tools installer keeps running in the background for several
minutes after `winget` reports success – wait until no `setup` process is left,
then open a new terminal so that `cargo` and `git` are on the `PATH`.

**ARM64** needs two more things (Rust targets `aarch64-pc-windows-msvc`):

1. The ARM64 compiler tools – a separate Visual Studio component. Add
   `--add Microsoft.VisualStudio.Component.VC.Tools.ARM64` to the `--override`
   above, or to an existing installation (**elevated** PowerShell):

   ```powershell
   & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" modify `
       --installPath "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools" `
       --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 --passive --norestart
   ```

2. LLVM – the `ring` crate of the TLS stack looks for `clang` on ARM64:

   ```powershell
   winget install LLVM.LLVM
   [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';C:\Program Files\LLVM\bin', 'User')
   ```

   The winget package does not put `C:\Program Files\LLVM\bin` on the `PATH`
   itself; open a new terminal afterwards. Check the toolchain with
   `rustc --version -v` (host `aarch64-pc-windows-msvc`).

Clone into a **short path** such as `C:\src`: deep directories fail with
`Filename too long` (delete the half-written folder before cloning again).

PostgreSQL 16 or newer – either [Docker Desktop](https://www.docker.com/products/docker-desktop/)
(WSL 2 backend; usually not available inside an ARM64 VM) or the native
installer (`winget install PostgreSQL.PostgreSQL.17`). The native installer is
the x64 build, which runs fine under Windows' x64 emulation on ARM64, and it
asks for the password of the `postgres` superuser in a setup window – answer it
(an `--override … --superpassword …` would put the password on a command line).
Create the role in *SQL Shell (psql)* as `postgres`:

```sql
CREATE ROLE teamcraft LOGIN CREATEDB PASSWORD 'choose-one';
CREATE DATABASE teamcraft OWNER teamcraft;
```

and tell TeamCraft about it (same PowerShell session):

```powershell
$env:TEAMCRAFT_DATABASE_URL = "postgres://teamcraft:choose-one@localhost:5432/teamcraft"
```

Alternatively put the line `TEAMCRAFT_DATABASE_URL=…` into `secrets\.env` (the
server reads it, the folder is git-ignored and has to be created by hand).
Write it with `Set-Content -Path secrets\.env -Encoding ascii` – the Windows 11
Notepad could not create new files there in one test.

## 2. Run

```powershell
git clone https://github.com/rudalabs/TeamCraft.git C:\src\TeamCraft; cd C:\src\TeamCraft
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

`/readyz` answers `{"database":"ok","status":"ok","vault":"ok","version":"…"}`.

The backup/restore test uses `pg_dump` and `pg_restore`; with the native
installer they are in `C:\Program Files\PostgreSQL\17\bin` – put that on
`PATH` (`$env:Path += ';C:\Program Files\PostgreSQL\17\bin'`), otherwise the
test is skipped. `teamcraft backup` / `restore` need them the same way.

A guided tour through every feature is in the documentation:
[Manual test tour](https://rudalabs.github.io/TeamCraft/guide/tour.html).

## 4. Desktop app (optional)

WebView2 is part of Windows 10/11, nothing else to install:

```powershell
cd desktop; cargo run --release              # connect to localhost:8080
```

A release build takes about 3 minutes on an M1 VM; the binary is
`desktop\target\release\teamcraft-desktop.exe` and starts without `clang` on
the `PATH` (only a rebuild needs it). Enter the server as `host:port`, then
**Sign in with the browser**.

With Autodesk Inventor or Siemens Solid Edge installed, the app offers the
*STEP* export (see the [desktop README](../desktop/README.md)); it finds the
programs through the registry:

```powershell
reg query "HKCR\Inventor.Application\CLSID"
reg query "HKCR\SolidEdge.Application\CLSID"
```

## 5. Server on another computer

To use the desktop app in a virtual machine with a server on the host (or the
other way round):

* start the server with `TEAMCRAFT_BIND_ADDR=0.0.0.0:8080` – it listens on
  `127.0.0.1` only by default;
* allow port 8080 in the firewall of the computer that **runs the server**
  (macOS firewall when the server runs on the Mac);
* in Parallels' *Shared Network* the host is the VM's default gateway, often
  `10.211.55.2`: `(Get-NetRoute -DestinationPrefix 0.0.0.0/0).NextHop`;
* check `Invoke-RestMethod http://<host>:8080/readyz` before starting the app.

A server inside the same VM avoids both questions.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `linker 'link.exe' not found` | install the VS Build Tools (C++ workload), open a new terminal; on **ARM64** add the `VC.Tools.ARM64` component (§1) |
| `failed to find tool "clang"` (crate `ring`) | **ARM64**: `winget install LLVM.LLVM` and add `C:\Program Files\LLVM\bin` to the `PATH` (§1) |
| `setup.exe modify … --passive` exits with `5007` | run it from an elevated PowerShell; `--wait` is not accepted (`87`) |
| `Filename too long` while cloning | clone into a short path such as `C:\src`, remove the half-written folder first |
| `cannot connect to PostgreSQL` after 20 s | Docker Desktop started? Windows service `postgresql-x64-17` running? |
| `password authentication failed` | check `$env:TEAMCRAFT_DATABASE_URL` (special characters in the password must be URL-encoded) |
| superuser password of the native PostgreSQL lost | in `pg_hba.conf` (keep a `.bak`) set `local` and `127.0.0.1`/`::1` to `trust`, restart the service `postgresql-x64-17`, run `psql -U postgres -c "ALTER USER postgres PASSWORD 'new-one';"` (single quotes inside the double-quoted string), restore the file and restart again |
| Windows Defender asks about network access | allow *private networks* – TeamCraft only listens on 127.0.0.1 unless `TEAMCRAFT_BIND_ADDR` says otherwise |
