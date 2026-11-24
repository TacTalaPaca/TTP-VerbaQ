use anyhow::{bail, Result};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::ipc;
use crate::util;

/// Start the background service detached from this terminal.
pub fn start() -> Result<String> {
    if ipc::is_running() {
        return Ok("VerbaQ is already running.".into());
    }
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach so closing the terminal does not stop the service.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if ipc::is_running() {
            // Reap the detached child so the caller never accumulates zombies.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(format!("Started the background service (pid {pid})."));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "the service did not come up. Check {}",
        util::state_dir().join("verbaq.log").display()
    )
}

/// Ask the running service to exit and wait for it to disappear.
pub fn stop() -> Result<String> {
    if !ipc::is_running() {
        return Ok("VerbaQ is not running.".into());
    }
    let _ = ipc::send("shutdown");
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if !ipc::is_running() {
            return Ok("Stopped the background service.".into());
        }
    }
    bail!(
        "the service did not stop. Check {}",
        util::state_dir().join("verbaq.log").display()
    )
}

pub fn autostart_enabled() -> bool {
    util::autostart_path().exists()
}

fn desktop_exec(path: &str) -> String {
    if path
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '$' | '`' | '\''))
    {
        let escaped = path
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`");
        format!("\"{escaped}\"")
    } else {
        path.to_string()
    }
}

pub fn autostart_enable() -> Result<()> {
    let path = util::autostart_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let executable = desktop_exec(&std::env::current_exe()?.display().to_string());
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=VerbaQ\n\
         Comment=Local dictation service\n\
         Exec={executable} daemon\n\
         Icon=verbaq-dictation\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    std::fs::write(&path, content)?;
    Ok(())
}

pub fn autostart_disable() {
    let _ = std::fs::remove_file(util::autostart_path());
}
