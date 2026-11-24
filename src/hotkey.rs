use anyhow::{bail, Result};
use evdev::{Device, EventType, KeyCode};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CTRL_LEFT: u16 = 29;
const CTRL_RIGHT: u16 = 97;
const SHIFT_LEFT: u16 = 42;
const SHIFT_RIGHT: u16 = 54;
const ALT_LEFT: u16 = 56;
const ALT_RIGHT: u16 = 100;
const SUPER_LEFT: u16 = 125;
const SUPER_RIGHT: u16 = 126;

type KeyChannel = (Sender<(u16, i32)>, Receiver<(u16, i32)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyEvent {
    Start,
    Stop,
    Toggle,
    Cancel,
}

fn device_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/input") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("event") {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

fn is_keyboard(device: &Device) -> bool {
    device
        .supported_keys()
        .map(|keys| keys.contains(KeyCode::KEY_A))
        .unwrap_or(false)
}

fn open_keyboards() -> Vec<(PathBuf, Device)> {
    let mut devices = Vec::new();
    for path in device_paths() {
        if let Ok(device) = Device::open(&path) {
            let name = device.name().unwrap_or_default();
            if is_keyboard(&device) && !name.contains("verbaq") {
                devices.push((path, device));
            }
        }
    }
    devices
}

pub fn readable_keyboards() -> usize {
    open_keyboards().len()
}

pub fn permission_available() -> bool {
    readable_keyboards() > 0
}

/// Map right-hand modifiers onto their left-hand twin so a shortcut works
/// with either key.
pub fn normalize_code(code: u16) -> u16 {
    match code {
        CTRL_RIGHT => CTRL_LEFT,
        SHIFT_RIGHT => SHIFT_LEFT,
        ALT_RIGHT => ALT_LEFT,
        SUPER_RIGHT => SUPER_LEFT,
        other => other,
    }
}

pub fn normalize_codes(codes: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for code in codes {
        let code = normalize_code(*code);
        if !out.contains(&code) {
            out.push(code);
        }
    }
    out.sort_by_key(|code| (modifier_order(*code), *code));
    out
}

fn modifier_order(code: u16) -> u8 {
    match code {
        CTRL_LEFT => 0,
        ALT_LEFT => 1,
        SHIFT_LEFT => 2,
        SUPER_LEFT => 3,
        _ => 4,
    }
}

pub fn is_modifier(code: u16) -> bool {
    matches!(
        normalize_code(code),
        CTRL_LEFT | ALT_LEFT | SHIFT_LEFT | SUPER_LEFT
    )
}

fn twin(code: u16) -> Option<u16> {
    match code {
        CTRL_LEFT => Some(CTRL_RIGHT),
        CTRL_RIGHT => Some(CTRL_LEFT),
        SHIFT_LEFT => Some(SHIFT_RIGHT),
        SHIFT_RIGHT => Some(SHIFT_LEFT),
        ALT_LEFT => Some(ALT_RIGHT),
        ALT_RIGHT => Some(ALT_LEFT),
        SUPER_LEFT => Some(SUPER_RIGHT),
        SUPER_RIGHT => Some(SUPER_LEFT),
        _ => None,
    }
}

fn pressed_contains(pressed: &HashSet<u16>, code: u16) -> bool {
    pressed.contains(&code) || twin(code).map(|t| pressed.contains(&t)).unwrap_or(false)
}

fn all_pressed(pressed: &HashSet<u16>, required: &[u16]) -> bool {
    required.iter().all(|code| pressed_contains(pressed, *code))
}

/// Correct Linux key names. The letter rows are not contiguous, so this is an
/// explicit table rather than arithmetic.
pub fn key_name(code: u16) -> String {
    match code {
        1 => "Esc",
        2 => "1",
        3 => "2",
        4 => "3",
        5 => "4",
        6 => "5",
        7 => "6",
        8 => "7",
        9 => "8",
        10 => "9",
        11 => "0",
        12 => "-",
        13 => "=",
        14 => "Backspace",
        15 => "Tab",
        16 => "Q",
        17 => "W",
        18 => "E",
        19 => "R",
        20 => "T",
        21 => "Y",
        22 => "U",
        23 => "I",
        24 => "O",
        25 => "P",
        26 => "[",
        27 => "]",
        28 => "Enter",
        29 => "Ctrl",
        30 => "A",
        31 => "S",
        32 => "D",
        33 => "F",
        34 => "G",
        35 => "H",
        36 => "J",
        37 => "K",
        38 => "L",
        39 => ";",
        40 => "'",
        41 => "`",
        42 => "Shift",
        43 => "\\",
        44 => "Z",
        45 => "X",
        46 => "C",
        47 => "V",
        48 => "B",
        49 => "N",
        50 => "M",
        51 => ",",
        52 => ".",
        53 => "/",
        54 => "Shift",
        55 => "*",
        56 => "Alt",
        57 => "Space",
        58 => "CapsLock",
        59 => "F1",
        60 => "F2",
        61 => "F3",
        62 => "F4",
        63 => "F5",
        64 => "F6",
        65 => "F7",
        66 => "F8",
        67 => "F9",
        68 => "F10",
        86 => "<",
        87 => "F11",
        88 => "F12",
        97 => "Ctrl",
        100 => "Alt",
        102 => "Home",
        103 => "Up",
        104 => "PageUp",
        105 => "Left",
        106 => "Right",
        107 => "End",
        108 => "Down",
        109 => "PageDown",
        110 => "Insert",
        111 => "Delete",
        125 => "Super",
        126 => "Super",
        _ => return format!("Key{code}"),
    }
    .to_string()
}

pub fn combo_label(codes: &[u16]) -> String {
    let codes = normalize_codes(codes);
    if codes.is_empty() {
        return "unset".into();
    }
    codes
        .iter()
        .map(|code| key_name(*code))
        .collect::<Vec<_>>()
        .join("+")
}

struct CaptureStop(Arc<AtomicBool>);

impl Drop for CaptureStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Wait until the keyboard is quiet, then return the next combination that
/// includes at least one modifier. Reader threads are non-blocking and stop as
/// soon as this function returns, so repeated captures do not leak threads.
pub fn capture() -> Result<Vec<u16>> {
    let devices = open_keyboards();
    if devices.is_empty() {
        bail!("no readable keyboard found");
    }
    let (tx, rx): KeyChannel = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = CaptureStop(stop.clone());
    let mut readers = 0usize;

    for (_, mut device) in devices {
        if device.set_nonblocking(true).is_err() {
            continue;
        }
        readers += 1;
        let tx = tx.clone();
        let stop = stop.clone();
        std::thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        if event.event_type() == EventType::KEY {
                            let _ = tx.send((event.code(), event.value()));
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
            std::thread::sleep(Duration::from_millis(5));
        });
    }
    if readers == 0 {
        bail!("keyboards could not be opened in non-blocking mode");
    }

    // Ignore whatever is still happening from the key that started the capture,
    // usually Enter from the terminal.
    let settle = Instant::now() + Duration::from_millis(350);
    while Instant::now() < settle {
        let _ = rx.recv_timeout(Duration::from_millis(50));
    }

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut pressed: HashSet<u16> = HashSet::new();
    while Instant::now() < deadline {
        let Ok((code, value)) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if value == 1 {
            pressed.insert(code);
            // Escape and Ctrl+C cancel capture instead of becoming shortcuts.
            if code == 1 || (code == 46 && pressed_contains(&pressed, CTRL_LEFT)) {
                bail!("cancelled");
            }
        }
        match value {
            1 => {
                if !is_modifier(code) {
                    if !pressed.iter().any(|c| is_modifier(*c)) {
                        // A bare key is not a usable global shortcut.
                        pressed.clear();
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(60));
                    let codes: Vec<u16> = pressed.iter().copied().collect();
                    pressed.clear();
                    return Ok(normalize_codes(&codes));
                }
            }
            0 => {
                pressed.remove(&code);
            }
            _ => {}
        }
    }
    bail!("no combination was pressed")
}

#[derive(Debug, Clone)]
pub struct ListenerConfig {
    pub codes: Vec<u16>,
    pub hold_to_talk: bool,
}

/// Watch every keyboard for one combination. Rescans for devices so a
/// reconnect or a newly plugged keyboard does not kill the shortcut.
/// The combination is read from shared state, so changing it in the settings
/// screen takes effect without restarting the service.
pub fn spawn_listener(config: Arc<Mutex<ListenerConfig>>, tx: Sender<HotkeyEvent>) {
    std::thread::spawn(move || {
        let active: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let mut warned = false;
        loop {
            let devices = open_keyboards();
            if devices.is_empty() && !warned {
                eprintln!(
                    "verbaq: no readable keyboard. Bind `verbaq toggle` in your desktop, \
                     or install the package udev rule and log out and back in."
                );
                warned = true;
            }
            if !devices.is_empty() {
                warned = false;
            }
            for (path, device) in devices {
                let mut guard = match active.lock() {
                    Ok(guard) => guard,
                    Err(_) => return,
                };
                if !guard.insert(path.clone()) {
                    continue;
                }
                drop(guard);
                spawn_reader(path, device, config.clone(), tx.clone(), active.clone());
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

fn spawn_reader(
    path: PathBuf,
    mut device: Device,
    config: Arc<Mutex<ListenerConfig>>,
    tx: Sender<HotkeyEvent>,
    active: Arc<Mutex<HashSet<PathBuf>>>,
) {
    std::thread::spawn(move || {
        let mut pressed: HashSet<u16> = HashSet::new();
        let mut triggered = false;
        while let Ok(events) = device.fetch_events() {
            for event in events {
                if event.event_type() != EventType::KEY {
                    continue;
                }
                let (required, hold_to_talk) = match config.lock() {
                    Ok(cfg) => (normalize_codes(&cfg.codes), cfg.hold_to_talk),
                    Err(_) => return,
                };
                if required.is_empty() {
                    triggered = false;
                    continue;
                }
                let code = event.code();
                match event.value() {
                    1 => {
                        pressed.insert(code);
                        if !triggered && all_pressed(&pressed, &required) {
                            triggered = true;
                            let _ = if hold_to_talk {
                                tx.send(HotkeyEvent::Start)
                            } else {
                                tx.send(HotkeyEvent::Toggle)
                            };
                        }
                    }
                    0 => {
                        pressed.remove(&code);
                        if triggered && hold_to_talk && !all_pressed(&pressed, &required) {
                            triggered = false;
                            let _ = tx.send(HotkeyEvent::Stop);
                        }
                        if !hold_to_talk && pressed.is_empty() {
                            triggered = false;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Ok(mut guard) = active.lock() {
            guard.remove(&path);
        }
    });
}

#[allow(dead_code)]
fn is_keyboard_path(path: &Path) -> bool {
    Device::open(path).map(|d| is_keyboard(&d)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_physical_keys() {
        assert_eq!(key_name(30), "A");
        assert_eq!(key_name(31), "S");
        assert_eq!(key_name(32), "D");
        assert_eq!(key_name(33), "F");
        assert_eq!(key_name(46), "C");
        assert_eq!(key_name(47), "V");
        assert_eq!(key_name(48), "B");
        assert_eq!(key_name(57), "Space");
    }

    #[test]
    fn right_modifiers_collapse() {
        assert_eq!(normalize_codes(&[97, 54, 32]), vec![29, 42, 32]);
        assert_eq!(combo_label(&[29, 42, 32]), "Ctrl+Shift+D");
        assert_eq!(combo_label(&[125, 42, 32]), "Shift+Super+D");
    }

    #[test]
    fn combo_order_is_canonical() {
        assert_eq!(combo_label(&[32, 56, 29]), "Ctrl+Alt+D");
    }
}
