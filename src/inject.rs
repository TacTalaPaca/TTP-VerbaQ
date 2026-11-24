use anyhow::{anyhow, bail, Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMethod {
    ShiftInsert,
    CtrlV,
}

impl PasteMethod {
    pub fn from_str(s: &str) -> Self {
        if s == "ctrl_v" {
            Self::CtrlV
        } else {
            Self::ShiftInsert
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clipboard {
    WlCopy,
    Xclip,
    Xsel,
    None,
}

fn clipboard_backend() -> Clipboard {
    if util::is_wayland() && util::which("wl-copy").is_some() && util::which("wl-paste").is_some() {
        return Clipboard::WlCopy;
    }
    if util::which("xclip").is_some() {
        return Clipboard::Xclip;
    }
    if util::which("xsel").is_some() {
        return Clipboard::Xsel;
    }
    if util::which("wl-copy").is_some() && util::which("wl-paste").is_some() {
        return Clipboard::WlCopy;
    }
    Clipboard::None
}

pub fn clipboard_available() -> bool {
    clipboard_backend() != Clipboard::None
}

pub fn read_clipboard() -> Option<String> {
    let (cmd, args): (&str, &[&str]) = match clipboard_backend() {
        Clipboard::WlCopy => ("wl-paste", &["--no-newline"]),
        Clipboard::Xclip => ("xclip", &["-o", "-selection", "clipboard"]),
        Clipboard::Xsel => ("xsel", &["-b", "-o"]),
        Clipboard::None => return None,
    };
    let mut command = Command::new(cmd);
    command.args(args);
    let out = util::run_with_timeout(&mut command, Duration::from_secs(2)).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    if text.is_empty() || text.len() > 1_000_000 {
        None
    } else {
        Some(text)
    }
}

pub fn write_clipboard(text: &str) -> Result<()> {
    let (cmd, args): (&str, &[&str]) = match clipboard_backend() {
        Clipboard::WlCopy => ("wl-copy", &[]),
        Clipboard::Xclip => ("xclip", &["-i", "-selection", "clipboard"]),
        Clipboard::Xsel => ("xsel", &["-b", "-i"]),
        Clipboard::None => {
            bail!("no clipboard tool found. Install wl-clipboard (Wayland) or xclip (X11).")
        }
    };
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot start {cmd}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = util::wait_for_process(&mut child, Duration::from_secs(3))?;
    if !status.success() {
        bail!("{cmd} exited with {status}");
    }
    Ok(())
}

fn virtual_keyboard() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in [29u16, 42, 47, 110] {
        keys.insert(KeyCode::new(code));
    }
    let device = VirtualDevice::builder()?
        .name("verbaq virtual keyboard")
        .with_keys(&keys)?
        .build()
        .map_err(|e| {
            anyhow!(
                "cannot open /dev/uinput ({e}). The package installs a udev rule for this; \
                 log out and back in if it was just installed."
            )
        })?;
    Ok(device)
}

fn tap(device: &mut VirtualDevice, codes: &[(u16, i32)]) -> Result<()> {
    for (code, value) in codes {
        device.emit(&[InputEvent::new(EventType::KEY.0, *code, *value)])?;
        std::thread::sleep(Duration::from_millis(18));
    }
    Ok(())
}

pub fn send_paste(method: PasteMethod) -> Result<()> {
    let mut device = virtual_keyboard()?;
    // Give the desktop a moment to notice the new virtual keyboard.
    std::thread::sleep(Duration::from_millis(150));
    match method {
        PasteMethod::ShiftInsert => {
            tap(&mut device, &[(42, 1), (110, 1), (110, 0), (42, 0)])?;
        }
        PasteMethod::CtrlV => {
            tap(&mut device, &[(29, 1), (47, 1), (47, 0), (29, 0)])?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipboardState {
    Empty,
    Text,
    Unsupported,
    Unknown,
}

fn clipboard_is_text(type_name: &str) -> bool {
    type_name.starts_with("text/")
        || matches!(
            type_name,
            "UTF8_STRING" | "STRING" | "TEXT" | "COMPOUND_TEXT"
        )
}

fn clipboard_state() -> ClipboardState {
    let backend = clipboard_backend();
    if backend == Clipboard::None {
        return ClipboardState::Empty;
    }
    if backend != Clipboard::WlCopy {
        return ClipboardState::Unknown;
    }
    let mut command = Command::new("wl-paste");
    command.arg("--list-types");
    let Ok(output) = util::run_with_timeout(&mut command, Duration::from_secs(1)) else {
        return ClipboardState::Unknown;
    };
    if !output.status.success() {
        return ClipboardState::Empty;
    }
    let types: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if types.is_empty() {
        ClipboardState::Empty
    } else if types.iter().all(|name| clipboard_is_text(name)) {
        ClipboardState::Text
    } else {
        ClipboardState::Unsupported
    }
}

pub enum InsertResult {
    Inserted,
    Copied,
    Blocked,
}

/// Put text on the clipboard, paste it, then restore the previous clipboard.
pub fn insert_text(text: &str, method: PasteMethod, restore_ms: u64) -> Result<InsertResult> {
    if text.trim().is_empty() {
        return Ok(InsertResult::Copied);
    }
    if !clipboard_available() {
        bail!("no clipboard tool found. Install wl-clipboard (Wayland) or xclip (X11).");
    }
    let state = clipboard_state();
    if state == ClipboardState::Unsupported {
        return Ok(InsertResult::Blocked);
    }
    let previous = read_clipboard();
    if previous.is_none() && state == ClipboardState::Text {
        // Text was offered but could not be read as UTF-8, so it cannot be restored safely.
        return Ok(InsertResult::Blocked);
    }
    write_clipboard(text)?;
    std::thread::sleep(Duration::from_millis(60));

    if let Err(err) = send_paste(method) {
        eprintln!("verbaq: paste failed: {err}");
        return Ok(InsertResult::Copied);
    }

    std::thread::sleep(Duration::from_millis(restore_ms));
    if let Some(prev) = previous {
        let _ = write_clipboard(&prev);
    }
    Ok(InsertResult::Inserted)
}

pub fn copy_only(text: &str) -> bool {
    write_clipboard(text).is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires /dev/uinput"]
    fn virtual_keyboard_can_be_created() {
        let device = super::virtual_keyboard();
        assert!(device.is_ok(), "uinput unavailable: {:?}", device.err());
    }

    #[test]
    #[ignore = "requires a desktop session and temporarily changes the clipboard"]
    fn clipboard_roundtrip() {
        if !super::clipboard_available() {
            return;
        }
        let previous = super::read_clipboard();
        assert!(super::write_clipboard("verbaq clipboard test").is_ok());
        assert_eq!(
            super::read_clipboard().as_deref(),
            Some("verbaq clipboard test")
        );
        if let Some(previous) = previous {
            let _ = super::write_clipboard(&previous);
        }
    }
}
