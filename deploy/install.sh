#!/usr/bin/env bash
# TeamCraft – install, upgrade or remove the Docker installation on Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/rudalabs/TeamCraft/main/deploy/install.sh | bash -s -- --user
#   ./deploy/install.sh --dry-run           (from a checkout)
#
# Runs TeamCraft and PostgreSQL with deploy/docker-compose.yml in
# /opt/teamcraft (or ~/teamcraft with --user). Running it again upgrades;
# configuration, the database password and all data are kept.
# Guide: https://rudalabs.github.io/TeamCraft/admin/docker.html
#
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

REPO="rudalabs/TeamCraft"
RAW="https://raw.githubusercontent.com/${REPO}"
API="https://api.github.com/repos/${REPO}"
IMAGE="ghcr.io/rudalabs/teamcraft"
# Compose project "teamcraft-server" (docker-compose.yml) → volume names.
VOLUME_DB="teamcraft-server_db"

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Installs TeamCraft with Docker Compose, or upgrades an existing installation.

  --version V       image version: 0.2.0, v0.2.0 or edge (main branch);
                    default: the newest release (edge while there is none),
                    an existing edge installation stays on edge
  --user            install into ~/teamcraft instead of /opt/teamcraft
  --dir DIR         install into DIR
  --bind ADDR       published address, default 127.0.0.1:8080 (this computer
                    only); 0.0.0.0:8080 serves the network
  --public-url URL  address users open, e.g. https://plm.example.org
  --upgrade         upgrade an existing installation (also the default when
                    one is found)
  --no-pull         do not pull the image (use one that is already loaded)
  --no-admin        do not offer to create the first administrator
  --uninstall       stop and remove the containers; data and configuration stay
  --purge           with --uninstall: also delete the volumes (database and
                    files) and the installation directory – asks first
  --install-docker  install Docker and Compose with the distribution's package
                    manager if they are missing – asks first
  --yes             answer yes to every question (unattended)
  --dry-run         only print what would be done
  -h, --help        this help
EOF
}

# Everything runs inside main(), which is called on the last line: with
# `curl … | bash` the whole script has been read before anything runs, so
# no command can swallow the rest of it from stdin.
main() {

# ---- options ---------------------------------------------------------------

VERSION=""
DIR=""
USER_MODE=0
BIND=""
PUBLIC_URL=""
ACTION="install"
PURGE=0
INSTALL_DOCKER=0
ASSUME_YES=0
DRY_RUN=0
PULL=1
OFFER_ADMIN=1

need_value() {
  if [ "$#" -lt 2 ] || [ -z "$2" ]; then
    echo "install.sh: $1 needs a value" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) need_value "$@"; VERSION=$2; shift ;;
    --version=*) VERSION=${1#*=} ;;
    --dir) need_value "$@"; DIR=$2; shift ;;
    --dir=*) DIR=${1#*=} ;;
    --user) USER_MODE=1 ;;
    --bind) need_value "$@"; BIND=$2; shift ;;
    --bind=*) BIND=${1#*=} ;;
    --public-url) need_value "$@"; PUBLIC_URL=$2; shift ;;
    --public-url=*) PUBLIC_URL=${1#*=} ;;
    --upgrade) ACTION="install" ;;
    --uninstall) ACTION="uninstall" ;;
    --purge) PURGE=1 ;;
    --install-docker) INSTALL_DOCKER=1 ;;
    --no-pull) PULL=0 ;;
    --no-admin) OFFER_ADMIN=0 ;;
    --yes|-y) ASSUME_YES=1 ;;
    --dry-run|-n) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install.sh: unknown option '$1' (see --help)" >&2; exit 2 ;;
  esac
  shift
done

# ---- helpers ---------------------------------------------------------------

if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
else
  BOLD=""; DIM=""; RED=""; YELLOW=""; RESET=""
fi

say() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf '%swarning:%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

# Prints a command the way it would be typed.
show() {
  local out="" arg
  for arg in "$@"; do out+="$(printf '%q' "$arg") "; done
  printf '%s    + %s%s\n' "$DIM" "${out% }" "$RESET"
}

# Runs a command, or only prints it with --dry-run.
run() {
  show "$@"
  if [ "$DRY_RUN" -eq 0 ]; then "$@"; fi
}

has() { command -v "$1" >/dev/null 2>&1; }

# A terminal for questions, also when the script itself comes from a pipe.
has_tty() { { : </dev/tty; } 2>/dev/null; }

# ask "question" default(y|n) – returns 0 for yes.
ask() {
  local question=$1 default=$2 answer hint="[y/N]"
  [ "$default" = "y" ] && hint="[Y/n]"
  if [ "$ASSUME_YES" -eq 1 ]; then return 0; fi
  if ! has_tty; then [ "$default" = "y" ]; return; fi
  printf '%s %s ' "$question" "$hint" >/dev/tty
  read -r answer </dev/tty || answer=""
  answer=${answer:-$default}
  case "$answer" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

SUDO=""        # prefix for file operations in the installation directory
DOCKER_SUDO="" # prefix for docker commands

as_root() {
  if [ -n "$SUDO" ]; then sudo "$@"; else "$@"; fi
}

run_root() {
  if [ -n "$SUDO" ]; then run sudo "$@"; else run "$@"; fi
}

docker_cmd() {
  if [ -n "$DOCKER_SUDO" ]; then sudo docker "$@"; else docker "$@"; fi
}

compose() {
  docker_cmd compose --project-directory "$DIR" -f "$DIR/docker-compose.yml" "$@"
}

run_compose() {
  local prefix=(docker)
  [ -n "$DOCKER_SUDO" ] && prefix=(sudo docker)
  run "${prefix[@]}" compose --project-directory "$DIR" -f "$DIR/docker-compose.yml" "$@"
}

TMP=$(mktemp -d "${TMPDIR:-/tmp}/teamcraft-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

# write_file PATH MODE – content from stdin; never echoes the content.
write_file() {
  local path=$1 mode=$2 tmp
  tmp=$(mktemp "$TMP/stage.XXXXXX")
  cat >"$tmp"
  if [ "$DRY_RUN" -eq 1 ]; then
    note "${DIM}would write $path (mode $mode)${RESET}"
    return
  fi
  as_root install -m "$mode" "$tmp" "$path"
  rm -f "$tmp"
}

http_get() { # URL [OUTPUT]
  if has curl; then
    if [ "$#" -eq 2 ]; then curl -fsSL --retry 3 -o "$2" "$1"; else curl -fsSL --retry 3 "$1"; fi
  elif has wget; then
    if [ "$#" -eq 2 ]; then wget -q -O "$2" "$1"; else wget -q -O - "$1"; fi
  else
    die "neither curl nor wget is installed"
  fi
}

# Reads KEY=value from an env file (no shell evaluation).
env_value() { # FILE KEY
  as_root cat "$1" 2>/dev/null | sed -n "s/^$2=//p" | tail -n 1 | tr -d '"' || true
}

gen_password() {
  # 40 characters from [A-Za-z0-9]: no quoting or URL-encoding problems.
  head -c 64 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | cut -c 1-40
}

# ---- checks ----------------------------------------------------------------

[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux (see https://rudalabs.github.io/TeamCraft/ for other systems)"
[ "$DRY_RUN" -eq 1 ] && say "Dry run: nothing is changed."

if [ -z "$DIR" ]; then
  if [ "$USER_MODE" -eq 1 ]; then DIR="$HOME/teamcraft"; else DIR="/opt/teamcraft"; fi
fi
case "$DIR" in /*) ;; *) DIR="$PWD/$DIR" ;; esac
while [ "${DIR%/}" != "$DIR" ]; do DIR=${DIR%/}; done
# --uninstall --purge deletes the directory: never / or a top-level one.
case "$DIR" in
  /[!/]*/[!/]*) ;;
  *) die "refusing to use '${DIR:-/}' as the installation directory – use one of its own, e.g. /opt/teamcraft" ;;
esac

# Root privileges only where the directory needs them.
if [ "$(id -u)" -ne 0 ]; then
  probe=$DIR
  while [ ! -e "$probe" ]; do probe=$(dirname "$probe"); done
  if [ ! -w "$probe" ] || { [ -e "$DIR/secrets" ] && [ ! -r "$DIR/secrets" ]; }; then
    has sudo || die "$DIR needs root privileges and sudo is missing – run as root or use --user"
    SUDO="sudo"
  fi
fi

install_docker() {
  local id="" like="" pkgs=() mgr=()
  if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    id=$(. /etc/os-release && echo "${ID:-}")
    # shellcheck disable=SC1091
    like=$(. /etc/os-release && echo "${ID_LIKE:-}")
  fi
  case " $id $like " in
    *" ubuntu "*) mgr=(apt-get install -y); pkgs=(docker.io docker-compose-v2) ;;
    *" debian "*) mgr=(apt-get install -y); pkgs=(docker.io docker-compose) ;;
    *" fedora "*) mgr=(dnf install -y); pkgs=(moby-engine docker-compose) ;;
    *" arch "*) mgr=(pacman -S --needed --noconfirm); pkgs=(docker docker-compose) ;;
    *" suse "*|*" opensuse "*) mgr=(zypper --non-interactive install); pkgs=(docker docker-compose) ;;
    *) die "unknown distribution '$id' – install Docker Engine and the Compose plugin: https://docs.docker.com/engine/install/" ;;
  esac
  say "Docker is missing. Packages for $id: ${pkgs[*]}"
  ask "Install them with ${mgr[0]} now?" n || die "Docker is required – install it and run this script again"
  local root=()
  [ "$(id -u)" -ne 0 ] && root=(sudo)
  if [ "${mgr[0]}" = "apt-get" ]; then run ${root[@]+"${root[@]}"} apt-get update; fi
  run ${root[@]+"${root[@]}"} "${mgr[@]}" "${pkgs[@]}"
  run ${root[@]+"${root[@]}"} systemctl enable --now docker
}

check_docker() {
  if ! has docker; then
    if [ "$INSTALL_DOCKER" -eq 1 ]; then
      install_docker
    elif [ "$DRY_RUN" -eq 1 ]; then
      warn "docker is not installed (a real run stops here; --install-docker installs it)"
      return
    else
      cat >&2 <<'EOF'
error: Docker is not installed. Install Docker Engine with the Compose v2 plugin:
  Arch / CachyOS:  sudo pacman -S docker docker-compose
  Ubuntu:          sudo apt install docker.io docker-compose-v2
  Debian 13:       sudo apt install docker.io docker-compose
  Fedora:          sudo dnf install moby-engine docker-compose
  others:          https://docs.docker.com/engine/install/
then: sudo systemctl enable --now docker
(or run this script with --install-docker)
EOF
      exit 1
    fi
  fi
  [ "$DRY_RUN" -eq 1 ] && ! has docker && return

  if [ -n "$SUDO" ]; then
    DOCKER_SUDO="sudo"
  elif ! docker info >/dev/null 2>&1; then
    if has sudo && sudo docker info >/dev/null 2>&1; then
      DOCKER_SUDO="sudo"
    elif [ "$DRY_RUN" -eq 1 ]; then
      warn "the Docker daemon is not reachable (is it running? sudo systemctl enable --now docker)"
      return
    else
      die "the Docker daemon is not reachable – start it (sudo systemctl enable --now docker) or add yourself to the docker group"
    fi
  fi

  local v
  v=$(docker_cmd compose version --short 2>/dev/null || true)
  v=${v#v}
  case "$v" in
    [2-9].*|[1-9][0-9].*) note "Docker Compose $v" ;;
    *)
      if [ "$DRY_RUN" -eq 1 ]; then warn "Docker Compose v2 is missing"; return; fi
      die "Docker Compose v2 is missing (the 'docker compose' plugin, not the old docker-compose 1.x) – https://docs.docker.com/compose/install/linux/"
      ;;
  esac
}

# ---- uninstall ---------------------------------------------------------------

uninstall() {
  [ -f "$DIR/docker-compose.yml" ] || die "no installation in $DIR (use --dir)"
  # Only a directory written by this script is stopped or deleted.
  if ! grep -q "image: $IMAGE:" "$DIR/docker-compose.yml" || ! as_root test -f "$DIR/.env"; then
    die "$DIR does not look like a TeamCraft installation made by install.sh – nothing changed"
  fi
  check_docker
  say "Stopping TeamCraft in $DIR"
  if [ "$PURGE" -eq 1 ]; then
    warn "--purge deletes the database, every stored file and $DIR. This cannot be undone."
    ask "Delete everything?" n || die "nothing deleted"
    run_compose down --volumes --remove-orphans
    run_root rm -rf "$DIR"
    say "TeamCraft and all its data are removed."
  else
    run_compose down --remove-orphans
    say "Containers removed. The volumes (database, files) and $DIR are kept;"
    note "running install.sh again starts the same installation."
    note "Delete everything: install.sh --uninstall --purge"
  fi
}

if [ "$ACTION" = "uninstall" ]; then
  uninstall
  exit 0
fi
[ "$PURGE" -eq 1 ] && die "--purge only works together with --uninstall"

# ---- install / upgrade ---------------------------------------------------------

check_docker

EXISTING=0
if as_root test -f "$DIR/.env" && as_root test -f "$DIR/secrets/db_password"; then
  EXISTING=1
fi

# Version: the image tag (0.2.0 / edge) and the git ref of the deploy files.
VERSION=${VERSION#v}
if [ -z "$VERSION" ] && [ "$EXISTING" -eq 1 ] && [ "$(env_value "$DIR/.env" TEAMCRAFT_VERSION)" = "edge" ]; then
  VERSION="edge"
fi
if [ -z "$VERSION" ]; then
  VERSION=$(http_get "$API/releases?per_page=30" 2>/dev/null \
    | grep -o '"tag_name": *"v[0-9]*\.[0-9]*\.[0-9]*"' \
    | sed -n '1s/.*"v\([^"]*\)"$/\1/p' || true)
  if [ -z "$VERSION" ]; then
    VERSION="edge"
    warn "no release found – installing the development version (edge, the main branch)"
  fi
fi
case "$VERSION" in
  main) VERSION="edge" ;;
  *[!A-Za-z0-9._-]*) die "invalid version '$VERSION'" ;;
esac
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) REF="v$VERSION" ;;
  *) REF="main" ;;
esac

# Deploy files: from this checkout when the script runs from one, else downloaded.
SRC=""
script=${BASH_SOURCE[0]:-}
if [ -n "$script" ] && [ -f "$script" ]; then
  here=$(cd "$(dirname "$script")" && pwd)
  if [ -f "$here/docker-compose.yml" ] && [ -f "$here/teamcraft.env.example" ] \
    && [ -f "$here/../Cargo.toml" ]; then
    SRC=$here
  fi
fi

if [ "$EXISTING" -eq 1 ]; then
  say "Upgrading TeamCraft in $DIR to $VERSION"
else
  say "Installing TeamCraft $VERSION into $DIR"
fi

if [ -n "$SRC" ]; then
  note "deploy files from $SRC"
  cp "$SRC/docker-compose.yml" "$TMP/docker-compose.yml"
  cp "$SRC/teamcraft.env.example" "$TMP/teamcraft.env.example"
else
  note "deploy files from $RAW/$REF/deploy/"
  http_get "$RAW/$REF/deploy/docker-compose.yml" "$TMP/docker-compose.yml" \
    || die "cannot download docker-compose.yml for $REF (does version $VERSION exist?)"
  http_get "$RAW/$REF/deploy/teamcraft.env.example" "$TMP/teamcraft.env.example" \
    || die "cannot download teamcraft.env.example for $REF"
fi
grep -q "image: $IMAGE:" "$TMP/docker-compose.yml" || die "the downloaded docker-compose.yml is not TeamCraft's"

run_root mkdir -p "$DIR"
write_file "$DIR/docker-compose.yml" 644 <"$TMP/docker-compose.yml"
write_file "$DIR/teamcraft.env.example" 644 <"$TMP/teamcraft.env.example"

# Server configuration: created once, then it belongs to the administrator.
if as_root test -f "$DIR/teamcraft.env"; then
  note "keeping $DIR/teamcraft.env"
  as_root cat "$DIR/teamcraft.env" >"$TMP/teamcraft.env.current"
else
  cp "$TMP/teamcraft.env.example" "$TMP/teamcraft.env.current"
fi
if [ -n "$PUBLIC_URL" ]; then
  case "$PUBLIC_URL" in http://*|https://*) ;; *) die "--public-url must start with http:// or https://" ;; esac
  # Written unquoted into teamcraft.env: no spaces, quotes, $, # or backslashes.
  case "$PUBLIC_URL" in
    *[[:space:][:cntrl:]]*|*[\"\'\$\`\\#]*) die "--public-url must be a plain URL without spaces or quotes" ;;
  esac
  { grep -v '^TEAMCRAFT_PUBLIC_URL=' "$TMP/teamcraft.env.current" || true
    printf 'TEAMCRAFT_PUBLIC_URL=%s\n' "${PUBLIC_URL%/}"; } >"$TMP/teamcraft.env.new"
  mv "$TMP/teamcraft.env.new" "$TMP/teamcraft.env.current"
fi
if [ -n "$PUBLIC_URL" ] || ! as_root test -f "$DIR/teamcraft.env"; then
  write_file "$DIR/teamcraft.env" 600 <"$TMP/teamcraft.env.current"
fi

# Database password: generated once and never printed. The secrets/
# directory is owner-only; the file itself must be readable for the postgres
# and teamcraft users inside the containers (Compose mounts it unchanged).
if as_root test -f "$DIR/secrets/db_password"; then
  note "keeping the database password in $DIR/secrets/db_password"
else
  # PostgreSQL reads the password only when its volume is initialised: a new
  # one would not match a database volume that is still there.
  if has docker && docker_cmd volume inspect "$VOLUME_DB" >/dev/null 2>&1; then
    die "the database volume $VOLUME_DB already exists, but $DIR/secrets/db_password is missing.
  Restore secrets/db_password from the previous installation directory (or a backup),
  or – only if its data is no longer needed – remove the volume: docker volume rm $VOLUME_DB"
  fi
  run_root install -d -m 700 "$DIR/secrets"
  gen_password | write_file "$DIR/secrets/db_password" 644
fi

# Compose settings: version and published address.
[ -z "$BIND" ] && [ "$EXISTING" -eq 1 ] && BIND=$(env_value "$DIR/.env" TEAMCRAFT_BIND)
BIND=${BIND:-127.0.0.1:8080}
# Written into .env: host:port with a numeric port, nothing else.
case "${BIND##*:}" in ""|*[!0-9]*) BIND_OK=0 ;; *) BIND_OK=1 ;; esac
case "$BIND" in *:*) ;; *) BIND_OK=0 ;; esac
case "$BIND" in *[!A-Za-z0-9.:\[\]-]*) BIND_OK=0 ;; esac
[ "$BIND_OK" -eq 1 ] || die "--bind needs host:port, e.g. 127.0.0.1:8080 or 0.0.0.0:8080"
write_file "$DIR/.env" 644 <<EOF
# Written by install.sh – used by docker compose in this directory.
TEAMCRAFT_VERSION=$VERSION
TEAMCRAFT_BIND=$BIND
EOF

if [ "$PULL" -eq 1 ]; then
  run_compose pull
fi
run_compose up -d --remove-orphans

# ---- wait until ready --------------------------------------------------------

PORT=${BIND##*:}
BIND_HOST=${BIND%:*}
HOST=$BIND_HOST
case "$HOST" in ""|0.0.0.0|"[::]"|"::") HOST="127.0.0.1" ;; esac
READY_URL="http://$HOST:$PORT/readyz"

if [ "$DRY_RUN" -eq 0 ]; then
  say "Waiting for $READY_URL"
  ready=0
  for _ in $(seq 1 90); do
    body=$(http_get "$READY_URL" 2>/dev/null || true)
    case "$body" in *'"status":"ok"'*) ready=1; break ;; esac
    sleep 2
  done
  if [ "$ready" -eq 0 ]; then
    compose ps || true
    compose logs --tail 50 teamcraft || true
    die "TeamCraft did not become ready within 3 minutes (logs above)"
  fi
  note "ready"
else
  note "${DIM}would wait for $READY_URL${RESET}"
fi

# ---- first administrator ------------------------------------------------------

DC="docker compose"
[ -n "$DOCKER_SUDO" ] && DC="sudo docker compose"
QDIR=$(printf '%q' "$DIR")
CREATE_ADMIN="cd $QDIR && $DC exec teamcraft teamcraft user create --username admin --role admin"

users=1
if [ "$DRY_RUN" -eq 0 ]; then
  users=$(compose exec -T teamcraft teamcraft user list </dev/null 2>/dev/null | tail -n +2 | grep -c . || true)
fi
if [ "$DRY_RUN" -eq 0 ] && [ "${users:-0}" -eq 0 ]; then
  if [ "$OFFER_ADMIN" -eq 1 ] && [ "$ASSUME_YES" -eq 0 ] && has_tty \
    && ask "Create the first administrator now?" y; then
    printf 'Username [admin]: ' >/dev/tty
    read -r admin </dev/tty || admin=""
    admin=${admin:-admin}
    compose exec teamcraft teamcraft user create --username "$admin" --role admin </dev/tty \
      || warn "the administrator was not created – run: $CREATE_ADMIN"
  else
    say "Next step: create the first administrator (the password is asked for):"
    note "$CREATE_ADMIN"
  fi
fi

URL=${PUBLIC_URL%/}
[ -z "$URL" ] && URL=$(env_value "$DIR/teamcraft.env" TEAMCRAFT_PUBLIC_URL)
[ -z "$URL" ] && URL="http://localhost:$PORT"

echo
if [ "$DRY_RUN" -eq 1 ]; then
  say "Dry run complete: TeamCraft $VERSION would run at $URL"
else
  say "TeamCraft $VERSION runs at $URL"
fi
note "directory:  $DIR (configuration: teamcraft.env)"
note "status:     cd $QDIR && $DC ps"
note "logs:       cd $QDIR && $DC logs -f teamcraft"
note "upgrade:    run install.sh again (data and configuration stay)"
case "$BIND_HOST" in
  127.*|localhost|"[::1]")
    note "only this computer can connect; a reverse proxy with TLS or --bind 0.0.0.0:8080 serves the network"
    ;;
  *)
    case "$URL" in
      http://localhost:*)
        warn "set the address users open: install.sh --public-url http://<host>:$PORT (forms are checked against it)"
        ;;
    esac
    ;;
esac
note "guide:      https://rudalabs.github.io/TeamCraft/admin/docker.html"
}

main "$@"
