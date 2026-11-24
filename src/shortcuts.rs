use std::collections::HashSet;

use crate::util;

#[derive(Debug, Clone, Copy)]
pub struct Suggestion {
    pub display: &'static str,
    pub codes: &'static [u16],
    pub why: &'static str,
}

/// Left hand, nothing exotic. Ordered by how easy they are to press while
/// the right hand stays on the mouse.
pub const SUGGESTIONS: &[Suggestion] = &[
    Suggestion {
        display: "Ctrl+Alt+D",
        codes: &[29, 56, 32],
        why: "left hand, D for Dictate",
    },
    Suggestion {
        display: "Ctrl+Alt+S",
        codes: &[29, 56, 31],
        why: "left hand, S for Speak",
    },
    Suggestion {
        display: "Ctrl+Alt+Space",
        codes: &[29, 56, 57],
        why: "one thumb on Alt, one on Space",
    },
    Suggestion {
        display: "Ctrl+Alt+A",
        codes: &[29, 56, 30],
        why: "far left, easy to find without looking",
    },
    Suggestion {
        display: "Ctrl+Alt+Z",
        codes: &[29, 56, 44],
        why: "bottom left corner",
    },
    Suggestion {
        display: "Super+Shift+D",
        codes: &[125, 42, 32],
        why: "only if you prefer the Super key",
    },
];

#[derive(Debug, Clone)]
pub struct Existing {
    pub combo: String,
    pub source: String,
}

/// All shortcuts this system already knows about, as far as we can read them.
pub fn existing() -> Vec<Existing> {
    let mut out: Vec<Existing> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::PathBuf::from(home);
        read_kde(&home, &mut out);
        read_gnome(&mut out);
        read_sway(&home, &mut out);
        read_hyprland(&home, &mut out);
        read_i3(&home, &mut out);
    }

    // Stable de-duplication: keep the first source for each combination.
    let mut seen: HashSet<String> = HashSet::new();
    out.retain(|entry| {
        let key = entry.combo.to_lowercase();
        !key.is_empty() && seen.insert(key)
    });
    out.sort_by(|a, b| a.combo.cmp(&b.combo));
    out
}

/// The source that already uses this combination, if any.
pub fn taken_by(combo: &str) -> Option<String> {
    taken_by_in(&existing(), combo)
}

/// Same lookup against a list the caller already read. The TUI uses this so it
/// does not scan every desktop shortcut several times per frame.
pub fn taken_by_in(entries: &[Existing], combo: &str) -> Option<String> {
    let wanted = normalize(combo);
    entries
        .iter()
        .find(|entry| normalize(&entry.combo) == wanted)
        .map(|entry| entry.source.clone())
}

pub fn count() -> usize {
    existing().len()
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

fn read_kde(home: &std::path::Path, out: &mut Vec<Existing>) {
    let path = home.join(".config/kglobalshortcutsrc");
    for line in read_lines(&path) {
        if line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().is_empty() || name.trim().starts_with("_k_") {
            continue;
        }
        // Value is "current,default,friendly". A shortcut can have several
        // alternatives separated by a tab.
        let current = value.split(',').next().unwrap_or("");
        for part in current.split('\t') {
            let part = part.trim();
            if part.is_empty() || part.eq_ignore_ascii_case("none") {
                continue;
            }
            if is_media_key(part) {
                continue;
            }
            let combo = normalize(part);
            if combo.is_empty() {
                continue;
            }
            out.push(Existing {
                combo,
                source: "KDE".into(),
            });
        }
    }
}

fn read_gnome(out: &mut Vec<Existing>) {
    if util::which("gsettings").is_none() {
        return;
    }
    let schemas = [
        "org.gnome.desktop.wm.keybindings",
        "org.gnome.shell.keybindings",
        "org.gnome.mutter.keybindings",
        "org.gnome.settings-daemon.plugins.media-keys",
    ];
    for schema in schemas {
        let mut command = std::process::Command::new("gsettings");
        command.args(["list-recursively", schema]);
        let Ok(output) =
            util::run_with_timeout(&mut command, std::time::Duration::from_millis(500))
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let Some((_, value)) = line.split_once(' ') else {
                continue;
            };
            for quoted in value.split(['\'', '"']) {
                if !quoted.contains('<') && !quoted.contains('>') {
                    continue;
                }
                let combo = normalize(&quoted.replace(['<', '>'], "+"));
                if combo.is_empty() {
                    continue;
                }
                out.push(Existing {
                    combo,
                    source: "GNOME".into(),
                });
            }
        }
    }

    // GNOME stores custom shortcuts in relocatable schemas.
    let mut command = std::process::Command::new("gsettings");
    command.args([
        "get",
        "org.gnome.settings-daemon.plugins.media-keys",
        "custom-keybindings",
    ]);
    if let Ok(output) = util::run_with_timeout(&mut command, std::time::Duration::from_millis(500))
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for quoted in text.split(['\'', '"']) {
            let path = quoted.trim();
            if !path.starts_with("/org/gnome/settings-daemon/") {
                continue;
            }
            let schema =
                format!("org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{path}");
            let mut binding = std::process::Command::new("gsettings");
            binding.args(["get", &schema, "binding"]);
            let Ok(value) =
                util::run_with_timeout(&mut binding, std::time::Duration::from_millis(500))
            else {
                continue;
            };
            let value = String::from_utf8_lossy(&value.stdout);
            for quoted in value.split(['\'', '"']) {
                if !quoted.contains('<') && !quoted.contains('>') {
                    continue;
                }
                let combo = normalize(&quoted.replace(['<', '>'], "+"));
                if !combo.is_empty() {
                    out.push(Existing {
                        combo,
                        source: "GNOME".into(),
                    });
                }
            }
        }
    }
}

fn read_sway(home: &std::path::Path, out: &mut Vec<Existing>) {
    let path = home.join(".config/sway/config");
    for line in read_lines(&path) {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("bindsym ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(keys) = words.find(|word| !word.starts_with("--")) else {
            continue;
        };
        let combo = normalize(&keys.replace("$mod", "SUPER"));
        if !combo.is_empty() {
            out.push(Existing {
                combo,
                source: "sway".into(),
            });
        }
    }
}

fn read_i3(home: &std::path::Path, out: &mut Vec<Existing>) {
    let path = home.join(".config/i3/config");
    for line in read_lines(&path) {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("bindsym ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(keys) = words.find(|word| !word.starts_with("--")) else {
            continue;
        };
        let combo = normalize(&keys.replace("$mod", "SUPER"));
        if !combo.is_empty() {
            out.push(Existing {
                combo,
                source: "i3".into(),
            });
        }
    }
}

fn read_hyprland(home: &std::path::Path, out: &mut Vec<Existing>) {
    let path = home.join(".config/hypr/hyprland.conf");
    let lines = read_lines(&path);
    let mut main_mod = "SUPER".to_string();
    for line in &lines {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "$mainMod" {
                main_mod = value.trim().trim_start_matches('$').to_string();
            }
        }
    }
    for line in lines {
        let line = line.trim();
        if !line.starts_with("bind") {
            continue;
        }
        let Some((_, rest)) = line.split_once('=') else {
            continue;
        };
        let mut fields = rest.split(',');
        let modifiers = fields.next().unwrap_or("").replace("$mainMod", &main_mod);
        let key = fields.next().unwrap_or("").trim();
        if key.is_empty() || key.starts_with("mouse") || key.starts_with("code:") {
            continue;
        }
        let combo = normalize(&format!("{modifiers}+{key}"));
        if !combo.is_empty() && has_letter_or_space(&combo) {
            out.push(Existing {
                combo,
                source: "Hyprland".into(),
            });
        }
    }
}

fn has_letter_or_space(combo: &str) -> bool {
    combo
        .split('+')
        .next_back()
        .map(|k| k.len() > 1 || k.chars().any(|c| c.is_ascii_alphabetic()))
        .unwrap_or(false)
}

fn is_media_key(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("volume")
        || lower.contains("mute")
        || lower.contains("mic ")
        || lower.contains("brightness")
        || lower.contains("media")
        || lower.contains("play")
        || lower.contains("sleep")
        || lower.contains("monitor")
}

/// Canonical "Ctrl+Alt+Shift+Super+Key" form, independent of the source.
pub fn normalize(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut sup = false;
    let mut key = String::new();

    let raw = raw.replace(['<', '>'], "+");
    for token in raw.split(['+', ',', ' ']) {
        let token = token.trim().trim_matches(['\'', '"']);
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "primary" | "control_l" | "control_r" => ctrl = true,
            "alt" | "mod1" | "alt_l" | "alt_r" | "option" => alt = true,
            "shift" | "shift_l" | "shift_r" => shift = true,
            "super" | "meta" | "win" | "logo" | "mod4" | "super_l" | "super_r" => sup = true,
            "none" | "any" => {}
            other => {
                let pretty = match other {
                    "space" => "Space".to_string(),
                    "return" | "enter" => "Enter".to_string(),
                    "escape" | "esc" => "Esc".to_string(),
                    "tab" => "Tab".to_string(),
                    "backspace" => "Backspace".to_string(),
                    "insert" | "ins" => "Insert".to_string(),
                    "delete" | "del" => "Delete".to_string(),
                    "home" => "Home".to_string(),
                    "end" => "End".to_string(),
                    "page_up" | "prior" => "PageUp".to_string(),
                    "page_down" | "next" => "PageDown".to_string(),
                    "left" => "Left".to_string(),
                    "right" => "Right".to_string(),
                    "up" => "Up".to_string(),
                    "down" => "Down".to_string(),
                    single if single.len() == 1 => single.to_ascii_uppercase(),
                    named => {
                        let mut chars = named.chars();
                        match chars.next() {
                            Some(first) => {
                                format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
                            }
                            None => String::new(),
                        }
                    }
                };
                if !pretty.is_empty() {
                    key = pretty;
                }
            }
        }
    }

    if key.is_empty() {
        return String::new();
    }
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if sup {
        parts.push("Super");
    }
    parts.push(&key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_desktop_variants() {
        assert_eq!(normalize("Meta+Ctrl+A"), "Ctrl+Super+A");
        assert_eq!(normalize("<Super>d"), "Super+D");
        assert_eq!(normalize("SHIFT+CTRL+SPACE"), "Ctrl+Shift+Space");
        assert_eq!(normalize("ctrl+alt+d"), "Ctrl+Alt+D");
        assert_eq!(normalize("none"), "");
    }

    #[test]
    fn lookup_uses_the_supplied_cache() {
        let entries = vec![Existing {
            combo: "Ctrl+Alt+D".into(),
            source: "KDE".into(),
        }];
        assert_eq!(taken_by_in(&entries, "Ctrl+Alt+D").as_deref(), Some("KDE"));
        assert!(taken_by_in(&entries, "Ctrl+Alt+S").is_none());
    }

    #[test]
    fn suggestions_are_unique() {
        let mut seen = HashSet::new();
        for suggestion in SUGGESTIONS {
            assert!(seen.insert(suggestion.display), "duplicate suggestion");
        }
    }
}
