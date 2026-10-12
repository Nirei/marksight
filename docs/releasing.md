# Release process

One release equals one annotated tag. The heavy half of the cycle runs in
CI (`.github/workflows/release.yml`); the human half is the version bump
and the tag.

## Prepare

1. Set the new version in `Cargo.toml` and refresh `Cargo.lock`
   (`cargo check` is enough).
2. Add the release to `data/io.github.cinzatech.marksight.metainfo.xml`
   at the top of `<releases>`, and update the date and version in the
   `data/marksight.1` header.
3. Update the packaging metadata:
   - `packaging/deb/debian/changelog`: new `X.Y.Z-1` entry,
   - `packaging/aur/PKGBUILD`: `pkgver`,
   - `packaging/flathub/io.github.cinzatech.marksight.yml`: source URL.
   The `REPLACE_AT_RELEASE` / `REPLACE_AT_SUBMISSION` checksum fields
   are filled when the respective channel picks the release up.
4. Optionally pre-write the `## [X.Y.Z] - YYYY-MM-DD` section in
   `CHANGELOG.md`. If you skip this, the release workflow generates it
   from the commits since the previous tag, using `cliff.toml`:
   keep-a-changelog format, user-facing changes only (features, fixes,
   performance work, user-visible renames; no maintenance, docs,
   refactors or test work).

## Release

```sh
git tag -a vX.Y.Z -m vX.Y.Z
git push origin vX.Y.Z
```

The release workflow then:

1. builds the stripped `x86_64-unknown-linux-gnu` tarball (binary,
   README, CHANGELOG, licenses) and the `.deb` package,
2. documents the version in `CHANGELOG.md` on `main`, prepending the
   generated section unless one was already written by hand, and
   appending the compare link,
3. opens the GitHub release with notes taken from that changelog
   section and the artifacts attached.

Release runs are serialized, so two tags pushed close together cannot
race the changelog update.

## After the release

- Flathub: fill the source `sha256` at submission time (the manifest's
  `x-checker-data` picks up later releases automatically).
- AUR: fill `sha256sums` from the published tarball.
- Debian/Ubuntu PPA: upload from `packaging/deb/` at release time.

## Local changelog tooling

`git cliff` with the repository's `cliff.toml`:

```sh
git cliff --latest --strip all        # the next section, preview only
git cliff --unreleased                 # what would ship as Unreleased
```

The config needs git-cliff 2.x. When touching the template, regenerate
into a scratch file first; a plain `git cliff -o CHANGELOG.md` rewrites
curated sections.
