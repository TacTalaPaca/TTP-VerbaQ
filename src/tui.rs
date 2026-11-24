use anyhow::Result;
use chrono::TimeZone;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Terminal;
use std::io::stdout;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::audio;
use crate::config::Config;
use crate::doctor;
use crate::feedback::{self, Cue};
use crate::hotkey;
use crate::inject;
use crate::ipc;
use crate::model;
use crate::service;
use crate::shortcuts;
use crate::util;

const SECTIONS: &[&str] = &[
    "Service",
    "Microphone",
    "Voice model",
    "Shortcut",
    "Sound",
    "Text insertion",
    "History",
    "Health",
];

enum Progress {
    Update(u64, u64),
    Done(Result<(), String>),
}

struct Download {
    spec: &'static model::ModelSpec,
    rx: Receiver<Progress>,
    done: u64,
    total: u64,
}

#[derive(Clone, Copy)]
enum Confirm {
    DeleteModel(&'static model::ModelSpec),
    ClearHistory,
}

struct App {
    cfg: Config,
    section: usize,
    devices: Vec<String>,
    device_idx: usize,
    model_idx: usize,
    suggestion_idx: usize,
    meter: Option<audio::Recorder>,
    download: Option<Download>,
    message: String,
    running: bool,
    last_check: Instant,
    health: Vec<(bool, String, String)>,
    history: Vec<String>,
    capture_requested: bool,
    pending_confirm: Option<(String, Vec<u16>)>,
    keyboard_permission: bool,
    status_text: String,
    existing: Vec<shortcuts::Existing>,
    show_existing: bool,
    show_help: bool,
    body_scroll: usize,
    confirm: Option<Confirm>,
    save_error: Option<String>,
}

fn dim(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(Color::DarkGray),
    ))
}

fn plain(text: impl Into<String>) -> Line<'static> {
    Line::from(text.into())
}

fn ok_mark(free: bool) -> Span<'static> {
    if free {
        Span::styled("✓ free", Style::default().fg(Color::Green))
    } else {
        Span::styled("✗ used", Style::default().fg(Color::Yellow))
    }
}

fn rule() -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(46),
        Style::default().fg(Color::DarkGray),
    ))
}

const GLYPHS: [&str; 8] = ["●", "◉", "◆", "⌨", "♪", "↵", "≡", "✔"];

impl App {
    fn new() -> Self {
        let cfg = Config::load().unwrap_or_default();
        let devices = audio::input_devices();
        let device_idx = devices.iter().position(|d| d == &cfg.mic).unwrap_or(0);
        let model_idx = model::MODELS
            .iter()
            .position(|m| m.id == cfg.model)
            .unwrap_or(0);
        let suggestion_idx = shortcuts::SUGGESTIONS
            .iter()
            .position(|s| s.codes == cfg.hotkey_codes.as_slice())
            .unwrap_or(0);
        Self {
            cfg,
            section: 0,
            devices,
            device_idx,
            model_idx,
            suggestion_idx,
            meter: None,
            download: None,
            message: String::new(),
            running: false,
            last_check: Instant::now() - Duration::from_secs(5),
            health: Vec::new(),
            history: load_history(),
            capture_requested: false,
            pending_confirm: None,
            keyboard_permission: hotkey::permission_available(),
            status_text: String::new(),
            existing: Vec::new(),
            show_existing: false,
            show_help: false,
            body_scroll: 0,
            confirm: None,
            save_error: None,
        }
    }

    fn stop_meter(&mut self) {
        if let Some(meter) = self.meter.take() {
            let _ = meter.stop();
        }
    }

    fn refresh_existing(&mut self) {
        self.existing = shortcuts::existing();
    }

    fn persist(&mut self) {
        match self.cfg.save() {
            Ok(()) => self.save_error = None,
            Err(error) => self.save_error = Some(error.to_string()),
        }
    }

    fn scroll_by(&mut self, amount: isize) {
        let length = if self.show_help {
            help_body().len()
        } else {
            self.body().len()
        };
        let maximum = length.saturating_sub(1);
        self.body_scroll = if amount >= 0 {
            self.body_scroll
                .saturating_add(amount as usize)
                .min(maximum)
        } else {
            self.body_scroll.saturating_sub(amount.unsigned_abs())
        };
    }

    fn execute_confirm(&mut self, action: Confirm) {
        self.confirm = None;
        match action {
            Confirm::DeleteModel(spec) => {
                if let Err(error) = model::remove(spec) {
                    self.message = format!("Could not remove {}: {error}", spec.label);
                    return;
                }
                if self.cfg.model == spec.id {
                    if let Some(other) = model::MODELS
                        .iter()
                        .find(|other| other.id != spec.id && model::is_installed(other))
                    {
                        self.cfg.model = other.id.to_string();
                        self.persist();
                        self.message =
                            format!("{} removed. Switched to {}.", spec.label, other.label);
                    } else {
                        self.message =
                            format!("{} removed. Install a model before dictating.", spec.label);
                    }
                } else {
                    self.message = format!("{} removed.", spec.label);
                }
            }
            Confirm::ClearHistory => match std::fs::remove_file(util::history_path()) {
                Ok(()) => {
                    self.history.clear();
                    self.message = "History cleared from disk.".into();
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.history.clear();
                    self.message = "History is already empty.".into();
                }
                Err(error) => self.message = format!("Could not clear history: {error}"),
            },
        }
    }

    fn tick(&mut self) {
        if self.last_check.elapsed() > Duration::from_secs(1) {
            self.running = ipc::is_running();
            if self.running {
                self.status_text = ipc::send("status").unwrap_or_default();
            } else {
                self.status_text.clear();
            }
            self.last_check = Instant::now();
        }
        self.poll_download();
    }

    fn poll_download(&mut self) {
        let mut finished: Option<Result<(), String>> = None;
        if let Some(dl) = self.download.as_mut() {
            loop {
                match dl.rx.try_recv() {
                    Ok(Progress::Update(done, total)) => {
                        dl.done = done;
                        dl.total = total;
                    }
                    Ok(Progress::Done(result)) => {
                        finished = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some(Err("download thread stopped".into()));
                        break;
                    }
                }
            }
        }
        if let Some(result) = finished {
            if let Some(dl) = self.download.take() {
                match result {
                    Ok(()) => {
                        self.cfg.model = dl.spec.id.to_string();
                        self.persist();
                        self.message = format!("{} installed and selected.", dl.spec.label);
                    }
                    Err(err) => self.message = format!("Download failed: {err}"),
                }
            }
        }
    }

    fn start_download(&mut self, spec: &'static model::ModelSpec) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = model::download(spec, |done, total| {
                let _ = tx.send(Progress::Update(done, total));
            })
            .map_err(|e| e.to_string());
            let _ = tx.send(Progress::Done(result));
        });
        self.download = Some(Download {
            spec,
            rx,
            done: 0,
            total: spec.size,
        });
        self.message = format!("Downloading {}...", spec.label);
    }

    fn on_key(&mut self, code: KeyCode) -> Result<bool> {
        if self.show_help {
            match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.show_help = false;
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
                KeyCode::PageDown => self.scroll_by(10),
                KeyCode::PageUp => self.scroll_by(-10),
                _ => {}
            }
            return Ok(false);
        }

        if self.show_existing {
            match code {
                KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('q') => {
                    self.show_existing = false;
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
                KeyCode::PageDown => self.scroll_by(10),
                KeyCode::PageUp => self.scroll_by(-10),
                _ => {}
            }
            return Ok(false);
        }

        if let Some(action) = self.confirm {
            match code {
                KeyCode::Enter => self.execute_confirm(action),
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.confirm = None;
                    self.message = "Cancelled.".into();
                }
                _ => {}
            }
            return Ok(false);
        }

        match code {
            KeyCode::Char('?') => {
                self.show_help = true;
                self.body_scroll = 0;
            }
            KeyCode::PageDown => self.scroll_by(10),
            KeyCode::PageUp => self.scroll_by(-10),
            KeyCode::Esc => {
                self.stop_meter();
                return Ok(true);
            }
            KeyCode::Char('q') => {
                if self.pending_confirm.is_some() {
                    self.pending_confirm = None;
                    self.message = "Custom shortcut discarded.".into();
                } else {
                    self.stop_meter();
                    return Ok(true);
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                self.pending_confirm = None;
                self.stop_meter();
                self.section = (self.section + 1) % SECTIONS.len();
                self.after_section_change();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.pending_confirm = None;
                self.stop_meter();
                self.section = (self.section + SECTIONS.len() - 1) % SECTIONS.len();
                self.after_section_change();
            }
            _ => self.section_key(code)?,
        }
        Ok(false)
    }

    fn after_section_change(&mut self) {
        self.body_scroll = 0;
        if self.section == 1 {
            self.devices = audio::input_devices();
            if self.device_idx >= self.devices.len() {
                self.device_idx = 0;
            }
        }
        if self.section == 3 {
            self.keyboard_permission = hotkey::permission_available();
            self.refresh_existing();
        }
        if self.section == 6 {
            self.history = load_history();
        }
    }

    fn apply_shortcut(&mut self, codes: &[u16], label: &str) {
        self.cfg.hotkey_codes = hotkey::normalize_codes(codes);
        self.cfg.hotkey = label.to_string();
        self.persist();
        self.pending_confirm = None;
        self.suggestion_idx = shortcuts::SUGGESTIONS
            .iter()
            .position(|s| s.codes == codes)
            .unwrap_or(self.suggestion_idx);
        if self.existing.is_empty() {
            self.refresh_existing();
        }
        match shortcuts::taken_by_in(&self.existing, label) {
            Some(source) => {
                self.message = format!(
                    "Saved {label}. Careful: {source} already uses it. Applies within 2 seconds."
                )
            }
            None => self.message = format!("Saved {label}. Applies within 2 seconds."),
        }
    }

    fn section_key(&mut self, code: KeyCode) -> Result<()> {
        if self.section == 7 && code == KeyCode::Enter {
            self.health = doctor::run(false)
                .into_iter()
                .map(|check| (check.ok, check.label, check.detail))
                .collect();
            self.message = "Health check done.".into();
            return Ok(());
        }

        match self.section {
            0 => match code {
                KeyCode::Char('s') => match service::start() {
                    Ok(message) => {
                        self.running = ipc::is_running();
                        self.message = message;
                    }
                    Err(err) => self.message = format!("Could not start: {err:#}"),
                },
                KeyCode::Char('x') => match service::stop() {
                    Ok(message) => {
                        self.running = ipc::is_running();
                        self.message = message;
                    }
                    Err(err) => self.message = format!("Could not stop: {err:#}"),
                },
                KeyCode::Char('a') => {
                    if service::autostart_enabled() {
                        service::autostart_disable();
                        self.message =
                            "Start at login is off. The autostart file was removed.".into();
                    } else {
                        match service::autostart_enable() {
                            Ok(()) => self.message =
                                "Start at login is on. One file was added in ~/.config/autostart."
                                    .into(),
                            Err(err) => self.message = format!("Could not enable: {err:#}"),
                        }
                    }
                }
                KeyCode::Enter => {
                    self.running = ipc::is_running();
                    if self.running {
                        self.status_text = ipc::send("status").unwrap_or_default();
                    } else {
                        self.status_text.clear();
                    }
                    self.message = "Status refreshed.".into();
                }
                _ => {}
            },
            1 => match code {
                KeyCode::Left | KeyCode::Right => {
                    if !self.devices.is_empty() {
                        if code == KeyCode::Right {
                            self.device_idx = (self.device_idx + 1) % self.devices.len();
                        } else {
                            self.device_idx =
                                (self.device_idx + self.devices.len() - 1) % self.devices.len();
                        }
                        self.cfg.mic = self.devices[self.device_idx].clone();
                        self.persist();
                        if self.meter.is_some() {
                            self.stop_meter();
                            self.meter = audio::Recorder::start_meter(self.cfg.mic_option()).ok();
                        }
                    }
                }
                KeyCode::Enter => {
                    if self.meter.is_some() {
                        self.stop_meter();
                        self.message = "Level meter stopped.".into();
                    } else {
                        match audio::Recorder::start_meter(self.cfg.mic_option()) {
                            Ok(rec) => {
                                self.meter = Some(rec);
                                self.message =
                                    "Speak and watch the bar. Enter stops the meter.".into();
                            }
                            Err(err) => self.message = format!("Microphone error: {err}"),
                        }
                    }
                }
                KeyCode::Char('t') => self.message = self.test_microphone(),
                _ => {}
            },
            2 => {
                let spec = &model::MODELS[self.model_idx];
                match code {
                    KeyCode::Left | KeyCode::Right => {
                        self.model_idx = (self.model_idx + 1) % model::MODELS.len();
                        self.message = model::MODELS[self.model_idx].note.into();
                    }
                    KeyCode::Enter => {
                        if model::is_installed(spec) {
                            self.cfg.model = spec.id.to_string();
                            self.persist();
                            self.message =
                                format!("{} selected. Used on the next recording.", spec.label);
                        } else if self.download.is_none() {
                            self.start_download(spec);
                        } else {
                            self.message = "A download is already running.".into();
                        }
                    }
                    KeyCode::Char('d') => {
                        if self.download.is_some() {
                            self.message = "Wait for the download to finish.".into();
                        } else if model::is_installed(spec) {
                            self.confirm = Some(Confirm::DeleteModel(spec));
                            self.message = format!(
                                "Press Enter to delete {} ({}). Esc cancels.",
                                spec.label,
                                util::human_size(spec.size)
                            );
                        }
                    }
                    _ => {}
                }
            }
            3 => {
                if let Some((label, codes)) = self.pending_confirm.clone() {
                    match code {
                        KeyCode::Enter => self.apply_shortcut(&codes, &label),
                        KeyCode::Esc => {
                            self.pending_confirm = None;
                            self.message = "Custom shortcut discarded.".into();
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                match code {
                    KeyCode::Left | KeyCode::Right => {
                        self.suggestion_idx = if code == KeyCode::Right {
                            (self.suggestion_idx + 1) % shortcuts::SUGGESTIONS.len()
                        } else {
                            (self.suggestion_idx + shortcuts::SUGGESTIONS.len() - 1)
                                % shortcuts::SUGGESTIONS.len()
                        };
                    }
                    KeyCode::Enter => {
                        let suggestion = &shortcuts::SUGGESTIONS[self.suggestion_idx];
                        self.apply_shortcut(suggestion.codes, suggestion.display);
                    }
                    KeyCode::Char('c') => {
                        if self.keyboard_permission {
                            self.capture_requested = true;
                            self.message = "Press the combination. Include Ctrl, Alt or Super. Names follow the US layout.".into();
                        } else {
                            self.message =
                                "Cannot read keys. Install the package, log out and back in, or pick a suggestion.".into();
                        }
                    }
                    KeyCode::Char('e') => {
                        self.refresh_existing();
                        self.show_existing = true;
                        self.body_scroll = 0;
                    }
                    KeyCode::Char('h') => {
                        self.cfg.hold_to_talk = !self.cfg.hold_to_talk;
                        self.persist();
                        self.message = format!(
                            "Mode: {}. Applies within 2 seconds.",
                            if self.cfg.hold_to_talk {
                                "hold to talk"
                            } else {
                                "press to start, press to stop"
                            }
                        );
                    }
                    KeyCode::Char('x') => {
                        self.cfg.hotkey_codes.clear();
                        self.cfg.hotkey = "unset".into();
                        self.persist();
                        self.message = "Shortcut cleared. Bind `verbaq toggle` in your desktop or pick a suggestion.".into();
                    }
                    other => {
                        if let KeyCode::Char(ch) = other {
                            if let Some(digit) = ch.to_digit(10) {
                                let index = digit as usize;
                                if index >= 1 && index <= shortcuts::SUGGESTIONS.len() {
                                    let suggestion = &shortcuts::SUGGESTIONS[index - 1];
                                    self.suggestion_idx = index - 1;
                                    self.apply_shortcut(suggestion.codes, suggestion.display);
                                }
                            }
                        }
                    }
                }
            }
            4 => match code {
                KeyCode::Char('s') => {
                    self.cfg.sound = !self.cfg.sound;
                    self.persist();
                    self.message = format!("Sounds {}.", on_off(self.cfg.sound));
                }
                KeyCode::Char('t') => {
                    if self.cfg.sound {
                        feedback::play_async(Cue::Test);
                        self.message = "Test sound played.".into();
                    } else {
                        self.message = "Sounds are off. Press s to turn them on.".into();
                    }
                }
                _ => {}
            },
            5 => match code {
                KeyCode::Char('p') => {
                    self.cfg.paste_method = if self.cfg.paste_method == "ctrl_v" {
                        "shift_insert".into()
                    } else {
                        "ctrl_v".into()
                    };
                    self.persist();
                    self.message = format!("Insert method: {}.", insert_label(&self.cfg));
                }
                KeyCode::Enter => {
                    let uinput = std::fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/uinput")
                        .is_ok();
                    if inject::copy_only("VerbaQ insert test: the clipboard works.") {
                        self.message = if uinput {
                            "Clipboard and typing look ready. Test text is on the clipboard.".into()
                        } else {
                            "Clipboard works, but /dev/uinput is not writable. Text can only be copied.".into()
                        };
                    } else {
                        self.message = "Could not write to the clipboard.".into();
                    }
                }
                _ => {}
            },
            6 => match code {
                KeyCode::Char('h') => {
                    self.cfg.history = !self.cfg.history;
                    self.persist();
                    self.message = format!("Transcript history {}.", on_off(self.cfg.history));
                }
                KeyCode::Char('c') => {
                    if self.history.is_empty() && !util::history_path().exists() {
                        self.message = "History is already empty.".into();
                    } else {
                        self.confirm = Some(Confirm::ClearHistory);
                        self.message = "Press Enter to clear all history. Esc cancels.".into();
                    }
                }
                KeyCode::Char('r') => {
                    self.history = load_history();
                    self.message = "History reloaded.".into();
                }
                _ => {}
            },
            7 => {}
            _ => {}
        }
        Ok(())
    }

    fn test_microphone(&mut self) -> String {
        match audio::Recorder::start(self.cfg.mic_option()) {
            Ok(recorder) => {
                std::thread::sleep(Duration::from_secs(3));
                let (samples, _) = recorder.stop();
                let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                if peak < 0.01 {
                    "3 second test: silence. Try another microphone or check mute.".into()
                } else {
                    format!(
                        "3 second test: microphone hears you (peak {:.0}%).",
                        peak * 100.0
                    )
                }
            }
            Err(err) => format!("Microphone error: {err}"),
        }
    }

    fn apply_capture(&mut self, result: Result<Vec<u16>>) {
        match result {
            Ok(codes) => {
                let label = hotkey::combo_label(&codes);
                self.message = format!(
                    "Detected {label}. Enter uses it, Esc discards it. Press c to try again."
                );
                self.pending_confirm = Some((label, codes));
            }
            Err(err) => {
                self.message = if err.to_string() == "cancelled" {
                    "Shortcut capture cancelled.".into()
                } else {
                    format!("Capture failed: {err}")
                };
            }
        }
    }

    fn context_help(&self) -> String {
        if self.show_help {
            return "PgUp/PgDn scroll   Esc or ? close".into();
        }
        if self.show_existing {
            return "Up/Down or j/k scroll   Esc or e back".into();
        }
        if self.confirm.is_some() {
            return "Enter confirm   Esc cancel".into();
        }
        match self.section {
            0 => "s start   x stop   a start at login   Enter refresh".into(),
            1 => "Left/Right device   Enter level meter   t 3 second test".into(),
            2 => "Left/Right model   Enter select or download   d delete".into(),
            3 => "Left/Right suggestion   Enter choose   c custom   e list   h mode".into(),
            4 => "s sounds on/off   t test".into(),
            5 => "p switch method   Enter check clipboard and typing".into(),
            6 => "h history on/off   c clear   r reload".into(),
            _ => "Enter run the health check".into(),
        }
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(4),
            ])
            .split(frame.area());

        let state = self
            .status_text
            .lines()
            .find_map(|line| line.strip_prefix("state: "))
            .unwrap_or("");
        let service = if self.running {
            if state.is_empty() || state == "idle" {
                "running".to_string()
            } else {
                format!("running ({state})")
            }
        } else {
            "stopped".to_string()
        };
        let (dot, dot_color) = if self.running {
            ("●", Color::Green)
        } else {
            ("○", Color::Red)
        };
        let mode = if self.cfg.hold_to_talk {
            "hold"
        } else {
            "toggle"
        };
        let header = Line::from(vec![
            Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
            Span::styled(
                service,
                Style::default().fg(dot_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw("    "),
            Span::styled("Model ", Style::default().fg(Color::DarkGray)),
            Span::raw(self.cfg.model.clone()),
            Span::raw("   "),
            Span::styled("Shortcut ", Style::default().fg(Color::DarkGray)),
            Span::raw(self.cfg.hotkey.clone()),
            Span::raw("   "),
            Span::styled("Mode ", Style::default().fg(Color::DarkGray)),
            Span::raw(mode),
        ]);
        frame.render_widget(
            Paragraph::new(header).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(format!(" VerbaQ {} ", util::version())),
            ),
            chunks[0],
        );

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(20), Constraint::Min(40)])
            .split(chunks[1]);

        let menu: Vec<Line> = SECTIONS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let glyph = GLYPHS.get(index).copied().unwrap_or("·");
                if index == self.section {
                    Line::from(vec![
                        Span::styled("▸ ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            format!("{glyph} {name}"),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{glyph} {name}"), Style::default().fg(Color::Gray)),
                    ])
                }
            })
            .collect();
        frame.render_widget(
            Paragraph::new(menu).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" Sections "),
            ),
            columns[0],
        );

        let title = if self.show_help {
            " Help ".to_string()
        } else if self.show_existing {
            " Shortcuts on this system ".to_string()
        } else if self.confirm.is_some() {
            " Confirm action ".to_string()
        } else {
            format!(" {} ", SECTIONS[self.section])
        };
        let mut lines = if self.show_help {
            help_body()
        } else {
            self.body()
        };
        if let Some(action) = self.confirm {
            let mut warning = vec![
                Line::from(Span::styled(
                    confirmation_text(action),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            warning.extend(lines);
            lines = warning;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title);
        let inner = block.inner(columns[1]);
        let visible = inner.height as usize;
        let maximum = lines.len().saturating_sub(visible);
        let start = self.body_scroll.min(maximum);
        let view: Vec<Line> = lines.iter().skip(start).take(visible).cloned().collect();
        frame.render_widget(
            Paragraph::new(view).block(block).wrap(Wrap { trim: false }),
            columns[1],
        );
        if visible > 0 && lines.len() > visible {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None);
            let mut scroll_state = ScrollbarState::new(lines.len())
                .position(start)
                .viewport_content_length(visible);
            frame.render_stateful_widget(scrollbar, inner, &mut scroll_state);
        }

        let primary = if let Some(error) = &self.save_error {
            Line::from(Span::styled(
                format!("Could not save settings: {error}"),
                Style::default().fg(Color::Red),
            ))
        } else if self.message.is_empty() {
            Line::from(self.context_help())
        } else {
            Line::from(self.message.clone())
        };
        let footer = vec![
            primary,
            Line::from(Span::styled(
                "Up/Down section   PgUp/PgDn scroll   ? help   Ctrl+C quit",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        frame.render_widget(
            Paragraph::new(footer).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" message / keys "),
            ),
            chunks[2],
        );
    }

    fn body(&self) -> Vec<Line<'static>> {
        if self.show_existing {
            return self.existing_body();
        }
        let mut lines: Vec<Line<'static>> = Vec::new();
        match self.section {
            0 => {
                let pid = self
                    .status_text
                    .lines()
                    .find_map(|line| line.strip_prefix("pid: "))
                    .unwrap_or("");
                let state = self
                    .status_text
                    .lines()
                    .find_map(|line| line.strip_prefix("state: "))
                    .unwrap_or(if self.running { "starting" } else { "stopped" });
                let (dot, color, label) = if self.running {
                    ("●", Color::Green, "RUNNING")
                } else {
                    ("○", Color::Red, "STOPPED")
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{dot} "), Style::default().fg(color)),
                    Span::styled(
                        label,
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(format!("state {state}"), Style::default().fg(Color::Gray)),
                    if pid.is_empty() {
                        Span::raw("")
                    } else {
                        Span::styled(
                            format!("    pid {pid}"),
                            Style::default().fg(Color::DarkGray),
                        )
                    },
                ]));
                lines.push(rule());
                lines.push(dim(
                    "One small process waits for the shortcut, records, transcribes and",
                ));
                lines.push(dim(
                    "pastes. Without it the shortcut does nothing. It starts at login if",
                ));
                lines.push(dim("you turned that on, and runs until you stop it."));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("Start at login  ", Style::default().fg(Color::DarkGray)),
                    if service::autostart_enabled() {
                        Span::styled("● on", Style::default().fg(Color::Green))
                    } else {
                        Span::styled("○ off", Style::default().fg(Color::Yellow))
                    },
                ]));
                lines.push(Line::from(""));
                lines.push(plain("Quick actions"));
                lines.push(plain("  s   start the service now"));
                lines.push(plain("  x   stop the service now"));
                lines.push(plain("  a   toggle start at login"));
                lines.push(Line::from(""));
                lines.push(plain("Commands"));
                lines.push(plain("  verbaq start    start now"));
                lines.push(plain("  verbaq stop     stop now"));
                lines.push(plain("  verbaq status   state and pid"));
                lines.push(plain(
                    "  verbaq daemon   run in this terminal for debugging",
                ));
                lines.push(plain("  verbaq toggle   start or stop listening"));
                lines.push(Line::from(""));
                lines.push(rule());
                lines.push(Line::from(vec![
                    Span::styled("Model ", Style::default().fg(Color::DarkGray)),
                    Span::raw(self.cfg.model.clone()),
                    Span::raw("    "),
                    Span::styled("Shortcut ", Style::default().fg(Color::DarkGray)),
                    Span::raw(self.cfg.hotkey.clone()),
                    Span::raw("    "),
                    Span::styled("Mode ", Style::default().fg(Color::DarkGray)),
                    Span::raw(if self.cfg.hold_to_talk {
                        "hold"
                    } else {
                        "toggle"
                    }),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("Microphone ", Style::default().fg(Color::DarkGray)),
                    Span::raw(if self.cfg.mic.is_empty() {
                        "system default".to_string()
                    } else {
                        self.cfg.mic.clone()
                    }),
                ]));
            }
            1 => {
                lines.push(Line::from(vec![
                    Span::styled("◉ ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "Microphone",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(dim("VerbaQ records only while the shortcut is held."));
                lines.push(dim(
                    "Pick the device that hears you best. The default is usually right.",
                ));
                lines.push(rule());
                if self.devices.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "✗ No input devices found.",
                        Style::default().fg(Color::Red),
                    )));
                } else {
                    for (index, name) in self.devices.iter().enumerate() {
                        let selected = index == self.device_idx;
                        lines.push(Line::from(vec![
                            Span::raw(if selected { "▸ " } else { "  " }),
                            Span::styled(
                                if selected { "● " } else { "○ " },
                                Style::default().fg(if selected {
                                    Color::Cyan
                                } else {
                                    Color::DarkGray
                                }),
                            ),
                            Span::styled(
                                name.clone(),
                                if selected {
                                    Style::default()
                                        .fg(Color::Cyan)
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default()
                                },
                            ),
                        ]));
                    }
                }
                lines.push(Line::from(""));
                if let Some(meter) = &self.meter {
                    let level = (meter.level() * 8.0).clamp(0.0, 1.0);
                    let filled = (level * 28.0) as usize;
                    let percent = (level * 100.0) as u32;
                    lines.push(Line::from(vec![
                        Span::styled("Level  ", Style::default().fg(Color::DarkGray)),
                        Span::styled("█".repeat(filled), Style::default().fg(Color::Green)),
                        Span::styled(
                            "░".repeat(28 - filled),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(format!("  {percent:>3}%")),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("Level  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            "meter off, press Enter to start",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
                lines.push(dim("Enter: level meter.  t: 3 second test."));
            }
            2 => {
                lines.push(Line::from(vec![
                    Span::styled("◆ ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "Voice model",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(dim(
                    "Smaller is faster. Larger is more accurate on harder speech.",
                ));
                lines.push(dim(
                    "One model is loaded at a time. Downloads are verified.",
                ));
                lines.push(rule());
                for (index, spec) in model::MODELS.iter().enumerate() {
                    let selected = index == self.model_idx;
                    let installed = model::is_installed(spec);
                    let downloading = self
                        .download
                        .as_ref()
                        .map(|download| download.spec.id == spec.id)
                        .unwrap_or(false);
                    let (dot, state, color) = if installed {
                        ("●", "installed", Color::Green)
                    } else if downloading {
                        ("◌", "downloading", Color::Yellow)
                    } else {
                        ("○", "not installed", Color::DarkGray)
                    };
                    let mut spans = vec![
                        Span::raw(if selected { "▸ " } else { "  " }),
                        Span::styled(dot, Style::default().fg(color)),
                        Span::raw(" "),
                        Span::styled(
                            format!("{:<6}", spec.label),
                            if selected {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(
                            format!("{:>8}   ", util::human_size(spec.size)),
                            Style::default().fg(Color::Gray),
                        ),
                        Span::styled(state, Style::default().fg(color)),
                    ];
                    if self.cfg.model == spec.id {
                        spans.push(Span::styled(
                            "   selected",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    lines.push(Line::from(spans));
                    lines.push(dim(format!("      {}", spec.note)));
                }
                lines.push(Line::from(""));
                if let Some(download) = &self.download {
                    let pct = (download.done.saturating_mul(100) / download.total.max(1)).min(100);
                    let filled = (pct / 2) as usize;
                    lines.push(Line::from(vec![
                        Span::raw(format!("{}  ", download.spec.label)),
                        Span::styled("█".repeat(filled), Style::default().fg(Color::Cyan)),
                        Span::styled(
                            "░".repeat(50 - filled),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(format!("  {pct:>3}%")),
                    ]));
                } else {
                    lines.push(dim("Enter: install or select.  d: delete."));
                }
            }
            3 => {
                lines.push(plain("Hold this key combination to talk"));
                lines.push(dim(
                    "Pick something free. VerbaQ checks the shortcuts it can read on this",
                ));
                lines.push(dim(
                    "system, but an application can still claim a key later. You can change",
                ));
                lines.push(dim("it any time from this screen."));
                lines.push(Line::from(""));
                lines.push(plain(format!("Current:  {}", self.cfg.hotkey)));
                lines.push(plain(format!(
                    "Mode:     {}   (h to change)",
                    if self.cfg.hold_to_talk {
                        "hold to talk"
                    } else {
                        "press to start, press to stop"
                    }
                )));
                lines.push(dim(format!(
                    "Checked {} shortcuts on this system.",
                    self.existing.len()
                )));
                lines.push(Line::from(""));
                lines.push(rule());
                lines.push(plain("Suggestions, easy with the left hand:"));

                if let Some((label, _)) = &self.pending_confirm {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        format!("Detected {label}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )));
                    lines.push(dim("Enter: use it.  Esc: discard.  c: try again."));
                    return lines;
                }

                for (i, suggestion) in shortcuts::SUGGESTIONS.iter().enumerate() {
                    let selected = i == self.suggestion_idx;
                    let taken = shortcuts::taken_by_in(&self.existing, suggestion.display);
                    let mut spans = vec![Span::raw(format!(
                        "{} {}. ",
                        if selected { "▸" } else { " " },
                        i + 1
                    ))];
                    spans.push(Span::styled(
                        format!("{:<16}", suggestion.display),
                        if selected {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ));
                    spans.push(ok_mark(taken.is_none()));
                    if let Some(source) = taken {
                        spans.push(Span::styled(
                            format!("  ({source})"),
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                    lines.push(Line::from(spans));
                    lines.push(dim(format!("     {}", suggestion.why)));
                }
                lines.push(Line::from(""));
                lines.push(dim(
                    "Enter: use highlighted.  1-6: pick directly.  c: record custom.",
                ));
                lines.push(dim("e: list every shortcut found.  x: clear."));
                if !self.keyboard_permission {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "Built-in shortcut is unavailable: keyboards are not readable.",
                        Style::default().fg(Color::Yellow),
                    )));
                    lines.push(dim("You can still bind `verbaq toggle` in your desktop."));
                }
            }
            4 => {
                lines.push(plain("Sound is the only feedback"));
                lines.push(dim(
                    "VerbaQ does not use notifications. They can be hidden by fullscreen",
                ));
                lines.push(dim(
                    "apps or Do Not Disturb, which made them unreliable. Sounds always work.",
                ));
                lines.push(Line::from(""));
                lines.push(plain("Start       rising tick"));
                lines.push(plain("Release     short click, transcription has begun"));
                lines.push(plain(
                    "Busy        very soft low tick, the shortcut was ignored",
                ));
                lines.push(plain(
                    "Problem     two low beeps, then the text is on the clipboard",
                ));
                lines.push(Line::from(""));
                lines.push(plain(format!(
                    "Short sounds: {}   (s)",
                    on_off(self.cfg.sound)
                )));
                lines.push(Line::from(""));
                lines.push(dim("t: play a test sound."));
                lines.push(Line::from(""));
                lines.push(dim(format!(
                    "Player: {}",
                    feedback::player().unwrap_or("none found, install pipewire or pulseaudio")
                )));
            }
            5 => {
                lines.push(plain("How the text gets into the other application"));
                lines.push(dim(
                    "VerbaQ copies the text, presses a paste key, then puts your previous",
                ));
                lines.push(dim(
                    "clipboard back. Nothing is typed letter by letter, so accents, emoji",
                ));
                lines.push(dim("and every keyboard layout work the same way."));
                lines.push(Line::from(""));
                lines.push(plain(format!("Method: {}", insert_title(&self.cfg))));
                lines.push(dim(insert_explanation(&self.cfg)));
                lines.push(Line::from(""));
                lines.push(dim("p: switch method.  Enter: check clipboard and typing."));
                lines.push(Line::from(""));
                lines.push(dim(
                    "If neither method works in an app, the text stays on the clipboard.",
                ));
            }
            6 => {
                lines.push(plain("Transcript history"));
                lines.push(dim(
                    "When on, VerbaQ keeps the last transcripts in a local file, so you can",
                ));
                lines.push(dim(
                    "recover text when a paste failed, or look up what you said earlier.",
                ));
                lines.push(dim("It stays on this machine and is never uploaded."));
                lines.push(Line::from(""));
                lines.push(plain(format!(
                    "Keep history: {}   (h)",
                    on_off(self.cfg.history)
                )));
                lines.push(plain(format!(
                    "Limit:        {} transcripts",
                    self.cfg.history_limit
                )));
                lines.push(dim(format!(
                    "File:         {}",
                    util::history_path().display()
                )));
                lines.push(Line::from(""));
                if !self.cfg.history {
                    lines.push(dim("History is off. Nothing is written."));
                } else if self.history.is_empty() {
                    lines.push(dim("No transcripts yet."));
                } else {
                    for line in self.history.iter().rev().take(10) {
                        lines.push(plain(format!("  {line}")));
                    }
                }
                lines.push(Line::from(""));
                lines.push(dim("c: clear history.  r: reload."));
            }
            _ => {
                lines.push(plain("Checks every piece VerbaQ needs"));
                lines.push(dim("Microphone, typing permission, shortcut, sound,"));
                lines.push(dim("clipboard, the background service and the model file."));
                lines.push(Line::from(""));
                if self.health.is_empty() {
                    lines.push(dim("Enter: run the check."));
                } else {
                    for (ok, label, detail) in &self.health {
                        lines.push(Line::from(vec![
                            Span::styled(
                                if *ok { "✓ " } else { "✗ " },
                                Style::default()
                                    .fg(if *ok { Color::Green } else { Color::Red })
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{:<22}", label),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(detail.replace('\n', " ")),
                        ]));
                    }
                    lines.push(Line::from(""));
                    lines.push(dim("Enter: run again."));
                }
            }
        }
        lines
    }

    fn existing_body(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(plain(format!(
            "{} shortcuts found in KDE, GNOME, sway, i3 or Hyprland settings.",
            self.existing.len()
        )));
        lines.push(Line::from(""));
        for entry in &self.existing {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<24}", entry.combo),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(entry.source.clone()),
            ]));
        }
        if self.existing.is_empty() {
            lines.push(dim(
                "Nothing readable. Your desktop may store shortcuts elsewhere.",
            ));
        }
        lines
    }
}

fn confirmation_text(action: Confirm) -> String {
    match action {
        Confirm::DeleteModel(spec) => format!(
            "Delete {} ({} on disk)?",
            spec.label,
            util::human_size(spec.size)
        ),
        Confirm::ClearHistory => "Clear every saved transcript? This cannot be undone.".into(),
    }
}

fn help_body() -> Vec<Line<'static>> {
    vec![
        plain("Moving around"),
        plain("  Up / Down or Tab       next section"),
        plain("  Left / Right           choose within a section"),
        plain("  Enter                  use the highlighted choice"),
        plain("  PgUp / PgDn            scroll a long section"),
        plain("  ?                      open or close this help"),
        plain("  Ctrl+C or q            quit"),
        Line::from(""),
        plain("Service"),
        plain("  s start   x stop   a start at login   Enter refresh"),
        Line::from(""),
        plain("Microphone"),
        plain("  Left/Right device   Enter level meter   t 3 second test"),
        Line::from(""),
        plain("Voice model"),
        plain("  Left/Right model   Enter select or download   d delete"),
        Line::from(""),
        plain("Shortcut"),
        plain("  Left/Right suggestion   Enter choose   c custom   e list   h mode"),
        Line::from(""),
        plain("Sound"),
        plain("  s on/off   t test"),
        Line::from(""),
        plain("Text insertion"),
        plain("  p switch method   Enter check clipboard and typing"),
        Line::from(""),
        plain("History"),
        plain("  h on/off   c clear   r reload"),
        Line::from(""),
        plain("Health"),
        plain("  Enter run the check"),
    ]
}

fn insert_title(cfg: &Config) -> String {
    if cfg.paste_method == "ctrl_v" {
        "Ctrl+V".into()
    } else {
        "Shift+Insert".into()
    }
}

fn insert_label(cfg: &Config) -> String {
    if cfg.paste_method == "ctrl_v" {
        "Ctrl+V (alternative)".into()
    } else {
        "Shift+Insert (recommended)".into()
    }
}

fn insert_explanation(cfg: &Config) -> String {
    if cfg.paste_method == "ctrl_v" {
        "Ctrl+V works in most graphical apps but not in terminals.".into()
    } else {
        "Shift+Insert pastes in terminals, browsers, editors and most other apps.".into()
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

fn load_history() -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(util::history_path()) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let (stamp, text) = line.split_once('\t')?;
            let secs: i64 = stamp.parse().ok()?;
            let time = chrono::Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.format("%m-%d %H:%M").to_string())
                .unwrap_or_default();
            Some(format!("{time}  {text}"))
        })
        .collect()
}

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    if !model::MODELS.iter().any(model::is_installed) {
        eprintln!("No voice model installed. Run `verbaq setup` first.");
        return Ok(());
    }
    drop(cfg);

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();

    let result = (|| -> Result<()> {
        loop {
            app.tick();
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::from_millis(150))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        break;
                    }
                    if app.on_key(key.code)? {
                        break;
                    }
                    if app.capture_requested {
                        app.capture_requested = false;
                        terminal.draw(|frame| app.draw(frame))?;
                        let result = hotkey::capture();
                        app.apply_capture(result);
                    }
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    app.stop_meter();
    result
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_meter();
    }
}
