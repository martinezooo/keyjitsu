//! Persisted app settings: overlay trigger/chord, RGB + layer-peek HUD,
//! autolayer rules, per-key glow and press effects, custom layers/shortcuts,
//! saved profiles, and local toolchain paths. One JSON file, loaded once at
//! startup and rewritten atomically on change.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::oryx_api::cache_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutolayerRule {
    /// Substring matched against the frontmost app's bundle id.
    pub bundle: String,
    pub layer: u8,
}

/// A user-chosen per-key glow color, overriding the layout's own. Flat list so
/// it round-trips through JSON trivially; keyed by layout hash + layer + key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlowOverride {
    pub layout: String,
    pub layer: u8,
    pub key: u16,
    pub rgb: [u8; 3],
}

/// Vertical anchor of the layer-peek HUD on its monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VAlign {
    Top,
    Bottom,
    /// Also the fallback for an unknown value from a newer config.
    #[serde(other)]
    Middle,
}

/// Horizontal anchor of the layer-peek HUD on its monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HAlign {
    Left,
    Right,
    /// Also the fallback for an unknown value from a newer config.
    #[serde(other)]
    Center,
}

/// The brief transparent "peek" of a layer shown when it activates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeekConfig {
    pub enabled: bool,
    /// Only pop up when leaving the base layer (0), not for every change.
    pub only_non_base: bool,
    pub duration_ms: u64,
    /// Background opacity 0.0-1.0.
    pub opacity: f32,
    /// Size multiplier 0.5-1.6.
    pub scale: f32,
    pub valign: VAlign,
    pub halign: HAlign,
    /// Monitor index (0 = main display).
    pub monitor: usize,
    /// Manual nudge in pixels.
    pub offset: [f32; 2],
    pub show_legends: bool,
    pub show_layer_name: bool,
    /// Draw the dark card behind the keys (off = keys float on transparency).
    pub show_background: bool,
    /// High-contrast black & white rendering instead of the layout colors.
    pub monochrome: bool,
    pub accent: [u8; 3],
    /// Show a live "combo" strip of recent key presses (with gestures:
    /// hold, double-tap, double-tap-hold) on the minimap.
    pub show_combo: bool,
    /// Measurement mode: show press-hold durations and double-tap gaps (ms).
    pub show_combo_ms: bool,
}

impl Default for PeekConfig {
    fn default() -> Self {
        PeekConfig {
            enabled: true,
            only_non_base: true,
            duration_ms: 1200,
            opacity: 0.78,
            scale: 1.0,
            valign: VAlign::Top,
            halign: HAlign::Center,
            monitor: 0,
            offset: [0.0, 0.0],
            show_legends: true,
            show_layer_name: true,
            show_background: true,
            monochrome: false,
            accent: [140, 108, 255],
            show_combo: false,
            show_combo_ms: false,
        }
    }
}

/// The board-level RGB state (constant effect + global press reaction) so it
/// survives restarts and rides along in saved profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RgbState {
    pub effect: crate::gui::Effect,
    pub color: [u8; 3],
    pub speed: f32,
    pub brightness: f32,
    pub press_effect: crate::gui::PressEffect,
    pub press_color: [u8; 3],
    /// Name of the custom sequence when `effect == Custom`.
    pub custom_name: String,
}

impl Default for RgbState {
    fn default() -> Self {
        RgbState {
            effect: crate::gui::Effect::Off,
            color: [80, 170, 255],
            speed: 1.0,
            brightness: 0.85,
            press_effect: crate::gui::PressEffect::None,
            press_color: [255, 255, 255],
            custom_name: String::new(),
        }
    }
}

/// A brand-new layer authored in keyjitsu (beyond the ones in the Oryx
/// source). Appended to the layout; its keys are emitted as a fresh
/// `[N] = LAYOUT(...)` block at build time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomLayer {
    /// Layout hash this layer belongs to.
    pub layout: String,
    pub name: String,
    /// (visual key index → QMK keycode) for the keys the user filled in.
    pub keys: Vec<CustomKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomKey {
    pub key: u16,
    pub code: String,
}

/// A user-added entry in the Shortcuts cheatsheet (built-ins ship in the
/// binary; these extend/customize them and survive restarts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomShortcut {
    pub category: String,
    pub keys: String,
    pub desc: String,
    pub high: bool,
}

/// A per-key press effect assigned in the key editor (Oryx-style: alongside
/// the key's color, a reaction that plays when the key is pressed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFx {
    pub layout: String,
    pub layer: u8,
    pub key: u16,
    pub trigger: crate::gui::FxTrigger,
    pub effect: crate::gui::PressEffect,
    pub color: [u8; 3],
    /// Name of a user-built sequence to play instead of `effect`.
    #[serde(default)]
    pub custom: Option<String>,
}

/// A per-key remap the user staged but hasn't built into firmware yet. Keyed
/// by layout hash (like [`GlowOverride`]) so it survives restarts and rides
/// along in profiles, instead of living only in memory until the next build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedEdit {
    pub layout: String,
    pub layer: u8,
    pub key: u16,
    pub code: String,
}

/// A per-key tap dance the user staged but hasn't built into firmware yet.
/// `slots` is `[tap, hold, double_tap, tap_hold]` (same order as the editor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedDance {
    pub layout: String,
    pub layer: u8,
    pub key: u16,
    pub slots: [Option<String>; 4],
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Matrix position `[row, col]` of the key that summons the overlay.
    pub overlay_trigger: Option<[u8; 2]>,
    /// Chord (one or more matrix positions held together) that shows the
    /// minimap while held. Supersedes `overlay_trigger` in the GUI.
    pub overlay_chord: Vec<[u8; 2]>,
    /// Name of the profile the app currently runs (None = default).
    pub active_profile: Option<String>,
    /// USB serial of the last keyboard seen (`hash/revision`), so the GUI can
    /// show that layout from cache when no keyboard is plugged in.
    pub last_layout: Option<String>,
    /// Hidden built-in cheatsheet entries ("category|keys|desc").
    pub hidden_shortcuts: Vec<String>,
    /// App → layer rules shared by `autolayer` and the GUI.
    pub autolayer_rules: Vec<AutolayerRule>,
    /// Per-key glow color overrides (see [`GlowOverride`]).
    pub glow_overrides: Vec<GlowOverride>,
    /// Per-key press effects (see [`KeyFx`]).
    pub key_fx: Vec<KeyFx>,
    /// User-built step-sequence effects from FX Studio.
    pub custom_fx: Vec<crate::gui::CustomFx>,
    /// User-authored extra layers (beyond the Oryx source).
    pub custom_layers: Vec<CustomLayer>,
    /// Per-key remaps staged in the editor but not yet built into firmware.
    pub staged_edits: Vec<StagedEdit>,
    /// Per-key tap dances staged in the editor but not yet built into firmware.
    pub staged_dances: Vec<StagedDance>,
    /// User-added shortcut cheatsheet entries.
    pub custom_shortcuts: Vec<CustomShortcut>,
    /// Board RGB state (constant effect + press reaction).
    pub rgb: RgbState,
    /// Path to a local `qmk_firmware` checkout for offline builds.
    pub qmk_firmware_dir: Option<String>,
    /// Layer-peek HUD settings.
    pub peek: PeekConfig,
    /// Show a live CPU pill in the app header.
    pub show_cpu_header: bool,
    /// Skip the once-per-launch check for a newer release (on by default so a
    /// fresh config checks; the user can turn it off in Settings).
    pub skip_update_check_on_start: bool,
    /// Built-in keyboard guard: re-engage automatically on startup.
    pub guard_enabled: bool,
    /// Autolayer: re-enable the app→layer watcher on startup.
    pub autolayer_enabled: bool,
}

fn path() -> Result<std::path::PathBuf> {
    Ok(cache_dir()?.join("config.json"))
}

pub fn load() -> Config {
    let Ok(p) = path() else { return Config::default() };
    let Ok(bytes) = std::fs::read(&p) else {
        // Missing file = first run; that's a clean default.
        return Config::default();
    };
    match serde_json::from_slice(&bytes) {
        Ok(cfg) => cfg,
        Err(e) => {
            // The file EXISTS but won't parse (corruption, a truncated save, a
            // downgrade past a new enum variant…). Do NOT silently return
            // default and let the next save clobber it - preserve the bytes so
            // the user (or we) can recover, and warn.
            let backup = p.with_extension("json.corrupt");
            let _ = std::fs::write(&backup, &bytes);
            eprintln!(
                "keyjitsu: config.json is unreadable ({e}); backed it up to {} and started from defaults",
                backup.display()
            );
            Config::default()
        }
    }
}

pub fn save(config: &Config) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let bytes = serde_json::to_vec_pretty(config)?;
    // Atomic write: serialize to a temp file, then rename over the target, so a
    // crash / full disk / power loss can never leave a truncated config.json.
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_roundtrip_and_old_config_compat() {
        // New fields round-trip (incl. the [Option<String>;4] dance slots).
        let mut c = Config::default();
        c.staged_edits.push(StagedEdit { layout: "H".into(), layer: 0, key: 1, code: "KC_A".into() });
        c.staged_dances.push(StagedDance {
            layout: "H".into(),
            layer: 0,
            key: 2,
            slots: [Some("KC_A".into()), Some("MO(2)".into()), Some("KC_B".into()), None],
        });
        let js = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&js).unwrap();
        assert_eq!(back.staged_edits.len(), 1);
        assert_eq!(back.staged_dances[0].slots[2].as_deref(), Some("KC_B"));
        assert_eq!(back.staged_dances[0].slots[3], None);
        // An OLD config (no staged_* keys) must still load → empty vecs.
        let old: Config = serde_json::from_str(r#"{"guard_enabled":true}"#).unwrap();
        assert!(old.staged_edits.is_empty() && old.staged_dances.is_empty());
    }
}
