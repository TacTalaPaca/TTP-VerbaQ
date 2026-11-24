use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

static LAST_PLAYED: AtomicU64 = AtomicU64::new(0);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Play a cue on a background thread. Rapid repeats are dropped so a stuck
/// shortcut cannot produce a machine-gun of sounds.
pub fn play_async(cue: Cue) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let previous = LAST_PLAYED.swap(stamp, Ordering::Relaxed);
    if stamp.saturating_sub(previous) < 70 {
        return;
    }
    std::thread::spawn(move || play_blocking(cue));
}

fn tone(out: &mut Vec<i16>, freq: f32, ms: u32, gain: f32, sweep_to: Option<f32>) {
    const RATE: f32 = 48_000.0;
    let n = (RATE * ms as f32 / 1000.0) as usize;
    for i in 0..n {
        let t = i as f32 / RATE;
        let progress = i as f32 / n.max(1) as f32;
        let f = match sweep_to {
            Some(to) => freq + (to - freq) * progress,
            None => freq,
        };
        let attack = (progress / 0.08).min(1.0);
        let release = ((1.0 - progress) / 0.35).min(1.0);
        let env = attack * release;
        let sample = (t * f * std::f32::consts::TAU).sin() * gain * env;
        out.push((sample.clamp(-1.0, 1.0) * 32767.0) as i16);
    }
}

fn synth(cue: Cue) -> Vec<i16> {
    let mut out: Vec<i16> = Vec::new();
    match cue {
        Cue::Start => tone(&mut out, 660.0, 80, 0.24, Some(990.0)),
        Cue::Stop => tone(&mut out, 880.0, 60, 0.20, None),
        Cue::Busy => tone(&mut out, 240.0, 45, 0.12, None),
        Cue::Error => {
            tone(&mut out, 300.0, 90, 0.22, None);
            out.resize(out.len() + 2880, 0);
            tone(&mut out, 300.0, 90, 0.22, None);
        }
        Cue::Test => tone(&mut out, 660.0, 120, 0.22, Some(1100.0)),
    }
    out
}

fn cue_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("verbaq-cue-{}-{sequence}.wav", std::process::id()))
}

fn write_wav(samples: &[i16]) -> Option<PathBuf> {
    let path = cue_path();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).ok()?;
    for sample in samples {
        if writer.write_sample(*sample).is_err() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }
    if writer.finalize().is_err() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

/// Try the native PipeWire players and PulseAudio in turn. Each one gets a
/// short timeout so a broken audio server cannot leave a thread behind.
fn play_file(path: &Path) -> bool {
    for program in ["pw-play", "pw-cat", "paplay"] {
        let mut command = Command::new(program);
        if program == "pw-cat" {
            command.arg("--playback");
        }
        command
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        if matches!(
            util::wait_for_process(&mut child, Duration::from_secs(3)),
            Ok(status) if status.success()
        ) {
            return true;
        }
    }
    false
}

fn play_blocking(cue: Cue) {
    let samples = synth(cue);
    let Some(path) = write_wav(&samples) else {
        return;
    };
    let _ = play_file(&path);
    let _ = std::fs::remove_file(&path);
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
pub fn play_now(cue: Cue) {
    play_blocking(cue);
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "plays a real sound on the current audio server"]
    fn playback_path_succeeds() {
        let samples = super::synth(super::Cue::Test);
        let path = super::write_wav(&samples).expect("write cue wav");
        assert!(super::play_file(&path), "no audio player accepted the cue");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cues_produce_audio() {
        for cue in [
            super::Cue::Start,
            super::Cue::Stop,
            super::Cue::Busy,
            super::Cue::Error,
            super::Cue::Test,
        ] {
            assert!(!super::synth(cue).is_empty());
        }
    }
}
