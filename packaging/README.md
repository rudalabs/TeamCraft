# Linux packages

| Package | What | Files |
|---|---|---|
| AUR `teamcraft-git` | server as a systemd service (`teamcraft.service`, user `teamcraft`, data in `/var/lib/teamcraft`, configuration in `/etc/teamcraft/teamcraft.env`) | [`aur/teamcraft-git/`](aur/teamcraft-git) |
| AUR `teamcraft-desktop-git` | desktop client | [`aur/teamcraft-desktop-git/`](aur/teamcraft-desktop-git) |
| Snap `teamcraft` | server as a snap service (configuration in `/var/snap/teamcraft/common/teamcraft.env`) | [`../snap/snapcraft.yaml`](../snap/snapcraft.yaml) |
| Flatpak `io.github.rudalabs.teamcraft` | desktop client | [`flatpak/`](flatpak) |

Shared files (systemd unit, sysusers, tmpfiles, system configuration,
desktop entry, AppStream metadata) are in [`linux/`](linux). Package builds
set `TEAMCRAFT_SYSTEM_CONFIG=/etc/teamcraft/teamcraft.env`, so the installed
binary – service and `teamcraft …` commands – reads the system configuration.

## Arch Linux / CachyOS (AUR, yay)

Until the packages are published on the AUR, build them from this repository:

```sh
cd packaging/aur/teamcraft-git && makepkg -si           # or: yay -Bi packaging/aur/teamcraft-git
cd packaging/aur/teamcraft-desktop-git && makepkg -si
```

Then follow the message of the server package (PostgreSQL role, `systemctl
enable --now teamcraft`, open <http://localhost:8080>). Once published:
`yay -S teamcraft-git teamcraft-desktop-git`.

Publishing (AUR account with an SSH key): `git clone
ssh://aur@aur.archlinux.org/teamcraft-git.git`, copy `PKGBUILD`,
`teamcraft.install` and `.SRCINFO` (`makepkg --printsrcinfo > .SRCINFO`),
commit and push.

## Snap

```sh
snapcraft pack                                       # needs LXD or Multipass
sudo snap install --dangerous teamcraft_0.1.0_amd64.snap
sudoedit /var/snap/teamcraft/common/teamcraft.env    # database
sudo snap restart teamcraft.server
```

Publishing: `snapcraft login`, `snapcraft register teamcraft`,
`snapcraft upload --release=edge teamcraft_*.snap`.

## Flatpak

Every release ships the desktop client as a single-file bundle for x86_64 and
aarch64 (`TeamCraft_<version>_<arch>.flatpak`, built by
`.github/workflows/release.yml` from this manifest). Install it per user; the
GNOME 49 runtime comes from Flathub:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user TeamCraft_*.flatpak
flatpak run io.github.rudalabs.teamcraft
```

The bundle is not on Flathub, so it gets no automatic updates: install the
next release's bundle the same way. Build and install it locally instead:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    --state-dir=target/flatpak-state target/flatpak-build packaging/flatpak/io.github.rudalabs.teamcraft.yml
flatpak run io.github.rudalabs.teamcraft
```

Flathub requires builds without network: generate `cargo-sources.json` for
`desktop/Cargo.lock` with `flatpak-cargo-generator.py`, replace the
`--share=network` build argument by that source, and submit the manifest as a
pull request to `github.com/flathub/flathub`.
