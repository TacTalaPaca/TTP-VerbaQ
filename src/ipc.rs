use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hotkey::HotkeyEvent;
use crate::util;

#[derive(Debug, Clone)]
pub enum Cmd {
    Toggle,
    Start,
    Stop,
    Cancel,
    Status,
    Ping,
    Shutdown,
}

pub fn parse(input: &str) -> Option<Cmd> {
    match input.trim() {
        "toggle" => Some(Cmd::Toggle),
        "start" => Some(Cmd::Start),
        "stop" => Some(Cmd::Stop),
        "cancel" => Some(Cmd::Cancel),
        "status" => Some(Cmd::Status),
        "ping" => Some(Cmd::Ping),
        "shutdown" => Some(Cmd::Shutdown),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: String,
    pub model: String,
    pub microphone: String,
    pub last: String,
    pub shortcut: String,
    pub mode: String,
    pub pid: u32,
    pub updated: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            model: "small".into(),
            microphone: "default".into(),
            last: String::new(),
            shortcut: "unset".into(),
            mode: "hold to talk".into(),
            pid: 0,
            updated: 0,
        }
    }
}

impl Snapshot {
    pub fn render(&self) -> String {
        let model = if self.model.is_empty() {
            "small".to_string()
        } else {
            self.model.clone()
        };
        format!(
            "state: {}\npid: {}\nmodel: {}\nmicrophone: {}\nshortcut: {}\nmode: {}\nlast: {}",
            self.state, self.pid, model, self.microphone, self.shortcut, self.mode, self.last
        )
    }
}

pub fn send(command: &str) -> Result<String> {
    let path = util::socket_path();
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("verbaq service is not running ({})", path.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    writeln!(stream, "{command}")?;
    let mut response = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_to_string(&mut response)?;
    Ok(response.trim().to_string())
}

pub fn is_running() -> bool {
    send("ping").is_ok()
}

pub fn serve(
    events: std::sync::mpsc::Sender<HotkeyEvent>,
    status: Arc<Mutex<Snapshot>>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let path = util::socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("another verbaq daemon is already running");
        }
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("cannot listen on {}", path.display()))?;
    restrict_permissions(&path);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = handle(stream, status.clone(), events.clone(), shutdown.clone());
        }
    });
    Ok(())
}

fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

fn handle(
    stream: UnixStream,
    status: Arc<Mutex<Snapshot>>,
    events: std::sync::mpsc::Sender<HotkeyEvent>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.by_ref().take(256).read_line(&mut line)?;
    let Some(cmd) = parse(&line) else {
        let mut stream = stream;
        writeln!(stream, "unknown command")?;
        return Ok(());
    };
    let mut stream = stream;
    match cmd {
        Cmd::Ping => writeln!(stream, "pong")?,
        Cmd::Status => {
            let snapshot = status.lock().map(|s| s.clone()).unwrap_or_default();
            write!(stream, "{}", snapshot.render())?;
        }
        Cmd::Start => {
            let _ = events.send(HotkeyEvent::Start);
            writeln!(stream, "ok")?;
        }
        Cmd::Stop => {
            let _ = events.send(HotkeyEvent::Stop);
            writeln!(stream, "ok")?;
        }
        Cmd::Toggle => {
            let _ = events.send(HotkeyEvent::Toggle);
            writeln!(stream, "ok")?;
        }
        Cmd::Cancel => {
            let _ = events.send(HotkeyEvent::Cancel);
            writeln!(stream, "ok")?;
        }
        Cmd::Shutdown => {
            shutdown.store(true, Ordering::Relaxed);
            writeln!(stream, "ok")?;
        }
    }
    Ok(())
}

pub fn reset_status(status: &Arc<Mutex<Snapshot>>, state: &str, last: Option<&str>) {
    if let Ok(mut snapshot) = status.lock() {
        snapshot.state = state.to_string();
        if let Some(last) = last {
            snapshot.last = last.to_string();
        }
        snapshot.updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
    }
}
