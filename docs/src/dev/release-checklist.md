# Release checklist

This is the list a maintainer follows to cut a release. The rules behind it are
in [CONTRIBUTING.md](https://github.com/rudalabs/TeamCraft/blob/main/CONTRIBUTING.md#releases-and-versions)
(*Releases and versions*) and in the release rules of
[CLAUDE.md](https://github.com/rudalabs/TeamCraft/blob/main/CLAUDE.md). The
workflow is `.github/workflows/release.yml`. Nothing is published before the
maintainer pushes the tag.

Version numbers follow [Semantic Versioning](https://semver.org). While the
version is `0.x`, a release may contain breaking changes. From `1.0` on, the
rules of the [support policy](../admin/support-policy.md) apply.

## 1. Before the release candidate

- [ ] `main` is green: the CI workflow (`ci.yml`, fmt, clippy, tests on
      PostgreSQL 16 and 18, macOS and Windows builds), `docs.yml` and
      `release.yml` (build only) all pass on the commit to release.
- [ ] The latest run of the scale check (`scale.yml`, manual or weekly) passed
      on this commit or a later one, or a failure is known and written down in
      the release notes.
- [ ] `CHANGELOG.md` has every change since the last tag under *Unreleased*,
      grouped as *Added*, *Changed*, *Fixed* and *Security*. Each entry is
      taken from a merged change; nothing is added from memory.
- [ ] The threat model (`docs/src/dev/threat-model.md`) and
      `docs/OPEN_WORK.md` show no open finding that the release claims to fix.
- [ ] The work-in-progress and no-warranty notices are still in the README,
      `docs/src/index.md`, the web footer and the desktop *About* dialog, unless
      the maintainer decides that this release is the first stable one.

## 2. Version bump and release candidate

The version must be the same in three files. The release workflow refuses a
mismatch.

- [ ] `Cargo.toml` (`version`)
- [ ] `desktop/Cargo.toml` (`version`)
- [ ] `desktop/tauri.conf.json` (`version`)
- [ ] Run `cargo check` and `cargo check --manifest-path desktop/Cargo.toml`
      so that both `Cargo.lock` files follow, then commit the bump on `main`
      through a pull request.
- [ ] Move the *Unreleased* entries of `CHANGELOG.md` under the new version
      heading, with the date, and leave an empty *Unreleased* section.
- [ ] Tag a release candidate: `git tag -a v1.0.0-rc.1 -m "TeamCraft 1.0.0-rc.1"`
      and `git push origin v1.0.0-rc.1`. A suffixed tag is a pre-release. It
      gets no `:latest` image tag and no `:<major>.<minor>` update.

## 3. Soak time

- [ ] Let the release candidate run for the soak period before the final tag.
      The project's proposed minimum is **7 days**, with no new open bug
      rated as blocking. A fix during the soak needs a new candidate
      (`-rc.2`) and the soak starts again.
- [ ] Test the candidate: the install from the container image with
      `deploy/install.sh`, an upgrade from the previous release with data, and
      one desktop installer per platform.
- [ ] Note what was found in the soak in the changelog or in the release
      notes.

## 4. Final tag

- [ ] Tag the same commit as the last candidate, without a suffix:
      `git tag -a v1.0.0 -m "TeamCraft 1.0.0"` and `git push origin v1.0.0`.
      Do not move a tag that was pushed; use a new version instead.
- [ ] Watch `release.yml` for the tag. It builds everything again from the tag,
      runs the install smoke test against the new image, and creates the GitHub
      Release. A `-rc` or `0.x` version is marked as a pre-release.

## 5. Verify the assets

- [ ] The GitHub Release lists the server archives (Linux x86_64 and ARM64,
      Windows x64), the desktop installers (AppImage, `.deb`, `.rpm`,
      Flatpak, `.dmg`, `.msi`, NSIS setup and portable `.zip`) and
      `SHA256SUMS.txt`.
- [ ] Download `SHA256SUMS.txt` and at least one archive and check the hash
      with `sha256sum -c SHA256SUMS.txt --ignore-missing`. A mismatch means
      the release is withdrawn, not patched.
- [ ] The release notes are the generated notes, with the changelog section
      added when it is more complete.

## 6. Container image tags

The image `ghcr.io/rudalabs/teamcraft` is published by CI only.

- [ ] The workflow sets `:<version>`, `:<major>.<minor>` and, for a version
      without a suffix, `:latest` (also for `0.x`). For `v1.0.0` that is `1.0.0`,
      `1.0` and `latest`. There is no `:<major>` tag.
- [ ] For a suffixed candidate such as `v1.0.0-rc.1`, `:latest` must not move.
      Check the tags on the registry after the run and report any mismatch.
- [ ] The `:edge` tag comes from `main`; it is not a release.
- [ ] Pull the image by its release tag and check `/readyz` on a test
      instance. Do not build or push an image by hand.

## 7. Announcement

- [ ] Publish the GitHub Release (it is created by the workflow; check that it
      is not a draft).
- [ ] Update the install examples that name a version (`deploy/install.sh`
      help text and the Docker guide) through a pull request, if they still
      name an older release.
- [ ] Post the announcement with the link to the release and the changelog
      section. Say what changed for operators first: configuration, migrations
      and the upgrade path.
- [ ] Report security fixes in the release through the advisory that was
      prepared under [SECURITY.md](https://github.com/rudalabs/TeamCraft/blob/main/SECURITY.md),
      and publish it on the release day.

## What is not part of a release

- No package registries. Nothing is published to crates.io, npm or a similar
  registry.
- No code signing yet. Installers are not signed (see *Releases and versions*
  in CONTRIBUTING.md); the release notes say so.
- No backport branch unless the support policy requires one.
