# AGENTS.md - VerbaQ

This file is the source of truth for work on VerbaQ. Read it before changing code.

## Shared rules

- The product name is VerbaQ in text. The binary, package, paths and commands stay `verbaq`.
- Plain words, short lines, neutral tone. No hype, no filler, no first person, no AI language.
- Never use em dashes. Use a period, comma or colon.
- Comments explain why, not what. Do not restate the code.
- Versioning uses the 1.x line. `VERSION` is the single source for the release version.
- `Cargo.toml` carries the same version in semver form, for example `1.1.0` for `1.1`.
- `CHANGELOG.md` gets an entry for every version.
- Commit subjects use `VerbaQ <version>: <summary>`. One version per commit.
- Do not add a remote, commit, tag, push, publish or create a release without explicit owner confirmation.
- Keep build output, packages, model files and private data out of git.
- Linux x86_64 only.
- Run `scripts/check-commit.sh` before asking for a commit.

## Product

VerbaQ is local dictation for Linux. Hold a key, speak, release, and the text is
typed into the focused application. It runs offline after the model download.
There is no window, tray icon or notification. Feedback is sound and a terminal
settings screen.

## Scope

- Linux x86_64. Wayland and X11.
- Rust, one binary, no runtime interpreter.
- Speech recognition uses whisper.cpp through `whisper-rs`.
- No cloud, no account, no telemetry, no update checks.
- The TUI is the only interface. Do not add a GUI toolkit.
- Keep the package clean: one binary, desktop entry, icon set, udev rule,
  modules-load entry and documentation. No service is enabled by the package.
- The package manager owns system files. User files live only in the XDG
  directories and the optional autostart file.

## Layout

```
AGENTS.md              rules for this repository
README.md              user and release documentation
RELEASE.md             release checklist
CHANGELOG.md           release history
VERSION                current release version
LICENSE                PolyForm Noncommercial 1.0.0
Cargo.toml             crate metadata and dependencies
src/                   application code
assets/                desktop entry
assets/icons/          icon master and PNG exports
packaging/             package definitions and shared package files
scripts/               build, package and check scripts
dist/                  binary, packages and checksums, ignored
target/                Cargo output, ignored
```

- Keep generated output out of git.
- Keep one source of truth for version, package names and descriptions.
- `packaging/nfpm.yaml` installs only the files listed above.
- `packaging/` holds declarative package data. `scripts/` holds executables.
- nfpm covers deb, rpm and Arch in one file. Do not split it into empty
  per-format folders.

## Build and test

```sh
cargo build --release
cargo test --release
cargo clippy --release -- -D warnings
cargo fmt --check
```

- Desktop-dependent tests are `#[ignore]` by default. Run them only on a real
  session with `cargo test --release -- --ignored`.
- `scripts/build-release.sh` builds the binary and every package format.
- Validate metadata with `desktop-file-validate assets/verbaq.desktop`.
- Keep the package version in `VERSION`, not in the script.

## Reliability rules

- One recording or transcription at a time.
- The shortcut must not interrupt an active transcription.
- Never block the daemon on an external command. Clipboard, sound and desktop
  queries need a timeout.
- Close a child process stdin before waiting. Tools such as `wl-copy` and
  `pw-cat` read until EOF.
- Do not overwrite non-text clipboard content.
- A failed paste must not claim the text is on the clipboard unless the write
  succeeded.
- A model change must reload the model on the next recording.
- Every user setting must actually affect the daemon.
- Always return to idle after an error.
- Log to `~/.local/state/verbaq/verbaq.log`. Do not log transcripts unless
  history is enabled.

## TUI rules

- Monochrome base with one cyan accent. No emoji.
- Use glyphs only when they survive a narrow terminal.
- Every screen starts with one plain sentence describing it.
- Show section keys in the footer and full help behind `?`.
- Destructive actions require a second Enter.
- Long sections scroll with PgUp and PgDn.
- Never hide a failed save. Show it in red.
- Keep the status header: service, model, shortcut, mode.

## Packaging and release

- Package names are `verbaq`. Display name is VerbaQ.
- Runtime dependencies: `libasound2`, `libstdc++6`, `libgcc-s1`, `libc6` on
  Debian; `alsa-lib`, `libstdc++`, `libgcc`, `glibc` on RPM systems;
  `alsa-lib`, `gcc-libs`, `glibc` on Arch.
- Only the latest package set is kept in `dist/`. Before building a new
  version, the previous deb, rpm and Arch package must already be archived in
  `TTP-pkgs/pkgs/verbaq/<version>/`. Release history goes there after owner
  approval.
- Public repository: `TTP-VerbaQ`.
- See `RELEASE.md` for the release checklist.
