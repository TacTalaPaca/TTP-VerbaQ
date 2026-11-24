use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

pub const APP: &str = "verbaq";
const VERSION_FILE: &str = include_str!("../VERSION");

pub fn version() -> &'static str {
    VERSION_FILE.trim()
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join(APP)
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join(APP)
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| data_dir().join("state"))
        .join(APP)
}

pub fn history_path() -> PathBuf {
    state_dir().join("history.jsonl")
}

pub fn autostart_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("autostart")
        .join("verbaq.desktop")
}

pub fn socket_path() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(runtime);
        if dir.is_dir() {
            return dir.join("verbaq.sock");
        }
    }
    PathBuf::from(format!("/tmp/verbaq-{}.sock", unsafe { libc::getuid() }))
}

pub fn ensure_dirs() -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::create_dir_all(models_dir())?;
    Ok(())
}

pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (MB * 1024.0))
    } else {
        format!("{:.0} MB", bytes as f64 / MB)
    }
}

/// Locate an executable on PATH without spawning `which`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&candidate) {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return Some(candidate);
                    }
                }
            }
            #[cfg(not(unix))]
            return Some(candidate);
        }
    }
    None
}

pub fn session_type() -> String {
    std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into())
}

pub fn desktop_name() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into())
}

pub fn is_wayland() -> bool {
    session_type().eq_ignore_ascii_case("wayland")
}

/// Trim and collapse whitespace, drop bracketed non-speech noise.
pub fn clean_transcript(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find(['[', '(']) {
        out.push_str(&rest[..start]);
        let open = rest.as_bytes()[start] as char;
        let close = if open == '[' { ']' } else { ')' };
        match rest[start..].find(close) {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let cleaned = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().all(|c| !c.is_alphanumeric()) {
        String::new()
    } else {
        cleaned
    }
}

/// Wait for a child without letting a broken desktop tool hang the app.
pub fn wait_for_process(child: &mut Child, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("command timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run a command with bounded wall time and captured output. Intended for
/// small desktop queries such as gsettings, not large streams.
pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_process(&mut child, timeout)?;
    Ok(child.wait_with_output()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_noise_and_spacing() {
        assert_eq!(clean_transcript("  hello   world "), "hello world");
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(
            clean_transcript("(music) salut (laughs) lume"),
            "salut lume"
        );
        assert_eq!(clean_transcript("..."), "");
    }

    #[test]
    fn command_timeout_is_bounded() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        assert!(run_with_timeout(&mut command, Duration::from_millis(50)).is_err());
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(181 * 1024 * 1024), "181 MB");
    }
}
