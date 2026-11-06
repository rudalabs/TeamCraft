# Release checklist

For maintainers cutting a release. The mechanics (version check, builds,
`SHA256SUMS.txt`, GitHub Release, image) are in
`.github/workflows/release.yml` and in
[CONTRIBUTING.md → Releases and versions](https://github.com/rudalabs/TeamCraft/blob/main/CONTRIBUTING.md#releases-and-versions).
This page is the order of the steps and what to check by hand. Points marked
*open* are not decided yet (maintainer decision).

## 1. Before the tag

- [ ] `main` is green: CI (fmt, clippy, tests on PostgreSQL 16 and 18, the
      macOS and Windows builds), docs, coverage floor, and the last *Release*
      run of `main` including the Docker install smoke test.
- [ ] The latest *Fuzzing* and *Scale check* runs have no new failure.
- [ ] No open pull request is meant to go into this release.
- [ ] `CHANGELOG.md`: rename *Unreleased* to the version and date, start a new
      empty *Unreleased*, update the compare links at the bottom. Every
      **Breaking** entry is also under *Breaking changes per version* in
      [Upgrade](../admin/upgrade.md).
- [ ] Migrations added since the last release are listed in the version notes
      of the upgrade page if they need any action (extensions, long
      back-fills, changed behaviour).

## 2. Release candidate

- [ ] For a release candidate, tag `vX.Y.Z-rc.N`. A suffixed version becomes
      a pre-release and does not move the image tag `latest`.
- [ ] How long a candidate stays out before the final tag, and who signs it
      off: *open*.
- [ ] Run the [desktop test](#desktop-test-per-platform) on every platform
      and the [server test](#server-test) with the candidate.

## 3. Version and tag

- [ ] Set the same version in `Cargo.toml`, `desktop/Cargo.toml` and
      `desktop/tauri.conf.json`. The workflow refuses a mismatch.
- [ ] `cargo check` and `cargo check --manifest-path desktop/Cargo.toml`, so
      that both `Cargo.lock` files follow; commit through a pull request.
- [ ] Tag the merged commit of `main` and push the tag:
      `git tag -a vX.Y.Z -m "TeamCraft X.Y.Z" && git push origin vX.Y.Z`.

## 4. After the workflow

- [ ] The *Release* run of the tag is green (server, desktop, Flatpak, image,
      image manifest, Docker smoke test, publish).
- [ ] The GitHub Release exists, as a pre-release for `0.x` and suffixed
      versions. It holds `SHA256SUMS.txt` and, per platform (as for 0.2.0):
  - server: `teamcraft-X.Y.Z-linux-x86_64.tar.gz`, `-linux-aarch64.tar.gz`,
    `-windows-x64.zip`;
  - Linux desktop: `.AppImage` (x86_64), `.deb` and `.rpm` (x86_64 and
    aarch64), `.flatpak` (x86_64 and aarch64);
  - macOS desktop: `_aarch64.dmg`, `_aarch64.app.zip`;
  - Windows desktop: `_x64_en-US.msi`, `_x64-setup.exe`, `_x64_portable.zip`
    and the same three for `arm64`.
- [ ] Checksums: download everything into an empty folder
      (`gh release download vX.Y.Z`) and run
      `sha256sum -c SHA256SUMS.txt`. Every line says `OK`.
- [ ] Image tags: `docker manifest inspect ghcr.io/rudalabs/teamcraft:X.Y.Z`
      shows `linux/amd64` and `linux/arm64`, and so do `:X.Y` and, for a
      release without a suffix, `:latest`.
- [ ] Release notes: the generated notes are complemented with the
      changelog section of the version, with the *Breaking* entries first
      and a link to [Upgrade](../admin/upgrade.md).

## 5. Announcement

- [ ] Where a release is announced besides the GitHub Release: *open*.
- [ ] Which versions get fixes after the release (support window): *open*.

## Server test

With the candidate's image or server archive, against a copy of a database
of the previous release:

1. Upgrade as in [Upgrade](../admin/upgrade.md): backup, start, migrations
   run, `/readyz` answers `"status":"ok"`.
2. `teamcraft vault check --verify-hashes` reports no problem.
3. Sign in (password and, if configured, single sign-on), open an item, look
   at its 3D preview, download a file.
4. Roll back once as described there, then upgrade again.

## Desktop test per platform

About 15 minutes per platform, against a test server of the same version:
Linux x86_64 (AppImage or `.deb`), Linux aarch64 (Flatpak), macOS (`.dmg`),
Windows x64 (`.msi`) and Windows ARM64 (`_arm64-setup.exe` or the portable
zip). Note the platform, the package and the result of every step.

1. **Install and first start.** The installers are not code-signed: macOS asks
   to confirm the first start (*right-click → Open*), Windows SmartScreen shows
   *More info → Run anyway*. The Flatpak installs with
   `flatpak install --user TeamCraft_X.Y.Z_<arch>.flatpak`. *Help → About TeamCraft*
   (macOS: *TeamCraft → About TeamCraft*)
   shows the version.
2. **Sign in** through the browser and, once, with a personal access token.
   The navigator shows the project groups.
3. **New item** (`Ctrl/Cmd+N`) with a CAD file from `tests/fixtures/cad`. It
   appears in the navigator, and its overview shows the preview.
4. **Check out**, change the file in the working folder, switch back to the
   app: *Check in…* is highlighted. Check in, and the history has the new
   version.
5. **Get…** another item with a BOM: the BOM and the referenced files arrive
   in the working folder in their folders.
6. **Import BOM…** from a CSV file into a checked-out assembly.
7. **3D viewer** of a STEP file, **Pack and Go…** into a folder.
8. **Appearance:** switch the operating system between light and dark; the
   *TeamCraft* skin follows.
9. Quit and start again: the app opens signed in (the token is stored).

Whether a failed step blocks the release or becomes a known issue in the
release notes: *open*.
