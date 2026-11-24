mod audio;
mod config;
mod daemon;
mod doctor;
mod feedback;
mod hotkey;
mod inject;
mod ipc;
mod log;
mod model;
mod service;
mod setup;
mod shortcuts;
mod stt;
mod tui;
mod util;

use anyhow::{bail, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::io::Write;

#[derive(Parser)]
#[command(
    name = "verbaq",
    about = "Local dictation for Linux. Press a key, speak, text appears."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// First-run wizard: download a model, pick a microphone, set the shortcut
    Setup,
    /// Start the background service now
    Start,
    /// Stop the background service
    Stop,
    /// Run the service in this terminal, for debugging (Ctrl+C stops it)
    Daemon,
    /// Open the settings screen
    Tui,
    /// Start or stop listening now
    Toggle,
    /// Show what the service is doing
    Status,
    /// Check every dependency and permission
    Doctor {
        /// Also copy a paste test string to the clipboard
        #[arg(long)]
        paste: bool,
    },
    /// Manage voice models
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Print the licence
    License,
}

#[derive(Subcommand)]
enum ModelCmd {
    /// List available and installed models
    List,
    /// Download and select a model (small or turbo)
    Install { id: String },
    /// Delete a downloaded model
    Remove { id: String },
}

fn main() {
    // Behave like a normal Unix tool when piped into head or less.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(err) = run() {
        eprintln!("verbaq: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    whisper_rs::install_logging_hooks();
    let matches = Cli::command().version(util::version()).get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    match cli.command {
        None | Some(Command::Tui) => tui::run(),
        Some(Command::Setup) => setup::run(),
        Some(Command::Start) => {
            println!("{}", service::start()?);
            Ok(())
        }
        Some(Command::Stop) => {
            println!("{}", service::stop()?);
            Ok(())
        }
        Some(Command::Daemon) => daemon::run(),
        Some(Command::Toggle) => {
            if !ipc::is_running() {
                bail!("the background service is not running. Start it with `verbaq start`.");
            }
            ipc::send("toggle")?;
            Ok(())
        }
        Some(Command::Status) => {
            println!("verbaq {}", util::version());
            let autostart = if service::autostart_enabled() {
                "on"
            } else {
                "off"
            };
            if !ipc::is_running() {
                println!("Service: stopped");
                println!("Start at login: {autostart}");
                println!();
                println!("Start it now with `verbaq start`.");
                println!("Or open the settings screen with `verbaq tui`.");
                return Ok(());
            }
            println!("Service: running");
            if let Ok(status) = ipc::send("status") {
                for line in status.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    println!("  {line}");
                }
            }
            println!("Start at login: {autostart}");
            Ok(())
        }
        Some(Command::Doctor { paste }) => {
            doctor::print(paste);
            Ok(())
        }
        Some(Command::Model { cmd }) => match cmd {
            ModelCmd::List => {
                for spec in model::MODELS {
                    let state = if model::is_installed(spec) {
                        format!("installed at {}", model::path(spec).display())
                    } else {
                        "not installed".to_string()
                    };
                    println!(
                        "{:<6} {:>8}  {}\n       {}",
                        spec.label,
                        util::human_size(spec.size),
                        state,
                        spec.note
                    );
                }
                Ok(())
            }
            ModelCmd::Install { id } => {
                let spec = model::find(&id)
                    .ok_or_else(|| anyhow::anyhow!("unknown model '{id}'. Use small or turbo."))?;
                if model::is_installed(spec) {
                    println!("{} is already installed.", spec.label);
                    return Ok(());
                }
                util::ensure_dirs()?;
                println!(
                    "Downloading {} ({})",
                    spec.label,
                    util::human_size(spec.size)
                );
                model::download(spec, |done, total| {
                    let pct = (done.saturating_mul(100) / total.max(1)).min(100);
                    let filled = (pct / 2) as usize;
                    print!(
                        "\r[{}{}] {pct:>3}%  {}",
                        "#".repeat(filled),
                        ".".repeat(50 - filled),
                        util::human_size(done)
                    );
                    let _ = std::io::stdout().flush();
                })?;
                println!("\nInstalled {}.", spec.label);
                let mut cfg = config::Config::load()?;
                cfg.model = spec.id.to_string();
                cfg.save()?;
                Ok(())
            }
            ModelCmd::Remove { id } => {
                let spec = model::find(&id)
                    .ok_or_else(|| anyhow::anyhow!("unknown model '{id}'. Use small or turbo."))?;
                model::remove(spec)?;
                println!("Removed {}.", spec.label);
                Ok(())
            }
        },
        Some(Command::License) => {
            println!("{}", include_str!("../LICENSE"));
            Ok(())
        }
    }
}
