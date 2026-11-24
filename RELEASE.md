# Release

VerbaQ releases use the 1.x line. The first number changes only when the owner
decides. The second number increases by one for every release.

## Sources of truth

- `VERSION` holds the release version, for example `1.1`.
- `Cargo.toml` holds the same version in semver form, for example `1.1.0`.
- `CHANGELOG.md` gets an entry for every version.
- `packaging/nfpm.yaml` reads the version from the environment.

## Before a release

Commit the current version first. Before bumping to a new version, the previous
`dist/` package set must already be copied to
`TTP-pkgs/pkgs/verbaq/<version>/` with the deb, rpm and Arch package. The build
script refuses to replace an older `dist/` that is not archived.

```sh
scripts/check-commit.sh
```

The check runs formatting, clippy with warnings denied, tests, desktop metadata
validation and scans for em dashes.

Then build every package:

```sh
scripts/build-release.sh
```

Output goes to `dist/`:

- `verbaq` (the standalone binary)
- `verbaq_<version>_amd64.deb`
- `verbaq-<version>-1.x86_64.rpm`
- `verbaq-<version>-1-x86_64.pkg.tar.zst`
- `verbaq-<version>-linux-x86_64.tar.gz`
- `SHA256SUMS`

Install the Arch package on a clean machine and test setup, dictation, the
shortcut and uninstall.

## Commit and tag

Commit subject: `VerbaQ <version>: <summary>`.

Example: `VerbaQ 1.3: softer cues and sound output`.

Do not commit, tag, push or publish until the owner confirms.

Tag format: `v<version>`, for example `v1.3`.

## GitHub release

- Repository: `TTP-VerbaQ`.
- Upload the deb, rpm, Arch package, tarball and `SHA256SUMS`.
- Use the version as the release title: `VerbaQ 1.3`.
- Release notes come from `CHANGELOG.md`.
- Keep only the latest package set in the repository. The full history lives in
  `/home/danuja/TacTalaPaca/TTP-pkgs/` after owner approval.

## TTP-pkgs

After the owner confirms the release:

1. Copy the deb, rpm and Arch package to `TTP-pkgs/pkgs/verbaq/<version>/`.
2. Follow `TTP-pkgs/AGENTS.md` for signing, indexing and verification.
3. Do not upload to R2 until `verify.sh` passes.

## Repository rules

- No remote, commit, tag or push without explicit owner confirmation.
- No AI language, filler or em dashes in release text.
- Keep generated files out of git.
- Do not rebuild an existing release folder without approval.
