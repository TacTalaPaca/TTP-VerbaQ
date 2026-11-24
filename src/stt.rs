use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Once;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::util;

static HOOKS: Once = Once::new();

pub struct Transcriber {
    ctx: WhisperContext,
    threads: i32,
}

impl Transcriber {
    pub fn load(path: &Path, threads: i32) -> Result<Self> {
        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(path, params)
            .with_context(|| format!("cannot load model {}", path.display()))?;
        Ok(Self { ctx, threads })
    }

    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        HOOKS.call_once(whisper_rs::install_logging_hooks);
        let mut state = self.ctx.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(None);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // One segment is faster, but a long dictation could hit the token limit.
        params.set_single_segment(samples.len() <= 16_000 * 28);
        params.set_no_speech_thold(0.6);
        // Allow whisper.cpp to retry hard audio at a higher temperature instead
        // of looping on one wrong hypothesis.
        params.set_temperature_inc(0.2);
        state.full(params, samples)?;

        let mut out = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(text) = segment.to_str() {
                    out.push_str(text);
                }
            }
        }
        Ok(util::clean_transcript(&out))
    }
}
