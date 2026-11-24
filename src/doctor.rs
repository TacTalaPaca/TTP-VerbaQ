use crate::config::Config;
use crate::feedback;
use crate::hotkey;
use crate::inject;
use crate::ipc;
use crate::model;
use crate::shortcuts;
use crate::util;

pub struct Check {
    pub ok: bool,
    pub label: String,
    pub detail: String,
}

pub fn run(verbose_paste: bool) -> Vec<Check> {
    let mut checks = Vec::new();

    let cfg = Config::load();
    checks.push(match &cfg {
        Ok(_) => Check {
            ok: true,
            label: "Configuration".into(),
            detail: Config::path().display().to_string(),
        },
        Err(err) => Check {
            ok: false,
            label: "Configuration".into(),
            detail: err.to_string(),
        },
    });
    let cfg = cfg.unwrap_or_default();

    let spec = model::find(&cfg.model);
    match spec {
        Some(spec) if model::is_installed(spec) => {
            let size = std::fs::metadata(model::path(spec))
                .map(|m| util::human_size(m.len()))
                .unwrap_or_default();
            checks.push(Check {
                ok: true,
                label: format!("Voice model ({})", spec.label),
                detail: format!("{} installed", size),
            });
        }
        Some(spec) => checks.push(Check {
            ok: false,
            label: format!("Voice model ({})", spec.label),
            detail: "not installed. Run `verbaq setup`.".into(),
        }),
        None => checks.push(Check {
            ok: false,
            label: "Voice model".into(),
            detail: "unknown model in config".into(),
        }),
    }

    let mics = crate::audio::input_devices();
    checks.push(Check {
        ok: !mics.is_empty(),
        label: "Microphone".into(),
        detail: if mics.is_empty() {
            "no input devices found".into()
        } else {
            format!(
                "{} device(s), using {}",
                mics.len(),
                if cfg.mic.is_empty() {
                    "default".to_string()
                } else {
                    cfg.mic.clone()
                }
            )
        },
    });

    let uinput_ok = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok();
    checks.push(Check {
        ok: uinput_ok,
        label: "Typing permission".into(),
        detail: if uinput_ok {
            "/dev/uinput is writable".into()
        } else {
            "no access to /dev/uinput. Install the package udev rule, then log out and back in."
                .into()
        },
    });

    let keyboards = hotkey::readable_keyboards();
    checks.push(Check {
        ok: keyboards > 0,
        label: "Shortcut permission".into(),
        detail: if keyboards > 0 {
            format!("{keyboards} keyboard device(s) readable")
        } else {
            "keyboards are not readable. Bind `verbaq toggle` in your desktop settings instead."
                .into()
        },
    });

    let taken = if cfg.hotkey == "unset" {
        None
    } else {
        shortcuts::taken_by(&cfg.hotkey)
    };
    checks.push(Check {
        ok: passed_shortcut(&cfg.hotkey, taken.is_some()),
        label: "Shortcut".into(),
        detail: match (&cfg.hotkey, taken) {
            (hotkey, Some(source)) if hotkey != "unset" => {
                format!("{hotkey} is also used by {source}. Pick another in `verbaq tui`.")
            }
            (hotkey, _) if hotkey == "unset" => {
                "not set. Pick one in `verbaq tui` or bind `verbaq toggle`.".into()
            }
            (hotkey, _) => format!(
                "{hotkey} looks free (checked {} shortcuts)",
                shortcuts::count()
            ),
        },
    });

    let outputs = feedback::output_devices();
    let missing_output = cfg
        .output_option()
        .filter(|name| !outputs.is_empty() && !outputs.iter().any(|output| output.name == *name));
    let output_label = cfg.output_option().and_then(|name| {
        outputs
            .iter()
            .find(|output| output.name == name)
            .map(|output| {
                if output.description.is_empty() {
                    output.name.clone()
                } else {
                    output.description.clone()
                }
            })
    });
    let set_ok = feedback::sound_available(&cfg.sound_set);
    let set_label = feedback::set_label(&cfg.sound_set);
    let detail = if !set_ok {
        format!("{set_label} sound files are missing. Reinstall verbaq.")
    } else if let Some(name) = missing_output {
        format!("output {name} is gone. Pick another in `verbaq tui`.")
    } else if feedback::player().is_none() {
        "no sound player. Install pipewire or pulseaudio.".into()
    } else if !cfg.output.is_empty() {
        format!(
            "{set_label} set on {}, this is the only feedback",
            output_label.as_deref().unwrap_or(&cfg.output)
        )
    } else {
        format!(
            "{set_label} set on the system default with {}, this is the only feedback",
            feedback::player().unwrap_or("no player")
        )
    };
    checks.push(Check {
        ok: feedback::player().is_some() && set_ok && missing_output.is_none(),
        label: "Sound".into(),
        detail,
    });

    checks.push(Check {
        ok: inject::clipboard_available(),
        label: "Clipboard".into(),
        detail: if inject::clipboard_available() {
            "clipboard tool available".into()
        } else {
            "install wl-clipboard (Wayland) or xclip (X11)".into()
        },
    });

    checks.push(Check {
        ok: inject::clipboard_available()
            && std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/uinput")
                .is_ok(),
        label: "Text insertion".into(),
        detail: format!(
            "{}, {}",
            if inject::clipboard_available() {
                "clipboard ready"
            } else {
                "clipboard tool missing"
            },
            if std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/uinput")
                .is_ok()
            {
                "typing ready"
            } else {
                "typing needs the udev rule and a new login"
            }
        ),
    });

    let running = ipc::is_running();
    checks.push(Check {
        ok: running,
        label: "Background service".into(),
        detail: if running {
            ipc::send("status")
                .unwrap_or_else(|_| "running".into())
                .lines()
                .next()
                .unwrap_or("running")
                .to_string()
        } else {
            format!(
                "not running. Start it with `verbaq start` (start at login: {}).",
                if crate::service::autostart_enabled() {
                    "on"
                } else {
                    "off"
                }
            )
        },
    });

    checks.push(Check {
        ok: true,
        label: "Session".into(),
        detail: format!("{} on {}", util::desktop_name(), util::session_type()),
    });

    if verbose_paste {
        checks.push(Check {
            ok: inject::clipboard_available(),
            label: "Paste test".into(),
            detail: if inject::copy_only("verbaq paste test") {
                "sample text copied to the clipboard".into()
            } else {
                "could not write to the clipboard".into()
            },
        });
    }

    checks
}

pub fn print(verbose_paste: bool) {
    for check in run(verbose_paste) {
        let mark = if check.ok { "ok  " } else { "fail" };
        println!(
            "[{mark}] {:<24} {}",
            check.label,
            check.detail.replace('\n', " ")
        );
    }
}

fn passed_shortcut(hotkey: &str, taken: bool) -> bool {
    hotkey != "unset" && !taken
}
