//! Turning Oryx key definitions into short labels that fit on a drawn keycap.

use crate::oryx_api::{KeyAction, OryxKey};

/// Max label width in terminal cells the renderers are designed around.
pub const LABEL_WIDTH: usize = 5;

/// Labels for one keycap: what to show big (tap) and small (hold/extra).
pub struct KeyLabels {
    pub tap: String,
    pub hold: Option<String>,
}

/// A category for a key, used to draw an Oryx-style corner icon so special
/// keys (layer switches, media, mouse…) are recognizable at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// Switches to / toggles a layer (target layer if known).
    Layer(Option<u8>),
    Modifier,
    Media,
    Mouse,
    Lighting,
    Plain,
}

impl KeyKind {
    /// A small glyph to draw in the key's corner (None for plain keys).
    pub fn icon(self) -> Option<&'static str> {
        match self {
            KeyKind::Layer(_) => Some("⧉"),
            KeyKind::Media => Some("♪"),
            KeyKind::Mouse => Some("⌖"),
            KeyKind::Lighting => Some("✦"),
            KeyKind::Modifier | KeyKind::Plain => None,
        }
    }
}

fn is_layer_family(code: &str) -> bool {
    matches!(code, "MO" | "TO" | "TG" | "TT" | "OSL" | "DF" | "LT")
}

fn action_kind(a: &KeyAction) -> Option<KeyKind> {
    let code = a.code.as_deref().unwrap_or("");
    if a.layer.is_some() || is_layer_family(code) {
        return Some(KeyKind::Layer(a.layer));
    }
    let s = code.strip_prefix("KC_").unwrap_or(code);
    if s.starts_with("MS_") || s.starts_with("BTN") || s.starts_with("WH_") || s.starts_with("ACL")
    {
        return Some(KeyKind::Mouse);
    }
    if s.starts_with("MEDIA_")
        || s.starts_with("AUDIO_")
        || s.starts_with("BRIGHTNESS_")
        || matches!(
            s,
            "MPLY" | "MNXT" | "MPRV" | "MSTP" | "MUTE" | "VOLU" | "VOLD" | "BRIU" | "BRID"
        )
    {
        return Some(KeyKind::Media);
    }
    if code.starts_with("RGB_") || matches!(code, "TOGGLE_LAYER_COLOR" | "LED_LEVEL") {
        return Some(KeyKind::Lighting);
    }
    if matches!(
        s,
        "LCTL"
            | "RCTL"
            | "LSFT"
            | "RSFT"
            | "LALT"
            | "RALT"
            | "LGUI"
            | "RGUI"
            | "HYPR"
            | "MEH"
            | "LEFT_CTRL"
            | "RIGHT_CTRL"
            | "LEFT_SHIFT"
            | "RIGHT_SHIFT"
            | "LEFT_ALT"
            | "RIGHT_ALT"
            | "LEFT_GUI"
            | "RIGHT_GUI"
    ) {
        return Some(KeyKind::Modifier);
    }
    None
}

/// Classify a key for its corner icon. Layer/hold behaviors win so a
/// layer-tap or mod-tap is recognizable.
pub fn key_kind(key: &OryxKey) -> KeyKind {
    for action in [key.hold.as_ref(), key.tap.as_ref()].into_iter().flatten() {
        if let Some(k @ KeyKind::Layer(_)) = action_kind(action) {
            return k;
        }
    }
    if let Some(k) = key.tap.as_ref().and_then(action_kind) {
        return k;
    }
    if let Some(k) = key.hold.as_ref().and_then(action_kind) {
        return k;
    }
    KeyKind::Plain
}

pub fn full_labels_for(key: &OryxKey) -> KeyLabels {
    if let Some(custom) = key.custom_label.as_deref().filter(|s| !s.is_empty()) {
        return KeyLabels {
            tap: custom.to_string(),
            hold: key.hold.as_ref().map(action_label).filter(|s| !s.is_empty()),
        };
    }
    if let Some(emoji) = key.emoji.as_deref().filter(|s| !s.is_empty()) {
        return KeyLabels {
            tap: emoji.to_string(),
            hold: None,
        };
    }
    KeyLabels {
        tap: key.tap.as_ref().map(action_label).unwrap_or_default(),
        hold: key.hold.as_ref().map(action_label).filter(|s| !s.is_empty()),
    }
}

pub fn labels_for(key: &OryxKey) -> KeyLabels {
    let full = full_labels_for(key);
    KeyLabels {
        tap: clip(&full.tap),
        hold: full.hold.as_deref().map(clip),
    }
}

pub fn keycap_labels_for(key: &OryxKey) -> KeyLabels {
    let full = full_labels_for(key);
    KeyLabels {
        tap: clip_to(&full.tap, 8),
        hold: full.hold.as_deref().map(|s| clip_to(s, 8)),
    }
}

pub fn action_label(a: &KeyAction) -> String {
    if let Some(layer) = a.layer {
        let fam = a.code.as_deref().unwrap_or("MO");
        return format!("{fam}{layer}");
    }
    if let Some(code) = a.qmk_code() {
        return keycode_label(&code);
    }
    if let Some(description) = a.description.as_deref().filter(|s| !s.trim().is_empty()) {
        return description.to_string();
    }
    a.fallback_kind().unwrap_or("").to_string()
}

/// Short human label for a QMK keycode.
pub fn keycode_label(code: &str) -> String {
    for (pfx, sym) in [
        ("LALT(", "⌥"),
        ("RALT(", "⌥"),
        ("LGUI(", "⌘"),
        ("RGUI(", "⌘"),
        ("LCTL(", "⌃"),
        ("RCTL(", "⌃"),
        ("LSFT(", "⇧"),
        ("RSFT(", "⇧"),
    ] {
        if let Some(inner) = code.strip_prefix(pfx).and_then(|r| r.strip_suffix(')')) {
            return format!("{sym}{}", keycode_label(inner));
        }
    }

    let stripped = code.strip_prefix("KC_").unwrap_or(code);
    // Single letters and digits map to themselves.
    if stripped.len() == 1 {
        return stripped.to_string();
    }
    let s = match stripped {
        "TRANSPARENT" | "TRNS" => "▽",
        "NO" => "",
        "ESCAPE" | "ESC" => "Esc",
        "ENTER" | "ENT" => "⏎",
        "SPACE" | "SPC" => "Spc",
        "BSPC" | "BACKSPACE" => "⌫",
        "TAB" => "Tab",
        "DELETE" | "DEL" => "⌦",
        "INSERT" | "INS" => "Ins",
        "CAPS_LOCK" | "CAPS" => "Caps",
        "GRAVE" | "GRV" => "`",
        "TILD" | "TILDE" => "~",
        "MINUS" | "MINS" => "-",
        "EQUAL" | "EQL" => "=",
        "PLUS" => "+",
        "UNDS" | "UNDERSCORE" => "_",
        "LBRC" | "LEFT_BRACKET" => "[",
        "RBRC" | "RIGHT_BRACKET" => "]",
        "LCBR" => "{",
        "RCBR" => "}",
        "LPRN" => "(",
        "RPRN" => ")",
        "LABK" | "LT" => "<",
        "RABK" | "GT" => ">",
        "BSLS" | "BACKSLASH" => "\\",
        "PIPE" => "|",
        "SCLN" | "SEMICOLON" => ";",
        "COLN" | "COLON" => ":",
        "QUOT" | "QUOTE" => "'",
        "DQUO" | "DOUBLE_QUOTE" => "\"",
        "COMMA" | "COMM" => ",",
        "DOT" => ".",
        "SLASH" | "SLSH" => "/",
        "QUES" | "QUESTION" => "?",
        "EXLM" => "!",
        "AT" => "@",
        "HASH" => "#",
        "DLR" | "DOLLAR" => "$",
        "PERC" | "PERCENT" => "%",
        "CIRC" | "CIRCUMFLEX" => "^",
        "AMPR" | "AMPERSAND" => "&",
        "ASTR" | "ASTERISK" => "*",
        "LEFT" => "←",
        "RIGHT" => "→",
        "UP" => "↑",
        "DOWN" => "↓",
        "HOME" => "Home",
        "END" => "End",
        "PAGE_UP" | "PGUP" => "PgUp",
        "PAGE_DOWN" | "PGDN" => "PgDn",
        "LEFT_SHIFT" | "LSFT" | "RIGHT_SHIFT" | "RSFT" => "⇧",
        "LEFT_CTRL" | "LCTL" | "RIGHT_CTRL" | "RCTL" => "⌃",
        "LEFT_ALT" | "LALT" | "RIGHT_ALT" | "RALT" => "⌥",
        "LEFT_GUI" | "LGUI" | "LCMD" | "RIGHT_GUI" | "RGUI" | "RCMD" => "⌘",
        "HYPR" | "HYPER" => "Hyp",
        "MEH" => "Meh",
        "AUDIO_VOL_UP" | "VOLU" | "KB_VOLUME_UP" => "Vol+",
        "AUDIO_VOL_DOWN" | "VOLD" | "KB_VOLUME_DOWN" => "Vol-",
        "AUDIO_MUTE" | "MUTE" | "KB_MUTE" => "Mute",
        "MEDIA_PLAY_PAUSE" | "MPLY" => "⏯",
        "MEDIA_NEXT_TRACK" | "MNXT" => "⏭",
        "MEDIA_PREV_TRACK" | "MPRV" => "⏮",
        "BRIGHTNESS_UP" | "BRIU" => "Bri+",
        "BRIGHTNESS_DOWN" | "BRID" => "Bri-",
        "PSCR" | "PRINT_SCREEN" => "PrSc",
        "KP_ASTERISK" => "*",
        "KP_SLASH" => "/",
        "KP_MINUS" => "-",
        "KP_PLUS" => "+",
        "KP_EQUAL" => "=",
        "KP_ENTER" => "⏎",
        "KP_DOT" => ".",
        "QK_BOOT" | "RESET" => "Boot",
        "QK_REPEAT_KEY" | "QK_REP" => "Rep",
        "QK_LEAD" => "Lead",
        other => {
            return fallback_label(other);
        }
    };
    s.to_string()
}

/// `KP_7` → `7`, `F11` → `F11`, `AUDIO_...` → prettified & clipped.
fn fallback_label(stripped: &str) -> String {
    if let Some(kp) = stripped.strip_prefix("KP_") {
        return clip(kp);
    }
    if stripped.starts_with('F') && stripped[1..].chars().all(|c| c.is_ascii_digit()) {
        return stripped.to_string();
    }
    // Last path segment style: take the tail word, title-case it.
    let tail = stripped.rsplit('_').next().unwrap_or(stripped);
    let mut chars = tail.chars();
    let pretty: String = match chars.next() {
        Some(c) => c
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    };
    clip(&pretty)
}

/// Clip to `LABEL_WIDTH` terminal cells (chars are a good-enough proxy here).
fn clip_to(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn clip(s: &str) -> String {
    clip_to(s, LABEL_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oryx_api::KeyAction;

    fn action(code: &str) -> KeyAction {
        KeyAction {
            code: Some(code.into()),
            ..Default::default()
        }
    }

    #[test]
    fn plain_letters_and_symbols() {
        assert_eq!(keycode_label("KC_A"), "A");
        assert_eq!(keycode_label("KC_1"), "1");
        assert_eq!(keycode_label("KC_COMMA"), ",");
        assert_eq!(keycode_label("KC_LEFT_SHIFT"), "⇧");
        assert_eq!(keycode_label("KC_TRANSPARENT"), "▽");
    }

    #[test]
    fn layer_switch_labels() {
        let a = KeyAction {
            code: Some("TO".into()),
            layer: Some(2),
            ..Default::default()
        };
        assert_eq!(action_label(&a), "TO2");
    }

    #[test]
    fn oryx_modifier_flags_are_part_of_the_label() {
        let a = KeyAction {
            code: Some("KC_TAB".into()),
            modifiers: Some(crate::oryx_api::KeyModifiers {
                left_alt: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(action_label(&a), "⌥Tab");

        let b = KeyAction {
            code: Some("KC_4".into()),
            modifiers: Some(crate::oryx_api::KeyModifiers {
                left_gui: true,
                left_shift: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(action_label(&b), "⇧⌘4");

        let c = KeyAction {
            code: Some("KC_F11".into()),
            modifiers: Some(crate::oryx_api::KeyModifiers {
                left_ctrl: true,
                left_shift: true,
                left_alt: true,
                left_gui: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(action_label(&c), "⌃⇧⌥⌘F11");
    }

    #[test]
    fn unsupported_oryx_actions_are_never_rendered_as_blank() {
        let a: KeyAction = serde_json::from_str(r#"{"macro":{"name":"example"}}"#).unwrap();
        assert_eq!(action_label(&a), "Macro");
    }

    #[test]
    fn kp_and_fkeys() {
        assert_eq!(keycode_label("KC_KP_7"), "7");
        assert_eq!(keycode_label("KC_F11"), "F11");
    }

    #[test]
    fn unknown_code_prettified_and_clipped() {
        assert_eq!(keycode_label("KC_MS_WH_DOWN"), "Down");
        assert!(keycode_label("KC_SOME_VERY_LONG_THING").chars().count() <= LABEL_WIDTH);
    }

    #[test]
    fn custom_label_wins() {
        let key = OryxKey {
            tap: Some(action("KC_A")),
            custom_label: Some("P-Monitor".into()),
            ..Default::default()
        };
        assert_eq!(labels_for(&key).tap, "P-Mon");
    }
}
