# VerbaQ

Local dictation for Linux. Hold a key, speak, release, and the text is typed
into the application you were already using.

Everything runs on this machine. No account, no cloud, no network after the
model download. There is no window, tray icon or notification. A few short
sounds confirm what is happening, and the settings screen lives in the terminal.

- Version: 1.2
- Licence: PolyForm Noncommercial 1.0.0
- Engine: whisper.cpp through `whisper-rs`
- Repository: `TTP-VerbaQ`

## Features

- Local speech recognition with Whisper Small or Turbo.
- Hold to talk, or press once to start and stop.
- Global shortcut with a conflict scan of the desktop's own shortcuts.
- Text inserted through the clipboard and a virtual keyboard, then the previous
  clipboard is restored.
- Sound cues only. No notifications, because desktops hide them.
- One recording or transcription at a time. The shortcut cannot interrupt work.
- Terminal settings screen with a service screen, model manager, microphone
  meter, shortcut picker, history and health check.
- Arch, Debian and RPM packages from one build script.
- No service enabled by the package. Start at login is one file you control.

## Requirements

- Linux x86_64
- PipeWire or PulseAudio
- Wayland or X11
- `wl-clipboard` on Wayland, `xclip` or `xsel` on X11
- About 200 MB of disk for the app and the Small model

## Install

Arch and derivatives:

```sh
sudo pacman -U verbaq-1.2-1-x86_64.pkg.tar.zst
```

Debian and Ubuntu:

```sh
sudo apt install ./verbaq_1.2_amd64.deb
```

Fedora and other RPM systems:

```sh
sudo dnf install ./verbaq-1.2-1.x86_64.rpm
```

The package installs the binary, a menu entry, icons, one udev rule and one
modules-load entry. It starts nothing and changes no user setting.

Log out and back in once after installing so the udev rule applies to your
session. This is what lets VerbaQ type for you and read the shortcut.

## First run

```sh
verbaq setup
```

Seven explained steps: voice model, microphone with a three second level test,
shortcut with free and used suggestions, hold or toggle mode, a test sentence,
history, sound and startup. The model download is verified against a checksum.

## Turning it on and off

There is one background service. It waits for the shortcut. Without it the
shortcut does nothing.

```sh
verbaq start     # start the service now
verbaq stop      # stop it now
verbaq status    # running or stopped, pid, state, start at login
```

`verbaq daemon` runs the service in the current terminal for debugging. It
prints one line per event and stops with Ctrl+C.

Start at login is one file:

```
~/.config/autostart/verbaq.desktop
```

Toggle it from the Service screen in `verbaq tui`, or delete the file.

## Daily use

Press the shortcut, speak, release.

- Hold to talk: keep the key down while speaking. Default.
- Press to start, press again to stop: hands free.

A rising tick means recording started. A short click means it stopped and
transcription began. Two low beeps mean a problem, and the text is left on the
clipboard when possible.

## Shortcut

Default: `Ctrl+Alt+D`.

Other suggestions: `Ctrl+Alt+S`, `Ctrl+Alt+Space`, `Ctrl+Alt+A`,
`Ctrl+Alt+Z`, `Super+Shift+D`.

VerbaQ reads shortcuts from KDE, GNOME, sway, i3 and Hyprland and marks each
suggestion free or used. It cannot see shortcuts owned by individual
applications, so a free mark is a strong hint, not a guarantee.

Custom capture is available. Key names follow the US layout. Esc or Ctrl+C
cancels capture.

If the built-in shortcut is unavailable, bind this command in the desktop:

```
verbaq toggle
```

- KDE: System Settings > Shortcuts > Add > Command or script
- GNOME: Settings > Keyboard > Custom Shortcuts
- sway: `bindsym $mod+d exec verbaq toggle`
- Hyprland: `bind = SUPER SHIFT, D, exec, verbaq toggle`

## Models

| Name | Size | Notes |
| --- | --- | --- |
| Small | 181 MB | default, fastest, best English |
| Turbo | 547 MB | more accurate, about 3x slower on CPU |

```sh
verbaq model install small
verbaq model install turbo
verbaq model list
```

Models live in `~/.local/share/verbaq/models`. Downloads are verified against
SHA-256 and the exact file size.

Measured on a Ryzen 5 5600, CPU only, model warm: about 2.3 seconds per
utterance with Small. Whisper always processes a fixed 30 second window, so a
short sentence costs the same as a long one. Turbo is about three times slower.

This build is CPU only. A Vulkan build is a later step.

## Settings

```sh
verbaq tui
```

- Service: running, pid, state, start and stop, start at login
- Microphone: device list, live level meter, three second test
- Voice model: install, select, delete, download progress
- Shortcut: mode, suggestions, custom capture, full desktop shortcut list
- Sound: what each cue means, on or off, test
- Text insertion: paste method and a live check
- History: on or off, recent entries, clear
- Health: every dependency with a plain result

Press `?` for the key map. PgUp and PgDn scroll long sections.

## History

History is a local copy of the last transcripts, so text can be recovered when
a paste failed.

- File: `~/.local/state/verbaq/history.jsonl`
- Off by default, enabled in setup or the History screen
- Default limit 200 entries
- Never uploaded, cleared with Enter twice

## Text insertion

VerbaQ copies the text, presses a paste key, then restores the previous
clipboard.

- `Shift+Insert` is the default. It works in terminals and normal apps.
- `Ctrl+V` is the alternative.
- Non-text clipboard content is not overwritten. In that case the text is not
  inserted, and the history keeps it when enabled.
- If pasting fails, the text stays on the clipboard.

## Reliability and limits

- One recording or transcription at a time.
- Busy presses are ignored with a soft tick.
- A lost key release stops recording after two minutes.
- A hung transcription times out after three minutes and returns to idle.
- A disconnected microphone stops the recording with an error.
- Clipboard and sound helpers have timeouts and close stdin before waiting.
- Model changes reload the model on the next recording.
- Failed settings writes are shown in red.

## Privacy

- No network after setup.
- No telemetry, accounts or update checks.
- Audio is held in memory and never written to disk.
- Text goes to the clipboard, is pasted, and the previous clipboard is restored.
- Local data: the model, config, optional history and the log.

## Project layout

```
AGENTS.md              repository rules
README.md              this file
RELEASE.md             release checklist
CHANGELOG.md           release history
VERSION                release version
LICENSE                PolyForm Noncommercial 1.0.0
Cargo.toml             crate metadata
src/                   application code
assets/                desktop entry
assets/icons/          icon master and PNG exports
packaging/             package definitions and shared package files
  nfpm.yaml            deb, rpm and Arch definition
  common/              udev rule, modules entry, install scripts
scripts/               build, check and release scripts
dist/                  binary, packages and checksums, ignored
target/                Cargo output, ignored
```

## Build from source

```sh
cargo build --release
scripts/check-commit.sh
scripts/build-release.sh
```

The binary and packages go to `dist/`. Only the latest set is kept in this
repository. Release history is stored in `TTP-pkgs` after owner approval.

## Test it

```sh
verbaq doctor
verbaq setup
verbaq start
verbaq status
```

Then open a text editor, press the shortcut, say a sentence and release.

Tests:

```sh
cargo test --release
cargo test --release -- --ignored   # real uinput and clipboard
```

## Release

See [RELEASE.md](RELEASE.md). Version 1.1. Commit subjects use
`VerbaQ <version>: <summary>`. No commit, tag, push or public release without
owner confirmation.

## Licence

PolyForm Noncommercial 1.0.0. See [LICENSE](LICENSE).

Whisper models and whisper.cpp are MIT licensed.

## Credits

Made by Danuja from TacTalaPaca.

- Whisper by OpenAI
- whisper.cpp and ggml
- cpal, ratatui and the Rust ecosystem
