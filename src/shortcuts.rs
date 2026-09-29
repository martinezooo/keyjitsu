//! Built-in shortcut cheatsheet: a curated CSV of ready-made shortcuts
//! (macOS, editors, terminals, pentest tools, QMK patterns…) shown in the
//! GUI's Shortcuts tab. User-added entries live in the config instead.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutDef {
    pub category: String,
    pub keys: String,
    pub desc: String,
    /// true = "High" usefulness, false = "Medium".
    pub high: bool,
}

/// Parse one CSV line. The Shortcut column may itself contain commas
/// ("Cmd + ,"), so: category = first field, usefulness = last field,
/// description = second-to-last, keys = everything in between re-joined.
fn parse_line(line: &str) -> Option<ShortcutDef> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() < 4 {
        return None;
    }
    let category = parts[0].trim().to_string();
    let high = parts[parts.len() - 1].trim().eq_ignore_ascii_case("high");
    let desc = parts[parts.len() - 2].trim().to_string();
    let keys = parts[1..parts.len() - 2].join(",").trim().to_string();
    if category.is_empty() || keys.is_empty() {
        return None;
    }
    Some(ShortcutDef {
        category,
        keys,
        desc,
        high,
    })
}

/// The embedded library, parsed once.
pub fn builtin() -> &'static [ShortcutDef] {
    static CACHE: OnceLock<Vec<ShortcutDef>> = OnceLock::new();
    CACHE.get_or_init(|| {
        include_str!("../resources/shortcuts.csv")
            .lines()
            .skip(1) // header
            .filter_map(parse_line)
            .collect()
    })
}

/// Import straightforward key bindings from terminal configs found under a
/// user's home directory. Only line-oriented formats are supported.
pub fn import_terminal_shortcuts(home: &Path) -> Vec<ShortcutDef> {
    let mut out = Vec::new();
    for path in [
        home.join(".config/ghostty/config"),
        home.join("Library/Application Support/com.mitchellh.ghostty/config"),
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            parse_ghostty_bindings(&text, &mut out);
        }
    }
    if let Ok(text) = std::fs::read_to_string(home.join(".config/kitty/kitty.conf")) {
        parse_kitty_bindings(&text, &mut out);
    }
    out.sort_by(|a, b| (&a.category, &a.keys, &a.desc).cmp(&(&b.category, &b.keys, &b.desc)));
    out.dedup_by(|a, b| a.category == b.category && a.keys == b.keys && a.desc == b.desc);
    out
}

fn parse_ghostty_bindings(text: &str, out: &mut Vec<ShortcutDef>) {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("keybind") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let Some((keys, action)) = rest.trim().split_once('=') else {
            continue;
        };
        push_terminal_binding(out, "Ghostty import", keys, action);
    }
}

fn parse_kitty_bindings(text: &str, out: &mut Vec<ShortcutDef>) {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("map ") else {
            continue;
        };
        let mut parts = rest.split_whitespace().peekable();
        while parts.peek().is_some_and(|p| p.starts_with("--")) {
            parts.next();
        }
        let Some(keys) = parts.next() else { continue };
        let action = parts.collect::<Vec<_>>().join(" ");
        if action.is_empty() {
            continue;
        }
        push_terminal_binding(out, "Kitty import", keys, &action);
    }
}

fn push_terminal_binding(out: &mut Vec<ShortcutDef>, category: &str, keys: &str, action: &str) {
    let keys = humanize_terminal_keys(keys);
    let action = action.trim();
    if keys.is_empty() || action.is_empty() || action == "no_op" {
        return;
    }
    out.push(ShortcutDef {
        category: category.to_string(),
        keys,
        desc: action.replace('_', " "),
        high: true,
    });
}

fn humanize_terminal_keys(keys: &str) -> String {
    let keys = keys
        .rsplit_once(':')
        .map(|(_, trigger)| trigger)
        .unwrap_or(keys);
    keys.split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl".to_string(),
            "shift" => "Shift".to_string(),
            "alt" | "opt" | "option" => "Opt".to_string(),
            "cmd" | "command" | "super" => "Cmd".to_string(),
            "enter" | "return" => "Enter".to_string(),
            "escape" | "esc" => "Esc".to_string(),
            "space" => "Space".to_string(),
            _ => {
                let mut chars = p.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => c.to_ascii_uppercase().to_string(),
                    _ => p.to_string(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Best-effort conversion of a human shortcut string ("Cmd + Shift + 4") into
/// a QMK keycode ("LGUI(LSFT(KC_4))"), so an entry from the shortcut library
/// can be assigned to a key slot directly instead of hand-typing QMK's
/// modifier-wrapper syntax. Reuses `keycodes::CATALOG` as the single source
/// of KC_ codes - only modifier words and a few label-wording differences
/// between the two lists are defined here.
///
/// Returns `None` for anything that isn't a single chord of modifiers plus
/// one base key: double-key sequences ("GG", "DD"), simultaneous-key combos
/// ("J + K combo"), and tap/hold descriptions ("Tap Esc / Hold Ctrl") aren't
/// one key press, so there is no code to generate - the entry is still
/// readable in the cheatsheet, just not one-click assignable.
pub fn to_qmk_code(keys: &str) -> Option<String> {
    let parts: Vec<&str> = keys
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let (base, mods) = parts.split_last()?;
    let base_code = base_key_code(base)?;
    let mut code = base_code.to_string();
    for m in mods.iter().rev() {
        code = format!("{}({code})", modifier_wrapper(m)?);
    }
    Some(code)
}

fn modifier_wrapper(word: &str) -> Option<&'static str> {
    match word.to_lowercase().as_str() {
        "cmd" | "gui" | "win" | "super" => Some("LGUI"),
        "ctrl" | "control" => Some("LCTL"),
        "option" | "opt" | "alt" => Some("LALT"),
        "shift" => Some("LSFT"),
        _ => None,
    }
}

/// Look up one base key by its shortcut-library wording in
/// `keycodes::CATALOG`'s labels (case-insensitive), applying a small alias
/// table first where the library's wording differs from the picker's terser
/// labels (both name the same keycodes). Numpad and templated (layer)
/// entries are excluded so a plain digit resolves to the top-row key, not
/// the numpad.
fn base_key_code(word: &str) -> Option<&'static str> {
    let alias = match word {
        "Up" => "↑",
        "Down" => "↓",
        "Left" => "←",
        "Right" => "→",
        "Backspace" => "Bksp",
        "Delete" => "Del",
        "Escape" => "Esc",
        "Next Track" => "Next",
        "Previous Track" => "Prev",
        "Volume Up" => "Vol +",
        "Volume Down" => "Vol -",
        "Brightness Up" => "Bright +",
        "Brightness Down" => "Bright -",
        other => other,
    };
    crate::keycodes::CATALOG
        .iter()
        .filter(|c| !c.templated && c.name != "Numpad")
        .find_map(|c| c.keys.iter().find(|k| k.label.eq_ignore_ascii_case(alias)))
        .map(|k| k.code)
}

/// Build a QMK modifier-chord code directly from toggle state (used by the
/// picker's "Build a combo" tab, for a shortcut that isn't already in the
/// library). The wrap order has no effect on what the OS receives -
/// modifiers are held simultaneously, not sequenced - so a fixed, readable
/// order is used regardless of which order the toggles were clicked in.
pub fn compose(ctrl: bool, shift: bool, alt: bool, gui: bool, base_code: &str) -> String {
    let mut code = base_code.to_string();
    if shift {
        code = format!("LSFT({code})");
    }
    if alt {
        code = format!("LALT({code})");
    }
    if ctrl {
        code = format!("LCTL({code})");
    }
    if gui {
        code = format!("LGUI({code})");
    }
    code
}

/// Human-readable label for [`compose`]'s result, for a live preview.
pub fn compose_label(ctrl: bool, shift: bool, alt: bool, gui: bool, base_label: &str) -> String {
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if shift {
        parts.push("Shift");
    }
    if alt {
        parts.push("Opt");
    }
    if gui {
        parts.push("Cmd");
    }
    parts.push(base_label);
    parts.join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_parses_completely() {
        let all = builtin();
        assert!(
            all.len() > 140,
            "expected the full library, got {}",
            all.len()
        );
        // Every entry has a category and keys.
        assert!(all
            .iter()
            .all(|s| !s.category.is_empty() && !s.keys.is_empty()));
    }

    #[test]
    fn comma_inside_shortcut_survives() {
        // "Cmd + ," (app preferences) must keep its comma.
        let hit = builtin()
            .iter()
            .find(|s| s.desc.contains("settings/preferences"))
            .unwrap();
        assert_eq!(hit.keys, "Cmd + ,");
        assert!(hit.high);
    }

    #[test]
    fn library_converts_to_qmk() {
        assert_eq!(
            to_qmk_code("Cmd + Shift + 4").as_deref(),
            Some("LGUI(LSFT(KC_4))")
        );
        assert_eq!(
            to_qmk_code("Ctrl + Cmd + Q").as_deref(),
            Some("LCTL(LGUI(KC_Q))")
        );
        assert_eq!(to_qmk_code("Tab").as_deref(), Some("KC_TAB"));
        assert_eq!(to_qmk_code("Cmd + ,").as_deref(), Some("LGUI(KC_COMMA)"));
        assert_eq!(to_qmk_code("Cmd + Up").as_deref(), Some("LGUI(KC_UP)"));
        assert_eq!(to_qmk_code("Play/Pause").as_deref(), Some("KC_MPLY"));
        assert_eq!(to_qmk_code("Volume Up").as_deref(), Some("KC_VOLU"));
        // Not a single key press: no code to generate.
        assert_eq!(to_qmk_code("GG"), None);
        assert_eq!(to_qmk_code("J + K combo"), None);
        assert_eq!(to_qmk_code("Tap Esc / Hold Ctrl"), None);
        assert_eq!(to_qmk_code("Shift + Arrow"), None);
    }

    #[test]
    fn most_of_the_library_converts() {
        // Not every entry is a single chord (see the failing cases above),
        // but the large majority should be - this guards against an alias
        // regression silently breaking the whole conversion path.
        let all = builtin();
        let ok = all
            .iter()
            .filter(|s| to_qmk_code(&s.keys).is_some())
            .count();
        assert!(
            ok * 100 / all.len() >= 75,
            "only {ok}/{} converted",
            all.len()
        );
    }

    #[test]
    fn imports_ghostty_and_kitty_line_bindings() {
        let mut got = Vec::new();
        parse_ghostty_bindings(
            "keybind = ctrl+shift+t=new_tab\n# nope\nkeybind = super+k=clear_screen\n",
            &mut got,
        );
        parse_kitty_bindings(
            "map ctrl+shift+w close_window\nmap alt+1 goto_tab 1\nmap --allow-fallback=shifted,ascii ctrl+shift+k scroll_line_up smooth\n",
            &mut got,
        );
        assert!(got.iter().any(|s| s.category == "Ghostty import"
            && s.keys == "Ctrl + Shift + T"
            && s.desc == "new tab"));
        assert!(got.iter().any(|s| s.category == "Kitty import"
            && s.keys == "Ctrl + Shift + W"
            && s.desc == "close window"));
        assert!(got.iter().any(|s| s.keys == "Cmd + K"));
        assert!(got
            .iter()
            .any(|s| s.keys == "Ctrl + Shift + K" && s.desc == "scroll line up smooth"));
    }

    #[test]
    fn compose_builds_expected_codes() {
        // Fixed wrap order (shift, alt, ctrl, gui, inner to outer): QMK
        // doesn't care which modifier wraps which - they're held
        // simultaneously, not sequenced - so this need not match
        // to_qmk_code's own (also valid) ordering byte-for-byte.
        assert_eq!(
            compose(false, true, false, true, "KC_4"),
            "LGUI(LSFT(KC_4))"
        );
        assert_eq!(
            compose(true, false, false, true, "KC_Q"),
            "LGUI(LCTL(KC_Q))"
        );
        assert_eq!(compose(false, false, false, false, "KC_TAB"), "KC_TAB");
    }

    #[test]
    fn categories_are_grouped() {
        let cats: Vec<&str> = {
            let mut v: Vec<&str> = builtin().iter().map(|s| s.category.as_str()).collect();
            v.dedup();
            v
        };
        // Dedup on the ordered list = one run per category (no interleaving).
        let mut sorted = cats.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            cats.len(),
            sorted.len(),
            "categories should be contiguous blocks"
        );
    }
}
