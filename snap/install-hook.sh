#!/bin/sh
# Snap install hook (installed with mode 755 by the "hooks" part):
# a configuration file to edit, without secrets.
set -e
mkdir -p "$SNAP_COMMON/vault"
if [ ! -f "$SNAP_COMMON/teamcraft.env" ]; then
    cat > "$SNAP_COMMON/teamcraft.env" <<'EOF'
# TeamCraft (snap) configuration – documented at
# https://rudalabs.github.io/TeamCraft/admin/configuration.html
# After a change: sudo snap restart teamcraft.server

TEAMCRAFT_BIND_ADDR=127.0.0.1:8080
#TEAMCRAFT_PUBLIC_URL=https://plm.example.org

# PostgreSQL over TCP. The role needs a database of its own:
#   sudo -u postgres createuser --pwprompt teamcraft
#   sudo -u postgres createdb -O teamcraft teamcraft
TEAMCRAFT_DB_HOST=127.0.0.1
TEAMCRAFT_DB_PORT=5432
TEAMCRAFT_DB_NAME=teamcraft
TEAMCRAFT_DB_USER=teamcraft
# Keep the password in a file only root can read and point to it:
#TEAMCRAFT_DB_PASSWORD_FILE=/var/snap/teamcraft/common/db_password

RUST_LOG=info,tower_http=info,sqlx=warn
EOF
    chmod 600 "$SNAP_COMMON/teamcraft.env"
fi
