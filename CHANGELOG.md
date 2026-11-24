# Changelog

## 1.1

- First release.
- Local dictation with whisper.cpp and Whisper Small as the default model.
- Optional Turbo model.
- Hold to talk or press to toggle, switchable at any time.
- Global shortcut with free/used suggestions read from KDE, GNOME, sway, i3
  and Hyprland, custom capture, and a full list of known shortcuts.
- Text insertion by clipboard and a virtual keyboard.
- Sound cues only. Notifications removed because desktops hide them.
- New monochrome waveform icon, installed as a new icon name so desktop caches cannot show the old one.
- Strict single-transcription state machine: busy presses are ignored, long
  recordings stop at the limit, hung transcriptions time out.
- Terminal settings screen with a Service screen for start, stop and start at
  login.
- First-run wizard with explanations for every step.
- The setup test shows the transcript and waits for Enter. No judgement prompt.
- Health check with shortcut conflict detection.
- Visual settings screen with a status header, icons, level and download bars, help overlay, scrolling and destructive-action confirmations.
- Arch, Debian and RPM packages with the C++ runtime dependencies declared.
- Clean package metadata: no directory permission warnings, refreshed desktop entry with service actions.
- Reliability: model reload on change, sound setting honored by the daemon, clipboard safety for non-text data, bounded external commands, cancellation with job generations, microphone error handling, audio fallback and a level-only meter.
