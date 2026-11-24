use std::io::Write;
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
        Cue::Start => tone(&mut out, 660.0, 70, 0.16, Some(990.0)),
        Cue::Stop => tone(&mut out, 880.0, 45, 0.12, None),
        Cue::Busy => tone(&mut out, 240.0, 40, 0.08, None),
        Cue::Error => {
            tone(&mut out, 300.0, 70, 0.15, None);
            out.resize(out.len() + 2880, 0);
            tone(&mut out, 300.0, 70, 0.15, None);
        }
        Cue::Test => tone(&mut out, 700.0, 90, 0.14, Some(1100.0)),
    }
    out
}

fn play_with_pw_cat(samples: &[i16]) -> bool {
    let Ok(mut child) = Command::new("pw-cat")
        .args([
            "--playback",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "1",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if stdin.write_all(&bytes).is_err() {
            return false;
        }
    }
    matches!(
        util::wait_for_process(&mut child, Duration::from_secs(2)),
        Ok(status) if status.success()
    )
}

fn play_with_paplay(samples: &[i16]) -> bool {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = std::path::Path::new(&dir).join("verbaq-cue.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let Ok(mut writer) = hound::WavWriter::create(&path, spec) else {
        return false;
    };
    for sample in samples {
        if writer.write_sample(*sample).is_err() {
            let _ = std::fs::remove_file(&path);
            return false;
        }
    }
    if writer.finalize().is_err() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    let ok = match Command::new("paplay")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => util::wait_for_process(&mut child, Duration::from_secs(2))
            .map(|status| status.success())
            .unwrap_or(false),
        Err(_) => false,
    };
    let _ = std::fs::remove_file(&path);
    ok
}

fn play_blocking(cue: Cue) {
    let samples = synth(cue);
    if play_with_pw_cat(&samples) {
        return;
    }
    let _ = play_with_paplay(&samples);
}

pub fn player() -> Option<&'static str> {
    if util::which("pw-cat").is_some() {
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
