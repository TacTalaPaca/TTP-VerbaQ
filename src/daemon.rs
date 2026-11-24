use anyhow::{bail, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool as StdAtomicBool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::{self, Recorder};
use crate::config::Config;
use crate::feedback::{self, Cue};
use crate::hotkey::{self, HotkeyEvent, ListenerConfig};
use crate::inject::{self, InsertResult, PasteMethod};
use crate::ipc::{self, Snapshot};
use crate::logln;
use crate::model;
use crate::stt::Transcriber;
use crate::util;

/// A transcription result tagged with the job it belongs to, so a late or
/// cancelled result can never be applied to newer work.
type DoneSlot = Arc<Mutex<Option<(u64, Result<String, String>)>>>;

struct LoadedModel {
    path: PathBuf,
    threads: i32,
    transcriber: Transcriber,
}

static SIGNALLED: StdAtomicBool = StdAtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

/// One line per meaningful event. When the service is detached this goes
/// nowhere, so it is purely for `verbaq daemon` in a terminal.
fn say(message: impl AsRef<str>) {
    println!("verbaq: {}", message.as_ref());
}

fn cue(cfg: &Config, cue: Cue) {
    if cfg.sound {
        feedback::play_async(cue);
    }
}

/// A transcription that takes longer than this is treated as failed so the
/// service can never stay stuck.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug)]
enum State {
    Idle,
    Recording { started: Instant },
    Transcribing { started: Instant },
}

impl State {
    fn is_idle(&self) -> bool {
        matches!(self, State::Idle)
    }

    fn is_recording(&self) -> bool {
        matches!(self, State::Recording { .. })
    }
}

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    util::ensure_dirs()?;
    crate::log::init();
    install_signal_handlers();
    logln!(
        "daemon starting, model={} mic={} shortcut={} mode={}",
        cfg.model,
        if cfg.mic.is_empty() {
            "default"
        } else {
            &cfg.mic
        },
        hotkey::combo_label(&cfg.hotkey_codes),
        if cfg.hold_to_talk { "hold" } else { "toggle" }
    );

    let (tx, rx) = channel::<HotkeyEvent>();
    let status = Arc::new(Mutex::new(Snapshot {
        state: "idle".into(),
        model: cfg.model.clone(),
        microphone: if cfg.mic.is_empty() {
            "default".into()
        } else {
            cfg.mic.clone()
        },
        shortcut: hotkey::combo_label(&cfg.hotkey_codes),
        mode: if cfg.hold_to_talk {
            "hold to talk".into()
        } else {
            "press to start, press to stop".into()
        },
        pid: std::process::id(),
        ..Default::default()
    }));

    let shutdown = Arc::new(AtomicBool::new(false));
    ipc::serve(tx.clone(), status.clone(), shutdown.clone())?;
    let listener = Arc::new(Mutex::new(ListenerConfig {
        codes: cfg.hotkey_codes.clone(),
        hold_to_talk: cfg.hold_to_talk,
    }));
    hotkey::spawn_listener(listener.clone(), tx.clone());

    let transcriber: Arc<Mutex<Option<LoadedModel>>> = Arc::new(Mutex::new(None));
    let done: DoneSlot = Arc::new(Mutex::new(None));

    say(format!(
        "{} service running in this terminal (debug mode). Ctrl+C stops it.",
        util::version()
    ));
    say(format!(
        "shortcut {} ({})",
        hotkey::combo_label(&cfg.hotkey_codes),
        if cfg.hold_to_talk {
            "hold to talk"
        } else {
            "press to toggle"
        }
    ));
    say(format!(
        "model {}, microphone {}",
        cfg.model,
        if cfg.mic.is_empty() {
            "default"
        } else {
            &cfg.mic
        }
    ));

    let mut cfg = cfg;
    let mut state = State::Idle;
    let mut recorder: Option<Recorder> = None;
    let mut last_use = Instant::now();
    let mut last_reload = Instant::now();
    let mut generation: u64 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) || SIGNALLED.load(Ordering::Relaxed) {
            break;
        }

        if let Some((job, result)) = done.lock().ok().and_then(|mut slot| slot.take()) {
            if job == generation && matches!(state, State::Transcribing { .. }) {
                apply_result(result, &cfg, &status);
                state = State::Idle;
                last_use = Instant::now();
                continue;
            }
            logln!("dropped a stale transcription result");
        }

        // Never record forever if a key release was lost.
        if let State::Recording { started } = &state {
            if started.elapsed() > Duration::from_secs(cfg.max_record_secs) {
                logln!("recording reached the {} s limit", cfg.max_record_secs);
                finish_recording(
                    &mut recorder,
                    &mut state,
                    &cfg,
                    &status,
                    &transcriber,
                    &done,
                    generation,
                );
            }
        }

        // A disconnected microphone must stop the recording with a clear state.
        if let State::Recording { .. } = &state {
            if let Some(error) = recorder.as_ref().and_then(|rec| rec.error()) {
                logln!("microphone stopped: {error}");
                if let Some(rec) = recorder.take() {
                    let _ = rec.stop();
                }
                state = State::Idle;
                ipc::reset_status(&status, "error", None);
                cue(&cfg, Cue::Error);
                say(format!("microphone stopped: {error}"));
            }
        }

        // Never stay busy forever if the model hangs.
        if let State::Transcribing { started } = &state {
            if started.elapsed() > TRANSCRIBE_TIMEOUT {
                logln!(
                    "transcription timed out after {} s",
                    TRANSCRIBE_TIMEOUT.as_secs()
                );
                generation = generation.wrapping_add(1);
                state = State::Idle;
                ipc::reset_status(&status, "error", None);
                cue(&cfg, Cue::Error);
            }
        }

        // Live settings: changes in the settings screen apply within two seconds.
        if state.is_idle() && last_reload.elapsed() > Duration::from_secs(2) {
            last_reload = Instant::now();
            if let Ok(fresh) = Config::load() {
                cfg = fresh;
                if let Ok(mut listener) = listener.lock() {
                    listener.codes = cfg.hotkey_codes.clone();
                    listener.hold_to_talk = cfg.hold_to_talk;
                }
                if let Ok(mut snapshot) = status.lock() {
                    snapshot.shortcut = hotkey::combo_label(&cfg.hotkey_codes);
                    snapshot.mode = if cfg.hold_to_talk {
                        "hold to talk".into()
                    } else {
                        "press to start, press to stop".into()
                    };
                    snapshot.model = cfg.model.clone();
                    snapshot.microphone = if cfg.mic.is_empty() {
                        "default".into()
                    } else {
                        cfg.mic.clone()
                    };
                }
            }
        }

        if state.is_idle() {
            let loaded = transcriber
                .try_lock()
                .map(|value| value.is_some())
                .unwrap_or(false);
            if loaded && last_use.elapsed() > Duration::from_secs(cfg.idle_unload_secs) {
                if let Ok(mut guard) = transcriber.try_lock() {
                    *guard = None;
                }
                logln!("model unloaded after idle");
            }
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(HotkeyEvent::Start) => {
                if state.is_idle() {
                    generation = generation.wrapping_add(1);
                    start_recording(&mut recorder, &mut state, &cfg, &status);
                } else {
                    logln!("shortcut ignored while busy");
                    cue(&cfg, Cue::Busy);
                }
            }
            Ok(HotkeyEvent::Stop) => {
                if state.is_recording() {
                    finish_recording(
                        &mut recorder,
                        &mut state,
                        &cfg,
                        &status,
                        &transcriber,
                        &done,
                        generation,
                    );
                }
            }
            Ok(HotkeyEvent::Toggle) => {
                if state.is_idle() {
                    generation = generation.wrapping_add(1);
                    start_recording(&mut recorder, &mut state, &cfg, &status);
                } else if state.is_recording() {
                    finish_recording(
                        &mut recorder,
                        &mut state,
                        &cfg,
                        &status,
                        &transcriber,
                        &done,
                        generation,
                    );
                } else {
                    logln!("shortcut ignored while transcribing");
                    cue(&cfg, Cue::Busy);
                }
            }
            Ok(HotkeyEvent::Cancel) => {
                if state.is_recording() {
                    if let Some(rec) = recorder.take() {
                        let _ = rec.stop();
                    }
                    state = State::Idle;
                    ipc::reset_status(&status, "idle", None);
                    cue(&cfg, Cue::Busy);
                    say("cancelled");
                    logln!("recording cancelled");
                } else if matches!(state, State::Transcribing { .. }) {
                    generation = generation.wrapping_add(1);
                    state = State::Idle;
                    ipc::reset_status(&status, "idle", None);
                    cue(&cfg, Cue::Busy);
                    say("cancelled");
                    logln!("transcription cancelled");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("event channel closed"),
        }
    }

    say("stopping");
    logln!("daemon stopped");
    let _ = std::fs::remove_file(util::socket_path());
    Ok(())
}

fn start_recording(
    recorder: &mut Option<Recorder>,
    state: &mut State,
    cfg: &Config,
    status: &Arc<Mutex<Snapshot>>,
) {
    let Some(spec) = model::find(&cfg.model) else {
        logln!("unknown model {}", cfg.model);
        cue(cfg, Cue::Error);
        return;
    };
    if !model::is_installed(spec) {
        logln!("model {} is not installed", spec.label);
        ipc::reset_status(status, "idle", None);
        cue(cfg, Cue::Error);
        return;
    }
    match Recorder::start(cfg.mic_option()) {
        Ok(rec) => {
            *recorder = Some(rec);
            *state = State::Recording {
                started: Instant::now(),
            };
            ipc::reset_status(status, "listening", None);
            cue(cfg, Cue::Start);
            say("listening");
            logln!("recording started");
        }
        Err(err) => {
            say(format!("microphone error: {err}"));
            logln!("cannot start the microphone: {err}");
            ipc::reset_status(status, "idle", None);
            cue(cfg, Cue::Error);
        }
    }
}

fn finish_recording(
    recorder: &mut Option<Recorder>,
    state: &mut State,
    cfg: &Config,
    status: &Arc<Mutex<Snapshot>>,
    transcriber: &Arc<Mutex<Option<LoadedModel>>>,
    done: &DoneSlot,
    generation: u64,
) {
    let Some(rec) = recorder.take() else {
        *state = State::Idle;
        return;
    };
    let (samples, rate) = rec.stop();
    let samples = audio::resample_to_target(&samples, rate);
    let trimmed = audio::trim_silence(&samples, audio::TARGET_RATE);
    cue(cfg, Cue::Stop);
    say("transcribing");
    logln!(
        "recording stopped: {:.1} s captured, {:.1} s after trimming",
        samples.len() as f32 / audio::TARGET_RATE as f32,
        trimmed.len() as f32 / audio::TARGET_RATE as f32
    );

    if !audio::has_speech(&trimmed) {
        *state = State::Idle;
        ipc::reset_status(status, "idle", None);
        say("no speech detected");
        logln!("no speech in the recording");
        return;
    }

    // Pick up model changes made since the daemon started.
    let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
    let Some(spec) = model::find(&cfg.model) else {
        *state = State::Idle;
        ipc::reset_status(status, "error", None);
        cue(&cfg, Cue::Error);
        logln!("unknown model {}", cfg.model);
        return;
    };
    if !model::is_installed(spec) {
        *state = State::Idle;
        ipc::reset_status(status, "error", None);
        cue(&cfg, Cue::Error);
        logln!("model {} is not installed", spec.label);
        return;
    }

    *state = State::Transcribing {
        started: Instant::now(),
    };
    ipc::reset_status(status, "transcribing", None);

    let transcriber = transcriber.clone();
    let done = done.clone();
    let path = model::path(spec);
    let threads = cfg.threads as i32;
    std::thread::spawn(move || {
        let result = (|| -> std::result::Result<String, String> {
            let mut guard = transcriber
                .try_lock()
                .map_err(|_| "a previous transcription is still running".to_string())?;
            let reload = match guard.as_ref() {
                Some(loaded) => loaded.path != path || loaded.threads != threads,
                None => true,
            };
            if reload {
                let loaded = Transcriber::load(&path, threads)
                    .map_err(|error| format!("cannot load the model: {error}"))?;
                *guard = Some(LoadedModel {
                    path: path.clone(),
                    threads,
                    transcriber: loaded,
                });
            }
            guard
                .as_ref()
                .expect("model was just loaded")
                .transcriber
                .transcribe(&trimmed)
                .map_err(|error| format!("transcription failed: {error}"))
        })();
        if let Ok(mut slot) = done.lock() {
            *slot = Some((generation, result));
        }
    });
}

fn apply_result(
    result: std::result::Result<String, String>,
    cfg: &Config,
    status: &Arc<Mutex<Snapshot>>,
) {
    match result {
        Ok(text) if !text.is_empty() => {
            let method = PasteMethod::from_str(&cfg.paste_method);
            match inject::insert_text(&text, method, cfg.paste_restore_ms) {
                Ok(InsertResult::Inserted) => {
                    ipc::reset_status(status, "idle", Some(&text));
                    say(format!("inserted {} characters", text.len()));
                    logln!("inserted {} characters", text.len());
                }
                Ok(InsertResult::Copied) => {
                    ipc::reset_status(status, "idle", Some(&text));
                    cue(cfg, Cue::Error);
                    say("could not paste, text is on the clipboard");
                    logln!("paste unavailable, text left on the clipboard");
                }
                Ok(InsertResult::Blocked) => {
                    ipc::reset_status(status, "idle", Some(&text));
                    cue(cfg, Cue::Error);
                    say("clipboard holds non-text data, text not inserted");
                    logln!("clipboard holds non-text data, text not inserted");
                }
                Err(error) => {
                    let copied = inject::copy_only(&text);
                    ipc::reset_status(status, "idle", Some(&text));
                    cue(cfg, Cue::Error);
                    if copied {
                        say("could not paste, text is on the clipboard");
                        logln!("insert failed, text left on the clipboard: {error}");
                    } else {
                        say("could not paste or copy the text");
                        logln!("insert and copy failed: {error}");
                    }
                }
            }
            if cfg.history {
                append_history(&text, cfg.history_limit);
            }
        }
        Ok(_) => {
            ipc::reset_status(status, "idle", None);
            say("nothing was transcribed");
            logln!("empty transcript");
        }
        Err(error) => {
            ipc::reset_status(status, "error", None);
            cue(cfg, Cue::Error);
            say(format!("error: {error}"));
            logln!("{error}");
        }
    }
}

fn append_history(text: &str, limit: usize) {
    let path = util::history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = format!(
        "{}\t{}\n",
        chrono::Local::now().timestamp(),
        text.replace(['\n', '\r', '\t'], " ")
    );
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
    if let Ok(content) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > limit {
            let keep = lines[lines.len() - limit..].join("\n") + "\n";
            let _ = std::fs::write(&path, keep);
        }
    }
}
