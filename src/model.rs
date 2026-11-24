use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::util;

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub note: &'static str,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "small",
        label: "Small",
        file: "ggml-small-q5_1.bin",
        size: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        note: "181 MB, fastest, good for everyday dictation",
    },
    ModelSpec {
        id: "turbo",
        label: "Turbo",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        note: "547 MB, more accurate, about 3x slower on CPU",
    },
];

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn path(spec: &ModelSpec) -> PathBuf {
    util::models_dir().join(spec.file)
}

pub fn is_installed(spec: &ModelSpec) -> bool {
    let p = path(spec);
    match std::fs::metadata(&p) {
        Ok(meta) => meta.len() == spec.size,
        Err(_) => false,
    }
}

fn url(spec: &ModelSpec) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        spec.file
    )
}

/// Download a model to a temporary file, verify its hash, then move it into place.
/// `progress(done, total)` is called at most a few times per second.
pub fn download(spec: &ModelSpec, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    std::fs::create_dir_all(util::models_dir())?;
    let dest = path(spec);
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build();
    let response = agent
        .get(&url(spec))
        .call()
        .with_context(|| format!("cannot reach the model server for {}", spec.label))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(spec.size);

    let mut reader = response.into_reader();
    let mut hasher = Sha256::new();
    let mut out = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    let mut last_report = std::time::Instant::now();

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if last_report.elapsed().as_millis() > 120 {
            progress(done, total);
            last_report = std::time::Instant::now();
        }
    }
    out.flush()?;
    drop(out);
    progress(done, total);

    let digest = hex::encode(hasher.finalize());
    if digest != spec.sha256 {
        let _ = std::fs::remove_file(&part);
        bail!(
            "downloaded {} is corrupt (checksum mismatch). Nothing was installed.",
            spec.label
        );
    }

    std::fs::rename(&part, &dest)?;
    Ok(())
}

pub fn remove(spec: &ModelSpec) -> Result<()> {
    let p = path(spec);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}
