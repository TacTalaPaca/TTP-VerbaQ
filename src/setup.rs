use anyhow::{bail, Result};
use std::io::{BufRead, Write};
use std::time::Duration;

use crate::audio;
use crate::config::Config;
use crate::feedback::{self, Cue};
use crate::hotkey;
use crate::model;
use crate::service;
use crate::shortcuts;
use crate::stt::Transcriber;
use crate::util;

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

fn ask_yes(prompt: &str, default: bool) -> bool {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    let answer = ask(&format!("{prompt} {hint} "));
    if answer.is_empty() {
        return default;
    }
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

fn bar(done: u64, total: u64) -> String {
    let pct = (done.saturating_mul(100) / total.max(1)).min(100);
    let filled = (pct / 2) as usize;
    format!(
        "[{}{}] {:>3}%  {}/{}",
        "#".repeat(filled),
        ".".repeat(50 - filled),
        pct,
        util::human_size(done),
        util::human_size(total)
    )
}

fn record_for(seconds: u64, mic: Option<&str>) -> Result<(Vec<f32>, u32)> {
    let recorder = audio::Recorder::start(mic)?;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(recorder.stop())
}

fn record_until_enter(mic: Option<&str>, max_seconds: u64) -> Result<(Vec<f32>, u32)> {
    let recorder = audio::Recorder::start(mic)?;
    let start = std::time::Instant::now();
    let deadline = Duration::from_secs(max_seconds);
    while start.elapsed() < deadline {
        let mut descriptor = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
        if ready > 0 && (descriptor.revents & libc::POLLIN) != 0 {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            break;
        }
        if ready < 0 {
            break;
        }
    }
    Ok(recorder.stop())
}

fn choose_shortcut(cfg: &mut Config) {
    if !hotkey::permission_available() {
        println!("The built-in shortcut is not available on this account.");
        println!("Bind this command in your desktop settings instead:");
        println!("    verbaq toggle");
        println!("KDE: System Settings > Shortcuts > Add > Command or script");
        println!("GNOME: Settings > Keyboard > Custom Shortcuts");
        cfg.hotkey = "unset".into();
        cfg.hotkey_codes.clear();
        return;
    }

    let existing = shortcuts::existing();
    println!(
        "VerbaQ found {} shortcuts in your desktop settings.",
        existing.len()
    );
    println!("It checks them so you do not pick one that is already taken.");
    println!();
    for (index, suggestion) in shortcuts::SUGGESTIONS.iter().enumerate() {
        let taken = shortcuts::taken_by(suggestion.display);
        let state = match &taken {
            Some(source) => format!("already used by {source}"),
            None => "free".to_string(),
        };
        println!(
            "  {}. {:<16} {:<28} {}",
            index + 1,
            suggestion.display,
            state,
            suggestion.why
        );
    }
    println!("  c. record your own combination");
    println!();
    let choice = ask("Choose 1-6, c, or Enter for Ctrl+Alt+D: ");
    let mut chosen: Option<(String, Vec<u16>)> = None;

    if choice.eq_ignore_ascii_case("c") {
        println!("Press the combination now. Include Ctrl, Alt or Super.");
        println!("Names follow the US key layout, so your letter may differ.");
        match hotkey::capture() {
            Ok(codes) => {
                let label = hotkey::combo_label(&codes);
                println!("Detected {label}.");
                if ask_yes("Use this combination?", true) {
                    chosen = Some((label, codes));
                }
            }
            Err(err) => println!("Could not read the keys: {err}"),
        }
    } else {
        let index = choice
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1 && *n <= shortcuts::SUGGESTIONS.len())
            .unwrap_or(1);
        let suggestion = &shortcuts::SUGGESTIONS[index - 1];
        chosen = Some((suggestion.display.to_string(), suggestion.codes.to_vec()));
    }

    if let Some((label, codes)) = chosen {
        if let Some(source) = shortcuts::taken_by(&label) {
            println!("Careful: {source} already uses {label}.");
            if !ask_yes("Use it anyway?", false) {
                cfg.hotkey = "unset".into();
                cfg.hotkey_codes.clear();
                return;
            }
        }
        cfg.hotkey = label;
        cfg.hotkey_codes = codes;
        println!("Shortcut: {}", cfg.hotkey);
    }
}

pub fn run() -> Result<()> {
    println!("VerbaQ setup");
    println!("============");
    println!();
    println!("VerbaQ turns speech into text on this machine. Nothing is uploaded.");
    println!("You hold a key, speak, and the text is pasted where your cursor is.");
    println!("This wizard takes about five minutes. You can change everything later");
    println!("with `verbaq tui`.");
    println!();

    let mut cfg = Config::load().unwrap_or_default();

    // 1. Model
    println!("1. Voice model");
    println!("   The model is the part that understands speech. It is downloaded once.");
    println!("   Small is faster and lighter. Turbo is more accurate on harder speech");
    println!("   but takes about three times longer on a processor.");
    println!();
    for (index, spec) in model::MODELS.iter().enumerate() {
        let state = if model::is_installed(spec) {
            "installed"
        } else {
            "not installed"
        };
        println!(
            "   {}. {:<6} {:>8}  {:<14} {}",
            index + 1,
            spec.label,
            util::human_size(spec.size),
            state,
            spec.note
        );
    }
    let choice = ask("\n   Choose 1 or 2, Enter for Small: ");
    let index = match choice.as_str() {
        "2" => 1,
        _ => 0,
    };
    let spec = &model::MODELS[index];
    cfg.model = spec.id.to_string();

    if model::is_installed(spec) {
        println!("   {} is already installed.", spec.label);
    } else {
        println!(
            "\n   Downloading {} ({})",
            spec.label,
            util::human_size(spec.size)
        );
        model::download(spec, |done, total| {
            print!("\r   {}", bar(done, total));
            let _ = std::io::stdout().flush();
        })?;
        println!("\n   Downloaded and verified.");
    }

    // 2. Microphone
    println!("\n2. Microphone");
    println!("   VerbaQ records only while the shortcut is held. It never listens");
    println!("   in the background.");
    println!("   The system default is usually right. Pick another only if the test");
    println!("   below shows silence.");
    println!();
    let devices = audio::input_devices();
    if devices.is_empty() {
        bail!("no microphone found. Connect one and run `verbaq setup` again.");
    }
    let max = devices.len().min(12);
    for (index, name) in devices.iter().take(max).enumerate() {
        let marker = if index == 0 { " (default)" } else { "" };
        println!("   {}. {}{}", index + 1, name, marker);
    }
    if devices.len() > max {
        println!(
            "   ... and {} more, visible in `verbaq tui`.",
            devices.len() - max
        );
    }
    let choice = ask(&format!("   Choose 1-{max}, Enter for default: "));
    cfg.mic.clear();
    if let Ok(n) = choice.parse::<usize>() {
        if n >= 1 && n <= max {
            cfg.mic = devices[n - 1].clone();
        }
    }

    println!("\n   Speak normally for three seconds.");
    let (samples, _) = record_for(3, cfg.mic_option())?;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 0.01 {
        println!("   Silence. Try another microphone in `verbaq tui` before using VerbaQ.");
    } else {
        println!(
            "   Good, the microphone hears you (peak {:.0}%).",
            peak * 100.0
        );
    }

    // 3. Shortcut and mode
    println!("\n3. Shortcut");
    println!("   While you hold this key combination, VerbaQ listens. Releasing it");
    println!("   stops the recording and starts the transcription.");
    println!();
    println!("   How should it behave?");
    println!("     1. Hold the key while speaking, release to stop (recommended)");
    println!("     2. Press once to start, press again to stop");
    let mode = ask("   Choose 1 or 2, Enter for hold: ");
    cfg.hold_to_talk = mode != "2";
    println!();
    choose_shortcut(&mut cfg);

    // 4. Try it
    println!("\n4. Try it");
    println!("   When text is ready, VerbaQ puts it on the clipboard, presses");
    println!("   Shift+Insert, and restores your previous clipboard. This works in");
    println!("   terminals and in normal applications. You can change it later.");
    println!();
    if ask_yes("   Record a test sentence now?", true) {
        let transcriber = Transcriber::load(&model::path(spec), cfg.threads as i32)?;
        println!("   Speak, then press Enter when you are done (max 15 seconds).");
        let (samples, rate) = record_until_enter(cfg.mic_option(), 15)?;
        let samples = audio::resample_to_target(&samples, rate);
        let trimmed = audio::trim_silence(&samples, audio::TARGET_RATE);
        if !audio::has_speech(&trimmed) {
            println!("   No speech was detected.");
        } else {
            let text = transcriber.transcribe(&trimmed)?;
            if text.is_empty() {
                println!("   Nothing was transcribed.");
            } else {
                println!("\n     \"{text}\"\n");
                let _ = ask("   Press Enter to continue. ");
            }
        }
    }

    // 5. History
    println!("\n5. History");
    println!("   History keeps a copy of the last transcripts in a local file.");
    println!("   It is useful when a paste failed, or when you want to look up what");
    println!("   you said. It stays on this machine and can be cleared at any time.");
    cfg.history = ask_yes("   Keep transcript history?", true);

    // 6. Sound and start at login
    println!("\n6. Feedback and startup");
    println!("   VerbaQ uses three short sounds: start, stop, and a problem.");
    println!("   There are no notifications, because desktops hide them too often.");
    cfg.sound = ask_yes("   Play sounds?", true);
    if cfg.sound {
        println!("   Which sound set?");
        for (index, (_, label)) in feedback::SETS.iter().enumerate() {
            println!("     {}. {label}", index + 1);
        }
        let answer = ask("   Number [1]: ");
        let choice = answer.parse::<usize>().unwrap_or(1);
        cfg.sound_set = if choice >= 1 && choice <= feedback::SETS.len() {
            feedback::SETS[choice - 1].0.to_string()
        } else {
            "material".into()
        };
        let outputs = feedback::output_devices();
        if !outputs.is_empty() {
            println!("   Where should the sounds play?");
            println!("     0. System default");
            for (index, output) in outputs.iter().enumerate() {
                let name = if output.description.is_empty() {
                    &output.name
                } else {
                    &output.description
                };
                println!(
                    "     {}. {name} ({})",
                    index + 1,
                    output.state.to_lowercase()
                );
            }
            let answer = ask("   Number [0]: ");
            let choice = answer.parse::<usize>().unwrap_or(0);
            cfg.output = if choice >= 1 && choice <= outputs.len() {
                outputs[choice - 1].name.clone()
            } else {
                String::new()
            };
        }
        feedback::play_now(Cue::Test, cfg.output_option(), &cfg.sound_set);
    }

    println!();
    println!("\n7. Starting VerbaQ");
    println!("   The background service is the part that waits for your shortcut.");
    println!("   Without it, the shortcut does nothing.");
    println!("   Start at login writes one file in ~/.config/autostart. You can");
    println!("   remove it any time from the Service screen in `verbaq tui`.");
    if ask_yes("   Start VerbaQ automatically at login?", true) {
        service::autostart_enable()?;
        println!("   Added {}.", util::autostart_path().display());
    } else {
        service::autostart_disable();
    }

    cfg.save()?;

    println!();
    if ask_yes("Start the background service now?", true) {
        println!("{}", service::start()?);
    } else {
        println!("Start it later with `verbaq start`.");
    }

    println!("\nDone.");
    println!("Settings: {}", Config::path().display());
    println!("Check everything with `verbaq doctor`.");
    println!("Change anything later with `verbaq tui`.");
    Ok(())
}
