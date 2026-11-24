use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const TARGET_RATE: u32 = 16_000;

pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok().map(|d| d.name().to_string()));
    let mut seen = std::collections::HashSet::new();
    let mut names: Vec<String> = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(description) = device.description() {
                let name = description.name().to_string();
                if seen.insert(name.clone()) {
                    names.push(name);
                }
            }
        }
    }
    if let Some(default) = default_name {
        if let Some(index) = names.iter().position(|n| *n == default) {
            names.swap(0, index);
        }
    }
    names
}

#[derive(Clone)]
struct Capture {
    buf: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
    format: SampleFormat,
    store: bool,
}

impl Capture {
    // as_chunks needs a newer compiler than the declared minimum.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn copy(&self, bytes: &[u8], channels: usize) {
        let mut mono: Vec<f32> = Vec::with_capacity(bytes.len() / 4);
        match self.format {
            SampleFormat::F32 => {
                for chunk in bytes.chunks_exact(4) {
                    mono.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            SampleFormat::I16 => {
                for chunk in bytes.chunks_exact(2) {
                    mono.push(i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0);
                }
            }
            SampleFormat::U16 => {
                for chunk in bytes.chunks_exact(2) {
                    let v = u16::from_le_bytes([chunk[0], chunk[1]]) as f32;
                    mono.push((v - 32768.0) / 32768.0);
                }
            }
            SampleFormat::I32 => {
                for chunk in bytes.chunks_exact(4) {
                    let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    mono.push(v as f32 / 2_147_483_648.0);
                }
            }
            _ => {}
        }
        if channels > 1 {
            let frames = mono.len() / channels;
            let mut mixed = Vec::with_capacity(frames);
            for frame in 0..frames {
                let mut sum = 0.0;
                for channel in 0..channels {
                    sum += mono[frame * channels + channel];
                }
                mixed.push(sum / channels as f32);
            }
            mono = mixed;
        }
        if mono.is_empty() {
            return;
        }
        let rms =
            (mono.iter().map(|sample| sample * sample).sum::<f32>() / mono.len() as f32).sqrt();
        self.level.store(rms.to_bits(), Ordering::Relaxed);
        if self.store {
            if let Ok(mut buffer) = self.buf.lock() {
                buffer.extend_from_slice(&mono);
            }
        }
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    capture: &Capture,
    errors: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    let capture = capture.clone();
    let channels = config.channels as usize;
    let errors = errors.clone();
    Ok(device.build_input_stream_raw(
        config,
        capture.format,
        move |data, _info| capture.copy(data.bytes(), channels),
        move |error| {
            eprintln!("audio error: {error}");
            if let Ok(mut slot) = errors.lock() {
                *slot = Some(error.to_string());
            }
        },
        None,
    )?)
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    buf: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    rate: u32,
}

impl Recorder {
    pub fn start(device_name: Option<&str>) -> Result<Self> {
        Self::start_with(device_name, true)
    }

    /// Level-only recorder for the settings meter. It never retains audio.
    pub fn start_meter(device_name: Option<&str>) -> Result<Self> {
        Self::start_with(device_name, false)
    }

    fn start_with(device_name: Option<&str>, store: bool) -> Result<Self> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) if !name.is_empty() => host
                .input_devices()?
                .find(|d| d.description().map(|d| d.name() == name).unwrap_or(false))
                .ok_or_else(|| anyhow!("microphone not found: {name}"))?,
            _ => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no microphone found"))?,
        };

        let supported = device.default_input_config()?;
        let default_config = supported.config();
        let format = supported.sample_format();
        let preferred = StreamConfig {
            channels: 1,
            sample_rate: TARGET_RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let preferred_supported = device
            .supported_input_configs()
            .map(|ranges| {
                ranges.into_iter().any(|range| {
                    range.channels() == 1
                        && range.min_sample_rate() <= TARGET_RATE
                        && range.max_sample_rate() >= TARGET_RATE
                })
            })
            .unwrap_or(false);
        let first_config = if preferred_supported {
            preferred
        } else {
            default_config.clone()
        };

        let buf = Arc::new(Mutex::new(Vec::with_capacity(
            first_config.sample_rate as usize * 40,
        )));
        let level = Arc::new(AtomicU32::new(0));
        let errors: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let capture = Capture {
            buf: buf.clone(),
            level: level.clone(),
            format,
            store,
        };

        let (stream, rate) = match build_stream(&device, &first_config, &capture, &errors) {
            Ok(stream) => (stream, first_config.sample_rate),
            Err(first_error) if preferred_supported => {
                eprintln!("verbaq: 16 kHz input failed ({first_error}), trying the device default");
                let stream = build_stream(&device, &default_config, &capture, &errors)?;
                (stream, default_config.sample_rate)
            }
            Err(error) => return Err(error),
        };
        stream.play()?;

        Ok(Self {
            stream: Some(stream),
            buf,
            level,
            error: errors,
            rate,
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn stop(mut self) -> (Vec<f32>, u32) {
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        // Give the audio thread a moment to hand over the last frames.
        std::thread::sleep(Duration::from_millis(80));
        let samples = self.buf.lock().map(|b| b.clone()).unwrap_or_default();
        (samples, self.rate)
    }
}

/// Downmix is already done. Convert to 16 kHz mono.
pub fn resample_to_target(input: &[f32], from: u32) -> Vec<f32> {
    if input.is_empty() || from == TARGET_RATE {
        return input.to_vec();
    }
    let to = TARGET_RATE;
    if from > to {
        let factor = from as f64 / to as f64;
        let window = factor.ceil() as usize;
        let out_len = (input.len() as f64 / factor).floor() as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let start = (i as f64 * factor).floor() as usize;
            let end = (start + window).min(input.len());
            if start >= end {
                break;
            }
            let sum: f32 = input[start..end].iter().sum();
            out.push(sum / (end - start) as f32);
        }
        out
    } else {
        let factor = to as f64 / from as f64;
        let out_len = (input.len() as f64 * factor).floor() as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = i as f64 / factor;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input.get(idx).copied().unwrap_or(0.0);
            let b = input.get(idx + 1).copied().unwrap_or(a);
            out.push(a + (b - a) * frac);
        }
        out
    }
}

/// Remove leading and trailing silence, keep a short pad around speech.
pub fn trim_silence(samples: &[f32], rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let frame = (rate as usize / 50).max(1); // 20 ms
    let frames = samples.len() / frame;
    if frames == 0 {
        return samples.to_vec();
    }
    let mut energies = Vec::with_capacity(frames);
    for i in 0..frames {
        let slice = &samples[i * frame..(i + 1) * frame];
        let rms = (slice.iter().map(|s| s * s).sum::<f32>() / slice.len() as f32).sqrt();
        energies.push(rms);
    }
    let peak = energies.iter().cloned().fold(0.0f32, f32::max);
    if peak < 0.004 {
        return Vec::new();
    }
    let threshold = (peak * 0.08).max(0.004);

    let first = energies.iter().position(|e| *e >= threshold);
    let last = energies.iter().rposition(|e| *e >= threshold);
    match (first, last) {
        (Some(a), Some(b)) => {
            let pad = (rate as usize / 10).max(1); // 100 ms
            let start = (a * frame).saturating_sub(pad);
            let end = ((b + 1) * frame + pad).min(samples.len());
            samples[start..end].to_vec()
        }
        _ => Vec::new(),
    }
}

/// True when there is enough loud audio to be worth transcribing.
pub fn has_speech(samples: &[f32]) -> bool {
    if samples.len() < (TARGET_RATE as usize / 5) {
        return false; // under 200 ms
    }
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 0.01 {
        return false;
    }
    let loud = samples.iter().filter(|s| s.abs() > 0.01).count();
    loud > TARGET_RATE as usize / 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_to_16k_and_keeps_length() {
        let input = vec![0.5f32; 48_000];
        let out = resample_to_target(&input, 48_000);
        assert!((out.len() as i64 - 16_000).abs() <= 2);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 0.01));
    }

    #[test]
    fn silence_is_trimmed_away() {
        let mut samples = vec![0.0f32; 16_000];
        for s in samples.iter_mut().skip(8_000).take(3_200) {
            *s = 0.3;
        }
        let trimmed = trim_silence(&samples, 16_000);
        assert!(trimmed.len() < samples.len());
        assert!(has_speech(&trimmed));
    }

    #[test]
    fn pure_silence_has_no_speech() {
        let samples = vec![0.0f32; 16_000];
        assert!(!has_speech(&trim_silence(&samples, 16_000)));
    }
}
