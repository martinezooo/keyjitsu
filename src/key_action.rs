//! Canonical QMK action composition/parsing shared by GUI state and editor.
//!
//! Keep all string-level QMK action translation here. Rendering must not grow
//! another parser for LT/MT/modifier wrappers.

use crate::oryx_api::{KeyAction, OryxKey};

const MODIFIER_WRAPPERS: &[&str] = &[
    "LCTL", "RCTL", "LSFT", "RSFT", "LALT", "RALT", "LGUI", "RGUI",
];

fn canonicalize_one_qmk_code(code: &str) -> String {
    let code = code.trim();
    for wrapper in MODIFIER_WRAPPERS {
        let prefix = format!("{wrapper}(");
        if let Some(inner) = code
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let inner = canonicalize_one_qmk_code(inner);
            let repeated = inner
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(')'))
                .is_some();
            if repeated {
                return inner;
            }
            return format!("{wrapper}({inner})");
        }
    }
    code.to_string()
}

/// Canonical QMK spelling used for state ids, editor round-trips and builds.
/// Repeating the same modifier wrapper is semantically redundant in QMK and
/// used to accumulate across edit/flash cycles.
pub(crate) fn canonicalize_qmk_code(code: &str) -> String {
    if code.contains('\n') {
        return code
            .lines()
            .map(canonicalize_one_qmk_code)
            .collect::<Vec<_>>()
            .join("\n");
    }
    canonicalize_one_qmk_code(code)
}

pub(crate) fn hold_wrap(hold: &str, tap: &str) -> Option<String> {
    if let Some(n) = hold.strip_prefix("MO(").and_then(|r| r.strip_suffix(')')) {
        return Some(format!("LT({},{tap})", n.trim()));
    }
    let m = match hold {
        "KC_LSFT" | "KC_LEFT_SHIFT" | "KC_LSHIFT" => "LSFT_T",
        "KC_RSFT" | "KC_RIGHT_SHIFT" | "KC_RSHIFT" => "RSFT_T",
        "KC_LCTL" | "KC_LEFT_CTRL" | "KC_LCTRL" => "LCTL_T",
        "KC_RCTL" | "KC_RIGHT_CTRL" | "KC_RCTRL" => "RCTL_T",
        "KC_LALT" | "KC_LEFT_ALT" => "LALT_T",
        "KC_RALT" | "KC_RIGHT_ALT" => "RALT_T",
        "KC_LGUI" | "KC_LEFT_GUI" | "KC_LCMD" => "LGUI_T",
        "KC_RGUI" | "KC_RIGHT_GUI" | "KC_RCMD" => "RGUI_T",
        "KC_HYPR" => "HYPR_T",
        "KC_MEH" => "MEH_T",
        _ => return None,
    };
    Some(format!("{m}({tap})"))
}

pub(crate) fn unknown_device_key() -> OryxKey {
    OryxKey {
        custom_label: Some("?".into()),
        ..Default::default()
    }
}

pub(crate) fn synth_slots(slots: &[Option<String>; 4]) -> OryxKey {
    let mut key = OryxKey::default();
    let action = |code: &Option<String>| -> Option<KeyAction> {
        let code = code.as_deref()?;
        let synthesized = synth_key(code);
        synthesized.tap.or(synthesized.hold)
    };
    key.tap = action(&slots[0]);
    key.hold = action(&slots[1]);
    key.double_tap = action(&slots[2]);
    key.tap_hold = action(&slots[3]);
    key
}

pub(crate) fn synth_key(code: &str) -> OryxKey {
    let canonical = canonicalize_qmk_code(code);
    let code = canonical.as_str();
    let mut k = OryxKey::default();
    for fam in ["MO", "TO", "TG", "TT", "OSL", "DF"] {
        if let Some(rest) = code.strip_prefix(fam).and_then(|r| r.strip_prefix('(')) {
            if let Some(inner) = rest.strip_suffix(')') {
                if let Ok(layer) = inner.trim().parse::<u8>() {
                    k.tap = Some(KeyAction {
                        code: Some(fam.to_string()),
                        layer: Some(layer),
                        description: None,
                        ..Default::default()
                    });
                    return k;
                }
            }
        }
    }
    // LT(n, tap) → tap key + a hold-to-layer hint.
    if let Some(rest) = code.strip_prefix("LT(").and_then(|r| r.strip_suffix(')')) {
        let mut it = rest.splitn(2, ',');
        if let (Some(n), Some(tap)) = (it.next(), it.next()) {
            if let Ok(layer) = n.trim().parse::<u8>() {
                k.tap = Some(KeyAction {
                    code: Some(tap.trim().to_string()),
                    layer: None,
                    description: None,
                    ..Default::default()
                });
                k.hold = Some(KeyAction {
                    code: Some("MO".into()),
                    layer: Some(layer),
                    description: None,
                    ..Default::default()
                });
                return k;
            }
        }
    }
    for (wrapper, hold) in [
        ("LSFT_T(", "KC_LSFT"),
        ("RSFT_T(", "KC_RSFT"),
        ("LCTL_T(", "KC_LCTL"),
        ("RCTL_T(", "KC_RCTL"),
        ("LALT_T(", "KC_LALT"),
        ("RALT_T(", "KC_RALT"),
        ("LGUI_T(", "KC_LGUI"),
        ("RGUI_T(", "KC_RGUI"),
        ("HYPR_T(", "KC_HYPR"),
        ("MEH_T(", "KC_MEH"),
    ] {
        if let Some(tap) = code.strip_prefix(wrapper).and_then(|r| r.strip_suffix(')')) {
            k.tap = Some(KeyAction {
                code: Some(tap.to_string()),
                ..Default::default()
            });
            k.hold = Some(KeyAction {
                code: Some(hold.to_string()),
                ..Default::default()
            });
            return k;
        }
    }
    k.tap = Some(KeyAction {
        code: Some(code.to_string()),
        ..Default::default()
    });
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_modifier_wrappers_are_canonicalized() {
        assert_eq!(canonicalize_qmk_code("LALT(LALT(KC_TAB))"), "LALT(KC_TAB)");
        assert_eq!(
            canonicalize_qmk_code("LGUI(LALT(LALT(KC_TAB)))"),
            "LGUI(LALT(KC_TAB))"
        );
        assert_eq!(
            canonicalize_qmk_code("LALT(LALT(KC_TAB))\nLGUI(LGUI(KC_A))"),
            "LALT(KC_TAB)\nLGUI(KC_A)"
        );
    }

    #[test]
    fn synthesized_legacy_double_modifier_becomes_canonical() {
        let key = synth_key("LALT(LALT(KC_TAB))");
        assert_eq!(
            key.tap.as_ref().and_then(KeyAction::qmk_code).as_deref(),
            Some("LALT(KC_TAB)")
        );
    }

    #[test]
    fn option_tab_stays_one_modified_action() {
        let key = synth_key("LALT(KC_TAB)");
        assert_eq!(
            key.tap.as_ref().and_then(KeyAction::qmk_code).as_deref(),
            Some("LALT(KC_TAB)")
        );
    }

    #[test]
    fn tap_hold_roundtrips_common_modifiers_and_layers() {
        assert_eq!(
            hold_wrap("KC_LALT", "KC_TAB").as_deref(),
            Some("LALT_T(KC_TAB)")
        );
        assert_eq!(hold_wrap("MO(2)", "KC_A").as_deref(), Some("LT(2,KC_A)"));
        let lt = synth_key("LT(2,KC_A)");
        assert_eq!(
            lt.tap.as_ref().and_then(KeyAction::qmk_code).as_deref(),
            Some("KC_A")
        );
        assert_eq!(
            lt.hold.as_ref().and_then(KeyAction::qmk_code).as_deref(),
            Some("MO(2)")
        );
    }
}
