//! Persisted app settings: overlay trigger/chord, RGB + layer-peek HUD,
//! autolayer rules, per-key glow and press effects, custom layers/shortcuts,
//! saved profiles, and local toolchain paths. Writes are atomic and schema
//! migrations are applied before any mutation.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomLayerSet {
    pub layout: String,
    pub layers: Vec<CustomLayer>,
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
/// by layout hash so it survives restarts without becoming profile state.
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

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
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
    /// Legacy flat custom-layer storage kept for migration.
    pub custom_layers: Vec<CustomLayer>,
    /// Desired custom-layer sets that differ from (or have not yet been
    /// confirmed against) the running firmware.
    pub custom_layer_sets: Vec<CustomLayerSet>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub overlay_trigger: Option<[u8; 2]>,
    pub overlay_chord: Vec<[u8; 2]>,
    pub hidden_shortcuts: Vec<String>,
    pub autolayer_rules: Vec<AutolayerRule>,
    pub glow_overrides: Vec<GlowOverride>,
    pub key_fx: Vec<KeyFx>,
    pub custom_fx: Vec<crate::gui::CustomFx>,
    pub custom_shortcuts: Vec<CustomShortcut>,
    pub rgb: RgbState,
    pub peek: PeekConfig,
    pub autolayer_enabled: bool,
}

impl Profile {
    pub fn from_config(c: &Config) -> Self {
        Self {
            overlay_trigger: c.overlay_trigger,
            overlay_chord: c.overlay_chord.clone(),
            hidden_shortcuts: c.hidden_shortcuts.clone(),
            autolayer_rules: c.autolayer_rules.clone(),
            glow_overrides: c.glow_overrides.clone(),
            key_fx: c.key_fx.clone(),
            custom_fx: c.custom_fx.clone(),
            custom_shortcuts: c.custom_shortcuts.clone(),
            rgb: c.rgb.clone(),
            peek: c.peek.clone(),
            autolayer_enabled: c.autolayer_enabled,
        }
    }

    pub fn apply_to(&self, c: &mut Config) {
        c.overlay_trigger = self.overlay_trigger;
        c.overlay_chord = self.overlay_chord.clone();
        c.hidden_shortcuts = self.hidden_shortcuts.clone();
        c.autolayer_rules = self.autolayer_rules.clone();
        c.glow_overrides = self.glow_overrides.clone();
        c.key_fx = self.key_fx.clone();
        c.custom_fx = self.custom_fx.clone();
        c.custom_shortcuts = self.custom_shortcuts.clone();
        c.rgb = self.rgb.clone();
        c.peek = self.peek.clone();
        c.autolayer_enabled = self.autolayer_enabled;
    }
}

fn path() -> Result<std::path::PathBuf> {
    Ok(cache_dir()?.join("config.json"))
}

fn migrate(mut cfg: Config) -> Result<Config> {
    if cfg.schema_version > CURRENT_SCHEMA_VERSION {
        bail!(
            "config schema {} is newer than this Keyjitsu supports ({CURRENT_SCHEMA_VERSION})",
            cfg.schema_version
        );
    }

    if cfg.schema_version == 0 {
        if cfg.custom_layer_sets.is_empty() && !cfg.custom_layers.is_empty() {
            for layer in &cfg.custom_layers {
                if let Some(set) = cfg
                    .custom_layer_sets
                    .iter_mut()
                    .find(|set| set.layout == layer.layout)
                {
                    set.layers.push(layer.clone());
                } else {
                    cfg.custom_layer_sets.push(CustomLayerSet {
                        layout: layer.layout.clone(),
                        layers: vec![layer.clone()],
                    });
                }
            }
            cfg.custom_layers.clear();
        }
        cfg.schema_version = 1;
    }

    Ok(cfg)
}


pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("persisted file has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary file in {}", parent.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("writing temporary file for {}", path.display()))?;
    tmp.as_file_mut()
        .sync_all()
        .with_context(|| format!("syncing temporary file for {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn load_checked() -> Result<Config> {
    let p = path()?;
    let bytes = match std::fs::read(&p) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut cfg = Config::default();
            cfg.schema_version = CURRENT_SCHEMA_VERSION;
            return Ok(cfg);
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    match serde_json::from_slice(&bytes) {
        Ok(cfg) => migrate(cfg),
        Err(e) => {
            let backup = p.with_extension("json.corrupt");
            write_atomic(&backup, &bytes).with_context(|| {
                format!(
                    "config is unreadable ({e}); failed to preserve the original bytes in {}",
                    backup.display()
                )
            })?;
            Err(e).with_context(|| {
                format!(
                    "config is unreadable; preserved the original bytes in {}",
                    backup.display()
                )
            })
        }
    }
}

pub fn load() -> Config {
    match load_checked() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("keyjitsu: {e:#}");
            Config::default()
        }
    }
}

pub fn update(f: impl FnOnce(&mut Config)) -> Result<()> {
    let mut cfg = load_checked()?;
    f(&mut cfg);
    save(&cfg)
}

pub fn save(config: &Config) -> Result<()> {
    let p = path()?;
    let config = migrate(config.clone())?;
    let bytes = serde_json::to_vec_pretty(&config)?;
    write_atomic(&p, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_does_not_overwrite_device_state() {
        let mut cfg = Config::default();
        cfg.last_layout = Some("layout/rev~kj0123456789".into());
        cfg.qmk_firmware_dir = Some("/qmk".into());
        cfg.staged_edits.push(StagedEdit { layout: "layout".into(), layer: 0, key: 1, code: "KC_A".into() });
        cfg.custom_layers.push(CustomLayer {
            layout: "layout".into(),
            name: "Extra".into(),
            keys: vec![CustomKey { key: 2, code: "KC_B".into() }],
        });

        let mut profile = Profile::from_config(&cfg);
        profile.peek.enabled = false;

        let mut target = cfg;
        profile.apply_to(&mut target);
        assert_eq!(target.last_layout.as_deref(), Some("layout/rev~kj0123456789"));
        assert_eq!(target.qmk_firmware_dir.as_deref(), Some("/qmk"));
        assert_eq!(target.staged_edits.len(), 1);
        assert_eq!(target.custom_layers.len(), 1);
        assert!(!target.peek.enabled);
    }

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
        let old = migrate(old).unwrap();
        assert_eq!(old.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(old.staged_edits.is_empty() && old.staged_dances.is_empty());
        assert!(old.custom_layer_sets.is_empty());
    }

    #[test]
    fn migrates_legacy_custom_layers_into_pending_sets() {
        let mut cfg = Config::default();
        cfg.custom_layers.push(CustomLayer {
            layout: "layout-a".into(),
            name: "One".into(),
            keys: vec![],
        });
        cfg.custom_layers.push(CustomLayer {
            layout: "layout-a".into(),
            name: "Two".into(),
            keys: vec![],
        });

        let cfg = migrate(cfg).unwrap();
        assert!(cfg.custom_layers.is_empty());
        assert_eq!(cfg.custom_layer_sets.len(), 1);
        assert_eq!(cfg.custom_layer_sets[0].layers.len(), 2);
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn rejects_newer_config_schema() {
        let mut cfg = Config::default();
        cfg.schema_version = CURRENT_SCHEMA_VERSION + 1;
        assert!(migrate(cfg).is_err());
    }
}
