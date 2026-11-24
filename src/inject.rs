use anyhow::{anyhow, bail, Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

fn clipboard_is_text(type_name: &str) -> bool {
    (type_name.starts_with("text/") && type_name != "text/uri-list")
        || matches!(
            type_name,
            "UTF8_STRING" | "STRING" | "TEXT" | "COMPOUND_TEXT"
        )
}

fn parse_types(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn wayland_types() -> Option<Vec<String>> {
    let mut command = Command::new("wl-paste");
    command.arg("--list-types");
    let output = util::run_with_timeout(&mut command, Duration::from_secs(1)).ok()?;
    if !output.status.success() {
        return Some(Vec::new());
    }
    Some(parse_types(&output.stdout))
}

fn x11_types() -> Option<Vec<String>> {
    let mut command = Command::new("xclip");
    command.args(["-o", "-selection", "clipboard", "-t", "TARGETS"]);
    let output = util::run_with_timeout(&mut command, Duration::from_secs(1)).ok()?;
    if !output.status.success() {
        return Some(Vec::new());
    }
    Some(parse_types(&output.stdout))
}

fn choose_type(types: &[String]) -> Option<&str> {
    const PRIORITY: &[&str] = &[
        "text/uri-list",
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
        "application/x-qt-image",
    ];
    for wanted in PRIORITY {
        if let Some(name) = types.iter().find(|name| name.as_str() == *wanted) {
            return Some(name);
        }
    }
    types
        .iter()
        .find(|name| {
            !clipboard_is_text(name)
                && !name.starts_with("x-kde-")
                && !name.starts_with("application/x-kde")
        })
        .map(String::as_str)
}

fn read_clipboard_type(type_name: &str) -> Option<Vec<u8>> {
    let mut command = match clipboard_backend() {
        Clipboard::WlCopy => {
            let mut command = Command::new("wl-paste");
            command.args(["--type", type_name]);
            command
        }
        Clipboard::Xclip => {
            let mut command = Command::new("xclip");
            command.args(["-o", "-selection", "clipboard", "-t", type_name]);
            command
        }
        _ => return None,
    };
    let output = util::run_with_timeout(&mut command, Duration::from_secs(3)).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

fn restore_clipboard_type(type_name: &str, path: &PathBuf) -> bool {
    let mut command = match clipboard_backend() {
        Clipboard::WlCopy => {
            let mut command = Command::new("wl-copy");
            command.args(["--type", type_name]);
            command
        }
        Clipboard::Xclip => {
            let mut command = Command::new("xclip");
            command.args(["-i", "-selection", "clipboard", "-t", type_name]);
            command
        }
        _ => return false,
    };
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut child) = command
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    matches!(
        util::wait_for_process(&mut child, Duration::from_secs(3)),
        Ok(status) if status.success()
    )
}

static CLIP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let sequence = CLIP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("verbaq-clip-{}-{sequence}.bin", std::process::id()))
}

enum Backup {
    None,
    Text(Option<String>),
    Data { type_name: String, path: PathBuf },
}

impl Backup {
    fn restore(&self) {
        match self {
            Backup::None | Backup::Text(None) => {}
            Backup::Text(Some(text)) => {
                if write_clipboard(text).is_err() {
                    crate::logln!("could not restore the text clipboard");
                }
            }
            Backup::Data { type_name, path } => {
                if !restore_clipboard_type(type_name, path) {
                    crate::logln!("could not restore the {type_name} clipboard");
                }
            }
        }
    }
}

impl Drop for Backup {
    fn drop(&mut self) {
        if let Backup::Data { path, .. } = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

enum Capture {
    Ready(Backup),
    Blocked,
}

fn capture_clipboard() -> Capture {
    let types = match clipboard_backend() {
        Clipboard::WlCopy => wayland_types(),
        Clipboard::Xclip => x11_types(),
        _ => None,
    };
    let Some(types) = types else {
        // Without a type list, only plain text can be restored safely.
        return Capture::Ready(Backup::Text(read_clipboard()));
    };
    if types.is_empty() {
        return Capture::Ready(Backup::None);
    }
    if types.iter().all(|name| clipboard_is_text(name)) {
        return match read_clipboard() {
            Some(text) => Capture::Ready(Backup::Text(Some(text))),
            None => Capture::Blocked,
        };
    }
    let Some(type_name) = choose_type(&types) else {
        return Capture::Blocked;
    };
    let Some(bytes) = read_clipboard_type(type_name) else {
        return Capture::Blocked;
    };
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Capture::Blocked;
    }
    let path = temp_path();
    if std::fs::write(&path, &bytes).is_err() {
        return Capture::Blocked;
    }
    Capture::Ready(Backup::Data {
        type_name: type_name.to_string(),
        path,
    })
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
    let backup = match capture_clipboard() {
        Capture::Blocked => return Ok(InsertResult::Blocked),
        Capture::Ready(backup) => backup,
    };
    write_clipboard(text)?;
    std::thread::sleep(Duration::from_millis(60));

    if let Err(err) = send_paste(method) {
        eprintln!("verbaq: paste failed: {err}");
        return Ok(InsertResult::Copied);
    }

    std::thread::sleep(Duration::from_millis(restore_ms));
    backup.restore();
    Ok(InsertResult::Inserted)
}

pub fn copy_only(text: &str) -> bool {
    write_clipboard(text).is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn prefers_file_list_then_images() {
        let types = vec![
            "text/plain".to_string(),
            "x-kde-force-image-copy".to_string(),
            "image/png".to_string(),
        ];
        assert_eq!(super::choose_type(&types), Some("image/png"));

        let files = vec!["text/uri-list".to_string(), "image/png".to_string()];
        assert_eq!(super::choose_type(&files), Some("text/uri-list"));

        let markers = vec![
            "text/plain".to_string(),
            "x-kde-force-image-copy".to_string(),
        ];
        assert_eq!(super::choose_type(&markers), None);
    }

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

    #[test]
    #[ignore = "requires a Wayland session and temporarily changes the clipboard"]
    fn non_text_clipboard_is_restored() {
        if super::clipboard_backend() != super::Clipboard::WlCopy {
            return;
        }
        let Some(types) = super::wayland_types() else {
            return;
        };
        let Some(type_name) = super::choose_type(&types) else {
            return;
        };
        if super::clipboard_is_text(type_name) {
            return;
        }
        let before = super::read_clipboard_type(type_name).expect("read original clipboard");
        let backup = match super::capture_clipboard() {
            super::Capture::Ready(backup) => backup,
            super::Capture::Blocked => panic!("non-text clipboard could not be saved"),
        };
        assert!(super::write_clipboard("verbaq clipboard test").is_ok());
        backup.restore();
        let after = super::read_clipboard_type(type_name).expect("read restored clipboard");
        assert_eq!(before, after, "clipboard was not restored");
    }
}
