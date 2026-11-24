use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::util;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub mic: String,
    pub hotkey: String,
    pub hotkey_codes: Vec<u16>,
    pub hold_to_talk: bool,
    pub sound: bool,
    pub paste_method: String,
    pub history: bool,
    pub history_limit: usize,
    pub threads: usize,
    pub idle_unload_secs: u64,
    pub max_record_secs: u64,
    pub paste_restore_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            model: "small".into(),
            mic: String::new(),
            hotkey: "Ctrl+Alt+D".into(),
            hotkey_codes: vec![29, 56, 32],
            hold_to_talk: true,
            sound: true,
            paste_method: "shift_insert".into(),
            history: false,
            history_limit: 200,
            threads: (cpus / 2).clamp(2, 8),
            idle_unload_secs: 900,
            max_record_secs: 120,
            paste_restore_ms: 400,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        util::config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        cfg.sanitize();
        // Older versions wrote the wrong key names. Trust the codes.
        if !cfg.hotkey_codes.is_empty() {
            cfg.hotkey = crate::hotkey::combo_label(&cfg.hotkey_codes);
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(util::config_dir())?;
        let path = Self::path();
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn sanitize(&mut self) {
        self.model = match self.model.as_str() {
            "turbo" | "large-v3-turbo" => "turbo".into(),
            _ => "small".into(),
        };
        self.paste_method = match self.paste_method.as_str() {
            "ctrl_v" => "ctrl_v".into(),
            _ => "shift_insert".into(),
        };
        self.threads = self.threads.clamp(1, 32);
        self.history_limit = self.history_limit.clamp(10, 10_000);
        self.paste_restore_ms = self.paste_restore_ms.clamp(100, 2_000);
        self.max_record_secs = self.max_record_secs.clamp(15, 600);
        self.idle_unload_secs = self.idle_unload_secs.clamp(60, 86_400);
        self.hotkey_codes.retain(|code| *code > 0 && *code <= 0x2ff);
        self.hotkey_codes.truncate(8);
        if self.hotkey_codes.is_empty() {
            self.hotkey = "unset".into();
        }
    }

    pub fn mic_option(&self) -> Option<&str> {
        if self.mic.is_empty() {
            None
        } else {
            Some(&self.mic)
        }
    }
}
