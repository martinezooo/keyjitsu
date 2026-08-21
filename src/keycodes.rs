//! A broad, Oryx-style catalog of assignable keycodes, grouped by category,
//! for the key picker. Layer entries use `{n}` as a placeholder for a layer
//! number the UI fills in (e.g. `MO({n})` → `MO(2)`).

pub struct KeyDef {
    pub code: &'static str,
    pub label: &'static str,
}

pub struct Category {
    pub name: &'static str,
    /// Entries whose `code` contains `{n}` need a layer number from the UI.
    pub templated: bool,
    pub keys: &'static [KeyDef],
}

macro_rules! k {
    ($code:literal, $label:literal) => {
        KeyDef { code: $code, label: $label }
    };
}

pub const CATALOG: &[Category] = &[
    Category {
        name: "Letters",
        templated: false,
        keys: &[
            k!("KC_A", "A"), k!("KC_B", "B"), k!("KC_C", "C"), k!("KC_D", "D"),
            k!("KC_E", "E"), k!("KC_F", "F"), k!("KC_G", "G"), k!("KC_H", "H"),
            k!("KC_I", "I"), k!("KC_J", "J"), k!("KC_K", "K"), k!("KC_L", "L"),
            k!("KC_M", "M"), k!("KC_N", "N"), k!("KC_O", "O"), k!("KC_P", "P"),
            k!("KC_Q", "Q"), k!("KC_R", "R"), k!("KC_S", "S"), k!("KC_T", "T"),
            k!("KC_U", "U"), k!("KC_V", "V"), k!("KC_W", "W"), k!("KC_X", "X"),
            k!("KC_Y", "Y"), k!("KC_Z", "Z"),
        ],
    },
    Category {
        name: "Numbers",
        templated: false,
        keys: &[
            k!("KC_1", "1"), k!("KC_2", "2"), k!("KC_3", "3"), k!("KC_4", "4"),
            k!("KC_5", "5"), k!("KC_6", "6"), k!("KC_7", "7"), k!("KC_8", "8"),
            k!("KC_9", "9"), k!("KC_0", "0"),
        ],
    },
    Category {
        name: "Symbols",
        templated: false,
        keys: &[
            k!("KC_MINUS", "-"), k!("KC_EQUAL", "="), k!("KC_LBRC", "["),
            k!("KC_RBRC", "]"), k!("KC_BSLS", "\\"), k!("KC_SCLN", ";"),
            k!("KC_QUOTE", "'"), k!("KC_GRAVE", "`"), k!("KC_COMMA", ","),
            k!("KC_DOT", "."), k!("KC_SLASH", "/"),
            k!("KC_EXLM", "!"), k!("KC_AT", "@"), k!("KC_HASH", "#"),
            k!("KC_DLR", "$"), k!("KC_PERC", "%"), k!("KC_CIRC", "^"),
            k!("KC_AMPR", "&"), k!("KC_ASTR", "*"), k!("KC_LPRN", "("),
            k!("KC_RPRN", ")"), k!("KC_UNDS", "_"), k!("KC_PLUS", "+"),
            k!("KC_LCBR", "{"), k!("KC_RCBR", "}"), k!("KC_PIPE", "|"),
            k!("KC_COLN", ":"), k!("KC_DQUO", "\""), k!("KC_TILD", "~"),
            k!("KC_LABK", "<"), k!("KC_RABK", ">"), k!("KC_QUES", "?"),
        ],
    },
    Category {
        name: "Navigation",
        templated: false,
        keys: &[
            k!("KC_ESCAPE", "Esc"), k!("KC_TAB", "Tab"), k!("KC_SPACE", "Space"),
            k!("KC_ENTER", "Enter"), k!("KC_BSPC", "Bksp"), k!("KC_DELETE", "Del"),
            k!("KC_INSERT", "Ins"), k!("KC_HOME", "Home"), k!("KC_END", "End"),
            k!("KC_PAGE_UP", "PgUp"), k!("KC_PAGE_DOWN", "PgDn"),
            k!("KC_LEFT", "←"), k!("KC_DOWN", "↓"), k!("KC_UP", "↑"),
            k!("KC_RIGHT", "→"), k!("KC_CAPS", "Caps"), k!("KC_PSCR", "PrtSc"),
        ],
    },
    Category {
        name: "Editing",
        templated: false,
        keys: &[
            k!("KC_UNDO", "Undo"), k!("KC_AGAIN", "Redo"), k!("KC_CUT", "Cut"),
            k!("KC_COPY", "Copy"), k!("KC_PASTE", "Paste"), k!("KC_FIND", "Find"),
            k!("LCTL(KC_A)", "Select all"), k!("LCTL(KC_S)", "Save"),
            k!("LGUI(KC_C)", "⌘C"), k!("LGUI(KC_V)", "⌘V"),
            k!("LGUI(KC_Z)", "⌘Z"), k!("LGUI(KC_SPACE)", "Spotlight"),
        ],
    },
    Category {
        name: "Function",
        templated: false,
        keys: &[
            k!("KC_F1", "F1"), k!("KC_F2", "F2"), k!("KC_F3", "F3"), k!("KC_F4", "F4"),
            k!("KC_F5", "F5"), k!("KC_F6", "F6"), k!("KC_F7", "F7"), k!("KC_F8", "F8"),
            k!("KC_F9", "F9"), k!("KC_F10", "F10"), k!("KC_F11", "F11"),
            k!("KC_F12", "F12"), k!("KC_F13", "F13"), k!("KC_F14", "F14"),
            k!("KC_F15", "F15"), k!("KC_F16", "F16"),
        ],
    },
    Category {
        name: "Media & System",
        templated: false,
        keys: &[
            k!("KC_MPLY", "Play/Pause"), k!("KC_MNXT", "Next"), k!("KC_MPRV", "Prev"),
            k!("KC_MSTP", "Stop"), k!("KC_MUTE", "Mute"), k!("KC_VOLU", "Vol +"),
            k!("KC_VOLD", "Vol -"), k!("KC_BRIU", "Bright +"), k!("KC_BRID", "Bright -"),
            k!("KC_PWR", "Power"), k!("KC_SLEP", "Sleep"), k!("KC_WAKE", "Wake"),
            k!("QK_BOOT", "Bootloader"), k!("EE_CLR", "Clear EEPROM"), k!("QK_RBT", "Reboot"),
        ],
    },
    Category {
        // The point of "mouse-free control": drive the pointer from the board.
        name: "Mouse",
        templated: false,
        keys: &[
            k!("KC_MS_U", "Move ↑"), k!("KC_MS_D", "Move ↓"), k!("KC_MS_L", "Move ←"),
            k!("KC_MS_R", "Move →"), k!("KC_BTN1", "Left click"),
            k!("KC_BTN2", "Right click"), k!("KC_BTN3", "Middle click"),
            k!("KC_BTN4", "Button 4"), k!("KC_BTN5", "Button 5"),
            k!("KC_WH_U", "Wheel ↑"), k!("KC_WH_D", "Wheel ↓"),
            k!("KC_WH_L", "Wheel ←"), k!("KC_WH_R", "Wheel →"),
            k!("KC_ACL0", "Slow"), k!("KC_ACL1", "Medium"), k!("KC_ACL2", "Fast"),
        ],
    },
    Category {
        name: "Modifiers",
        templated: false,
        keys: &[
            k!("KC_LCTL", "L Ctrl"), k!("KC_LSFT", "L Shift"), k!("KC_LALT", "L Alt/Opt"),
            k!("KC_LGUI", "L Cmd/Win"), k!("KC_RCTL", "R Ctrl"), k!("KC_RSFT", "R Shift"),
            k!("KC_RALT", "R Alt/Opt"), k!("KC_RGUI", "R Cmd/Win"),
            k!("KC_HYPR", "Hyper"), k!("KC_MEH", "Meh"),
            k!("OSM(MOD_LSFT)", "One-shot Shift"), k!("OSM(MOD_LGUI)", "One-shot Cmd"),
        ],
    },
    Category {
        name: "Layers",
        templated: true,
        keys: &[
            k!("MO({n})", "Momentary L{n}"), k!("TO({n})", "Switch to L{n}"),
            k!("TG({n})", "Toggle L{n}"), k!("TT({n})", "Tap-toggle L{n}"),
            k!("OSL({n})", "One-shot L{n}"), k!("DF({n})", "Default L{n}"),
            k!("LT({n},KC_SPC)", "L{n}/Space (hold)"),
            k!("LT({n},KC_ENT)", "L{n}/Enter (hold)"),
        ],
    },
    Category {
        name: "Lighting (RGB)",
        templated: false,
        keys: &[
            k!("RGB_TOG", "Toggle RGB"), k!("RGB_MOD", "Next mode"),
            k!("RGB_RMOD", "Prev mode"), k!("RGB_HUI", "Hue +"), k!("RGB_HUD", "Hue -"),
            k!("RGB_SAI", "Sat +"), k!("RGB_SAD", "Sat -"), k!("RGB_VAI", "Bright +"),
            k!("RGB_VAD", "Bright -"), k!("RGB_SPI", "Speed +"), k!("RGB_SPD", "Speed -"),
            k!("RGB_SLD", "Solid"), k!("TOGGLE_LAYER_COLOR", "Toggle layer colors"),
            k!("LED_LEVEL", "LED level"),
        ],
    },
    Category {
        name: "Numpad",
        templated: false,
        keys: &[
            k!("KC_P1", "1"), k!("KC_P2", "2"), k!("KC_P3", "3"), k!("KC_P4", "4"),
            k!("KC_P5", "5"), k!("KC_P6", "6"), k!("KC_P7", "7"), k!("KC_P8", "8"),
            k!("KC_P9", "9"), k!("KC_P0", "0"), k!("KC_PDOT", "."), k!("KC_PPLS", "+"),
            k!("KC_PMNS", "-"), k!("KC_PAST", "*"), k!("KC_PSLS", "/"),
            k!("KC_PENT", "Enter"), k!("KC_PEQL", "="), k!("KC_NUM", "Num Lock"),
        ],
    },
    Category {
        name: "Special",
        templated: false,
        keys: &[
            k!("KC_TRNS", "▽ Transparent"), k!("KC_NO", "✗ Disabled"),
        ],
    },
];

/// Whether a keycode drives mouse movement/buttons/wheel (needs MOUSEKEY).
pub fn needs_mousekey(code: &str) -> bool {
    const P: &[&str] = &["KC_MS_", "KC_BTN", "KC_WH_", "KC_ACL"];
    P.iter().any(|p| code.contains(p))
}

/// Whether a keycode is a consumer/system control (needs EXTRAKEY).
pub fn needs_extrakey(code: &str) -> bool {
    const P: &[&str] = &[
        "KC_MPLY", "KC_MNXT", "KC_MPRV", "KC_MSTP", "KC_MUTE", "KC_VOL", "KC_BRIU",
        "KC_BRID", "KC_MSEL", "KC_PWR", "KC_SLEP", "KC_WAKE",
    ];
    P.iter().any(|p| code.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_detection() {
        assert!(needs_mousekey("KC_MS_U"));
        assert!(needs_mousekey("KC_BTN1"));
        assert!(!needs_mousekey("KC_A"));
        assert!(needs_extrakey("KC_VOLU"));
        assert!(needs_extrakey("KC_MPLY"));
        assert!(!needs_extrakey("KC_A"));
    }

    #[test]
    fn catalog_is_broad() {
        let total: usize = CATALOG.iter().map(|c| c.keys.len()).sum();
        assert!(total > 150, "expected a rich catalog, got {total}");
    }
}
