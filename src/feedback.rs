use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::util;

#[derive(Debug, Clone, Copy)]
pub enum Cue {
    /// Recording started.
    Start,
    /// Recording ended, transcription is running.
    Stop,
    /// The shortcut was pressed while busy. Ignored.
    Busy,
    /// Something failed.
    Error,
    /// A test from the settings screen.
    Test,
}

impl Cue {
    fn file(self) -> &'static str {
        match self {
            Cue::Start => "start.wav",
            Cue::Stop => "stop.wav",
            Cue::Busy => "busy.wav",
            Cue::Error => "error.wav",
            Cue::Test => "test.wav",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub description: String,
    pub state: String,
    pub default: bool,
}

pub const SETS: &[(&str, &str)] = &[("material", "Material"), ("minimal", "Minimal")];

pub fn set_label(id: &str) -> &'static str {
    SETS.iter()
        .find(|(set, _)| *set == id)
        .map(|(_, label)| *label)
        .unwrap_or("Material")
}

pub fn valid_set(id: &str) -> bool {
    SETS.iter().any(|(set, _)| *set == id)
}

static LAST_PLAYED: AtomicU64 = AtomicU64::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn pactl(args: &[&str]) -> Option<String> {
    util::which("pactl")?;
    let mut command = Command::new("pactl");
    command.args(args);
    let output = util::run_with_timeout(&mut command, Duration::from_millis(800)).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn output_devices() -> Vec<Output> {
    let default = pactl(&["get-default-sink"]).map(|name| name.trim().to_string());
    let Some(text) = pactl(&["list", "sinks"]) else {
        return Vec::new();
    };
    let mut outputs: Vec<Output> = Vec::new();
    let mut current = Output {
        name: String::new(),
        description: String::new(),
        state: String::new(),
        default: false,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Sink #") {
            if !current.name.is_empty() {
                outputs.push(current);
            }
            current = Output {
                name: String::new(),
                description: String::new(),
                state: String::new(),
                default: false,
            };
        } else if let Some(value) = line.strip_prefix("Name: ") {
            current.name = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("Description: ") {
            current.description = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("State: ") {
            current.state = value.trim().to_string();
        }
    }
    if !current.name.is_empty() {
        outputs.push(current);
    }
    if let Some(default) = default {
        for output in &mut outputs {
            output.default = output.name == default;
        }
    }
    outputs
}

fn resume_sink(device: Option<&str>) {
    let sink = match device.filter(|name| !name.is_empty()) {
        Some(name) => name.to_string(),
        None => pactl(&["get-default-sink"])
            .map(|name| name.trim().to_string())
            .unwrap_or_default(),
    };
    if sink.is_empty() || util::which("pactl").is_none() {
        return;
    }
    let mut command = Command::new("pactl");
    command.args(["suspend-sink", &sink, "0"]);
    let _ = util::run_with_timeout(&mut command, Duration::from_millis(700));
}

fn sound_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/share/verbaq/sounds")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("sounds"));
            dirs.push(parent.join("../share/verbaq/sounds"));
        }
    }
    dirs.push(PathBuf::from("assets/sounds"));
    dirs.push(util::data_dir().join("sounds"));
    dirs
}

pub fn sound_path(set: &str, cue: Cue) -> Option<PathBuf> {
    if !valid_set(set) {
        return None;
    }
    let name = cue.file();
    for dir in sound_dirs() {
        let path = dir.join(set).join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn sound_available(set: &str) -> bool {
    [Cue::Start, Cue::Stop, Cue::Busy, Cue::Error, Cue::Test]
        .iter()
        .all(|cue| sound_path(set, *cue).is_some())
}

/// Play a cue on a background thread. Rapid repeats are dropped so a stuck
/// shortcut cannot produce a machine-gun of sounds.
pub fn play_async(cue: Cue, device: Option<&str>, set: &str) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let previous = LAST_PLAYED.swap(stamp, Ordering::Relaxed);
    if stamp.saturating_sub(previous) < 70 {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let device = device.map(str::to_string);
    let set = set.to_string();
    std::thread::spawn(move || play_blocking(cue, device.as_deref(), &set, generation));
}

/// Try the native PipeWire players and PulseAudio in turn. Each one gets a
/// short timeout so a broken audio server cannot leave a thread behind.
fn try_players(path: &Path, device: Option<&str>, generation: u64) -> bool {
    for program in ["pw-play", "pw-cat", "paplay"] {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return false;
        }
        let mut command = Command::new(program);
        match program {
            "pw-play" => {
                if let Some(device) = device {
                    command.args(["--target", device]);
                }
            }
            "pw-cat" => {
                command.arg("--playback");
                if let Some(device) = device {
                    command.args(["--target", device]);
                }
            }
            _ => {
                if let Some(device) = device {
                    command.arg(format!("--device={device}"));
                }
            }
        }
        command
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut success = false;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    success = status.success();
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if GENERATION.load(Ordering::SeqCst) != generation {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if success {
            return true;
        }
    }
    false
}

fn play_file(path: &Path, device: Option<&str>, generation: u64) -> bool {
    if GENERATION.load(Ordering::SeqCst) != generation {
        return false;
    }
    let device = device.filter(|name| !name.is_empty());
    if try_players(path, device, generation) {
        return true;
    }
    if GENERATION.load(Ordering::SeqCst) != generation {
        return false;
    }
    if let Some(name) = device {
        crate::logln!("sound output {name} failed, trying the system default");
        resume_sink(None);
        std::thread::sleep(Duration::from_millis(20));
        if GENERATION.load(Ordering::SeqCst) != generation {
            return false;
        }
        return try_players(path, None, generation);
    }
    false
}

fn play_blocking(cue: Cue, device: Option<&str>, set: &str, generation: u64) {
    let set = if valid_set(set) { set } else { "material" };
    let Some(path) = sound_path(set, cue) else {
        crate::logln!("sound set {set} is missing {}", cue.file());
        return;
    };
    // A suspended USB or wireless sink needs a moment to wake. The files
    // begin with 200 ms of pure silence, then the audible part fades in.
    resume_sink(device);
    std::thread::sleep(Duration::from_millis(20));
    if GENERATION.load(Ordering::SeqCst) != generation {
        return;
    }
    let _ = play_file(&path, device, generation);
}

pub fn player() -> Option<&'static str> {
    if util::which("pw-play").is_some() {
        Some("pw-play")
    } else if util::which("pw-cat").is_some() {
        Some("pw-cat")
    } else if util::which("paplay").is_some() {
        Some("paplay")
    } else {
        None
    }
}

/// Play a cue and wait for it, used by the setup wizard and tests.
pub fn play_now(cue: Cue, device: Option<&str>, set: &str) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    play_blocking(cue, device, set, generation);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_set_has_a_complete_audio_file_set() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/sounds");
        for (set, _) in SETS {
            for cue in [Cue::Start, Cue::Stop, Cue::Busy, Cue::Error, Cue::Test] {
                let path = root.join(set).join(cue.file());
                let data = std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                assert!(data.len() > 20_000, "{} is too small", path.display());
                assert_eq!(&data[0..4], b"RIFF", "{} is not a WAV", path.display());
                assert_eq!(&data[8..12], b"WAVE", "{} is not a WAV", path.display());
            }
        }
    }

    #[test]
    fn sound_sets_are_valid_and_cycle() {
        assert!(valid_set("material"));
        assert!(!valid_set("nope"));
        assert_eq!(set_label("material"), "Material");
        assert_eq!(set_label("minimal"), "Minimal");
    }
}
