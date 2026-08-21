//! `keyjitsu gui` (also plain `keyjitsu`) - the windowed app: Keymapp feature
//! parity (live view, layers, heatmap, flashing) plus keyjitsu's extras
//! (per-key RGB, built-in-keyboard guard, autolayer rules).

mod rgb_anim;
mod widget;
mod worker;

pub use rgb_anim::{CustomFx, Effect, FxStep, FxTrigger, PressEffect};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rgb_anim::{Anim, FxEvent};

use anyhow::{anyhow, Result};
use eframe::egui::{self, Color32, ProgressBar, RichText};

use std::time::Instant;

use crate::config::{
    self, AutolayerRule, GlowOverride, HAlign, PeekConfig, StagedDance, StagedEdit, VAlign,
};
use crate::geometry::{self, Geometry};
use crate::heatmap::{normalize, HeatmapStore};
use crate::legend::{self, labels_for};
use crate::keycodes;
use crate::localbuild::{self, BuildMsg, KeyEdit};
use crate::perf;
use crate::oryx_api::{KeyAction, Layer, Layout, LayoutId, OryxKey};
use crate::protocol::Event;
use widget::{draw_keyboard, parse_hex};
use worker::{DevEvent, FlashState, KbCmd};

pub fn run(serial: Option<String>) -> Result<()> {
    // Safety net for the built-in-keyboard guard (see macos_kb):
    // 1. heal a stale guard left by a previous crash/kill,
    // 2. restore on SIGINT/SIGTERM so `kill`/pkill can't leave the Mac
    //    keyboard disabled (SIGKILL is uncatchable - the startup heal above
    //    covers that on next launch).
    #[cfg(target_os = "macos")]
    {
        if crate::macos_kb::heal_stale_guard() {
            eprintln!("keyjitsu: restored the built-in keyboard from a previous session's guard");
        }
        let _ = ctrlc::set_handler(|| {
            crate::macos_kb::force_restore_if_active();
            std::process::exit(0);
        });
        // Restore the built-in keyboard around the lock screen so the guard can
        // never lock you out (login window is always usable; re-disabled on
        // unlock). No-op while the guard is off.
        crate::macos_lockwatch::install();
    }

    let options = eframe::NativeOptions {
        // Request an alpha-capable framebuffer so the peek viewport can be
        // genuinely see-through (not just fade against an opaque clear).
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1020.0, 620.0])
            .with_min_inner_size([760.0, 420.0])
            .with_transparent(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../resources/icon_256.png"))
                    .unwrap_or_default(),
            )
            .with_title("Keyjitsu - Voyager keyboard mapper"),
        ..Default::default()
    };
    eframe::run_native(
        "keyjitsu",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, serial);
            // QA: preview the build modal without running a build.
            if std::env::var("KEYJITSU_BUILD_DEMO").is_ok() {
                app.build_open = true;
                app.build_busy = true;
                app.build_phase = "Compiling firmware…".into();
                app.build_progress = 0.62;
                app.build_log = "Fetching generated source for revision wODgzD…\nApplying 1 key change(s) to keymap.c…\nGenerating 1 tap dance(s)…\nCompiling: quantum/keymap_introspection.c            [OK]\nCompiling: platforms/chibios/hardware_id.c           [OK]\nCompiling: quantum/process_keycode/process_tap_dance.c [OK]\nLinking: .build/zsa_voyager_keyjitsu.elf".into();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow!("gui failed: {e}"))
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Live,
    Layers,
    Heatmap,
    Peek,
    Fx,
    Perf,
    Auto,
    Tools,
}

/// What's selected in the FX Studio library.
#[derive(PartialEq, Clone, Copy)]
enum FxSel {
    Const(Effect),
    Press(PressEffect),
    /// Index into `App::custom_fx` - a user-built step sequence.
    Custom(usize),
}

/// One recorded key gesture for the combo HUD.
#[derive(Clone)]
struct ComboEntry {
    key: usize,
    /// 1 = single, 2 = double tap.
    count: u8,
    /// The (final) press was held long enough to count as a hold.
    held: bool,
    /// Measured duration of the final press, in ms.
    hold_ms: u128,
    /// Press-to-press gap to the previous tap (double-tap only), in ms.
    gap_ms: u128,
    /// When this entry's (first) key press went DOWN - used for the
    /// press-to-press double-tap window.
    down_at: Instant,
    /// Last time this entry was touched (for the fade-out).
    at: Instant,
}

/// A combo chip prepared for display, with its measured timings.
struct ComboChip {
    label: String,
    count: u8,
    held: bool,
    live: bool,
    /// Live: elapsed hold so far; finalized: the press duration. In ms.
    ms: u128,
    /// Press-to-press gap for a double-tap, in ms (0 if n/a).
    gap_ms: u128,
}

/// Which FX Studio library category is browsed (picked in the sidebar).
#[derive(PartialEq, Clone, Copy)]
enum FxLib {
    Const,
    Press,
    Custom,
    /// The board-level application panel (constant effect + press reaction).
    Apply,
}

/// The four Oryx-style action slots of a key, shown as editor rows.
/// Index into `App::edit_slots`: 0 tap, 1 hold, 2 double-tap, 3 tap+hold.
const SLOT_LABELS: [&str; 4] = ["Tap", "Hold", "Double-tap", "Double-tap + hold"];
/// Badge color per action tier (tap violet, hold cyan, double amber, t+h green).
const SLOT_COLORS: [Color32; 4] = [
    Color32::from_rgb(0x8B, 0x5C, 0xF6),
    Color32::from_rgb(0x22, 0xD3, 0xEE),
    Color32::from_rgb(0xF5, 0x9E, 0x0B),
    Color32::from_rgb(0x34, 0xD3, 0x99),
];

/// Wrap a tap keycode with a hold action: `MO(n)` → `LT(n,tap)`, a plain
/// modifier → the matching mod-tap macro. None = not expressible as MT/LT.
fn hold_wrap(hold: &str, tap: &str) -> Option<String> {
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


struct App {
    egui_ctx: egui::Context,
    erx: Receiver<DevEvent>,
    cmd_tx: Sender<KbCmd>,

    connected: Option<(String, String)>, // (model, serial/layout-id)
    layout: Option<Layout>,
    heat: Option<HeatmapStore>,

    active_layer: u8,
    view_layer: u8,
    follow: bool,
    pressed: Vec<bool>,

    tab: Tab,

    // Heatmap tab
    heat_layer: Option<u8>, // None = all layers summed
    confirm_reset: bool,

    // Glow editor (Live tab)
    layout_hash: Option<String>,
    /// User-authored extra layers for the current layout (persisted).
    custom_layers: Vec<config::CustomLayer>,
    /// Synthesized `Layer`s for `custom_layers`, so `layer_def` can hand out
    /// references. Rebuilt whenever `custom_layers` changes.
    synth_layers: Vec<Layer>,
    /// Inline "new layer name" field state (sidebar).
    new_layer_open: bool,
    new_layer_name: String,
    glow_work: HashMap<(u8, usize), [u8; 3]>, // being edited
    glow_saved: HashMap<(u8, usize), [u8; 3]>, // persisted snapshot
    selected_key: Option<usize>, // shown in the bottom config panel
    edit_color: [u8; 3],
    sync_glow: bool,   // mirror the glow onto the physical keyboard
    needs_push: bool,  // re-push colors on next frame
    show_flash: bool,  // Flash section expanded in Live

    // Local firmware editing (QMK)
    key_edits: HashMap<(u8, usize), String>, // (layer, led pos) → new keycode
    /// Working keycodes per action slot of the selected key (see SLOT_LABELS).
    edit_slots: [Option<String>; 4],
    /// Rows the user added with ＋ that have no code picked yet.
    slot_added: [bool; 4],
    /// Which slot the key picker is currently feeding.
    picker_slot: usize,
    /// (layer,key) the slot editor is hydrated for (re-syncs on change).
    edit_synced: Option<(u8, usize)>,
    /// Staged tap dances: keys whose double-tap/tap+hold slots need TD().
    key_dances: HashMap<(u8, usize), [Option<String>; 4]>,
    build_rx: Option<Receiver<BuildMsg>>,
    build_log: String,
    build_busy: bool,
    build_flash_after: bool,
    last_build_bin: Option<std::path::PathBuf>,
    build_cancel: Arc<AtomicBool>,
    /// Build/flash progress modal: open, current phase, 0..1 progress, and a
    /// running count of compiled files (for the compile-band estimate).
    build_open: bool,
    build_phase: String,
    build_progress: f32,
    build_compiles: u32,
    /// Non-None once the run finishes: Ok(msg) or Err(msg) for the result card.
    build_result: Option<Result<String, String>>,
    flash_cancel: Arc<AtomicBool>,
    /// Cached QMK toolchain status (recomputing spawns processes, so never
    /// do it per frame - refresh on a button or lazily).
    env: localbuild::BuildEnv,
    /// Cached monitor list, refreshed periodically (see `update`).
    monitors_cache: Vec<MonitorInfo>,
    monitors_checked: Instant,

    // Keycode picker (Oryx-style)
    picker_open: bool,
    picker_cat: usize,
    picker_search: String,
    picker_layer_arg: u8,

    // Key behavior (tap/hold/one-shot)

    // Layer-peek HUD
    peek: PeekConfig,
    peek_until: Option<Instant>,
    peek_layer: u8,

    // RGB animations (host-driven LED effects)
    anim: Arc<Mutex<Anim>>,
    _anim_handle: rgb_anim::AnimHandle,
    /// Per-key press effects for the current layout: (layer, key) → fx.
    #[allow(clippy::type_complexity)]
    key_fx: HashMap<(u8, usize), (FxTrigger, PressEffect, [u8; 3], Option<String>)>,
    /// Last press time per key, for double-press detection.
    last_press_at: HashMap<usize, Instant>,
    /// Combo HUD: press instant per key (to measure hold), and the recent
    /// gesture log shown on the minimap.
    combo_down: HashMap<usize, Instant>,
    combo_log: std::collections::VecDeque<ComboEntry>,

    // Tools tab
    #[cfg(target_os = "macos")]
    guard: Option<crate::macos_kb::BuiltinKeyboardGuard>,
    guard_enabled: bool,
    guard_error: Option<String>,
    rules: Vec<AutolayerRule>,
    rules_dirty: bool,
    autolayer_enabled: bool,
    autolayer: Option<worker::AutolayerHandle>,

    // Flash tab
    flash_rx: Option<Receiver<FlashState>>,
    flash_state: Option<FlashState>,
    flash_input: String,

    // Performance sampler
    perf_sampler: perf::CpuSampler,
    perf_live: f32,
    perf_tick: Instant,
    perf_run: Option<PerfRun>,
    perf_last: Option<perf::Summary>,
    show_cpu_header: bool,
    /// Check GitHub for a newer release once at startup (Settings toggle).
    auto_update_check: bool,
    app_started: Instant,

    // FX Studio
    fx_sel: FxSel,
    fx_color: [u8; 3],
    fx_speed: f32,
    fx_bright: f32,
    fx_playing: bool,
    /// User-built step sequences (FX Studio), persisted in config.
    custom_fx: Vec<CustomFx>,
    /// FX Studio library category (picked in the sidebar).
    fx_lib: FxLib,
    /// Chord of matrix positions that shows the minimap while held.
    overlay_chord: Vec<[u8; 2]>,
    /// True while waiting for the user to press the combo to bind.
    binding_overlay: bool,
    /// Keys collected during binding (committed on first release).
    binding_draft: Vec<[u8; 2]>,
    /// Hidden built-in cheatsheet entries ("category|keys|desc").
    hidden_shortcuts: Vec<String>,
    /// Active profile name (None = default).
    active_profile: Option<String>,
    /// Inline "new profile" name field open in the sidebar.
    prof_new_open: bool,

    // Shortcuts tab
    /// Active cheatsheet category (None = all).
    keys_cat: Option<String>,
    keys_search: String,
    custom_shortcuts: Vec<config::CustomShortcut>,
    keys_adding: bool,
    /// Draft name for "save current as profile" (Settings).
    profile_draft: String,
    /// Last autostart toggle error (shown in the App card).
    autostart_error: Option<String>,
    /// In-flight "check for updates" request (manual, from Settings).
    update_rx: Option<std::sync::mpsc::Receiver<UpdateCheck>>,
    update_state: Option<UpdateCheck>,
    draft_sc: config::CustomShortcut,
    /// Active step being painted in the custom-effect editor.
    fx_step: usize,
    /// Board test in progress: restore this effect at this time.
    fx_board_restore: Option<(Instant, Effect)>,
    fx_t0: Instant,
    fx_events: Vec<FxEvent>,
    fx_last_fire: Instant,

    // Heatmap extras
    csv_saved: Option<std::path::PathBuf>,
}

/// One phase of the "compare modes" test.
#[derive(Clone)]
struct PerfPhase {
    label: String,
    secs: u64,
    anim: Effect,
    peek: bool,
}

/// An in-progress sampling session.
struct PerfRun {
    samples: Vec<(String, f32)>,
    sampler: perf::CpuSampler,
    started: Instant,
    next_sample: Instant,
    end_at: Instant,
    /// Empty = passive 5-min observation; non-empty = scripted mode comparison.
    phases: Vec<PerfPhase>,
    phase_i: usize,
    phase_until: Instant,
    /// Constant effect to restore after a scripted run.
    restore: Option<Effect>,
}

/// A press held longer than this (ms) reads as a "hold" gesture in the combo
/// readout, not a tap. Shared by `record_combo` and `combo_recent`.
const HOLD_MS: u128 = 180;

/// The Voyager board's span in key units, used to size the layer-peek HUD.
/// Width is the LAYOUT column count; height includes the rotated thumb swing.
/// Named so the standalone peek window and the in-app preview stay in step
/// (they drifted at 6.45 vs 6.4 before).
const PEEK_BOARD_UNITS_WIDE: f32 = 14.5;
const PEEK_BOARD_UNITS_TALL: f32 = 6.45;

/// The app's color system: violet is the brand, cyan is the UI "selected"
/// state, amber = unsaved/warning, red = error. Layout colors on the keys stay
/// faithful but calm - these are for the app chrome.
pub(crate) mod pal {
    use eframe::egui::Color32 as C;
    pub const BG: C = C::from_rgb(0x0F, 0x10, 0x15); // keyboard canvas (darkest)
    pub const SURFACE: C = C::from_rgb(0x18, 0x1A, 0x22); // panels / inspector
    pub const CARD: C = C::from_rgb(0x1D, 0x20, 0x2A); // cards (raised over surface)
    pub const INPUT: C = C::from_rgb(0x25, 0x28, 0x36); // inputs / controls
    pub const RAISED: C = INPUT; // same tone; named for "raised control" call sites
    pub const HOVER: C = C::from_rgb(0x30, 0x34, 0x45);
    pub const BORDER: C = C::from_rgb(0x3A, 0x3E, 0x50);
    pub const VIOLET: C = C::from_rgb(0x8B, 0x5C, 0xF6);
    pub const VIOLET_HI: C = C::from_rgb(0xA7, 0x8B, 0xFA);
    pub const CYAN: C = C::from_rgb(0x22, 0xD3, 0xEE);
    pub const GREEN: C = C::from_rgb(0x22, 0xC5, 0x5E);
    pub const AMBER: C = C::from_rgb(0xF5, 0x9E, 0x0B);
    pub const RED: C = C::from_rgb(0xEF, 0x44, 0x44);
    pub const TEXT: C = C::from_rgb(0xE5, 0xE7, 0xEB);
    pub const TEXT_MUTED: C = C::from_rgb(0x9C, 0xA3, 0xAF);
    pub const TEXT_DIM: C = C::from_rgb(0x6B, 0x72, 0x80);
}

/// A cohesive, rounded dark devtool theme.
fn setup_style(ctx: &egui::Context) {
    use egui::{CornerRadius, Stroke};

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(11.0, 5.0);
    style.spacing.interact_size.y = 27.0;
    style.spacing.window_margin = egui::Margin::same(12);
    style.spacing.menu_margin = egui::Margin::same(8);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(pal::TEXT);
    v.panel_fill = pal::SURFACE;
    v.window_fill = pal::SURFACE;
    v.window_stroke = Stroke::new(1.0, pal::BORDER);
    v.extreme_bg_color = pal::INPUT; // text-edit background
    v.faint_bg_color = pal::CARD;
    v.hyperlink_color = pal::CYAN;
    v.selection.bg_fill = pal::VIOLET.gamma_multiply(0.40);
    v.selection.stroke = Stroke::new(1.0, pal::VIOLET);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);

    let round = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.corner_radius = round;
    w.noninteractive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    w.inactive.corner_radius = round;
    w.inactive.bg_fill = pal::RAISED;
    w.inactive.weak_bg_fill = pal::RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    w.hovered.corner_radius = round;
    w.hovered.bg_fill = pal::HOVER;
    w.hovered.weak_bg_fill = pal::HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, pal::VIOLET.gamma_multiply(0.7));
    w.active.corner_radius = round;
    w.active.bg_fill = pal::VIOLET.gamma_multiply(0.65);
    w.active.weak_bg_fill = pal::VIOLET.gamma_multiply(0.6);
    w.active.bg_stroke = Stroke::new(1.0, pal::VIOLET);
    w.open.corner_radius = round;

    style.visuals = v;
    ctx.set_style(style);
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Bundled Noto Sans Symbols 2 (OFL): consistent, well-hinted glyphs for
    // ⇧ ⌃ ⌥ ⌘ ▽ ⏯ etc. Inserted right after the main text font, so symbol
    // legends render solid instead of thin/jagged system-font fallbacks.
    fonts.font_data.insert(
        "noto-symbols".to_owned(),
        egui::FontData::from_static(include_bytes!("../../resources/NotoSansSymbols2-Regular.ttf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        let pos = 1.min(list.len());
        list.insert(pos, "noto-symbols".to_owned());
    }

    // System fonts stay as a wide-coverage net behind Noto.
    #[cfg(target_os = "macos")]
    for (name, path) in [
        ("apple-symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
        ("arial-unicode", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.to_owned());
            }
        }
    }
    ctx.set_fonts(fonts);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, serial: Option<String>) -> App {
        setup_style(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let (erx, cmd_tx) = worker::spawn_device_worker(serial, cc.egui_ctx.clone());
        let cfg = config::load();
        let key_count = geometry::voyager().len();
        // RGB animation engine: a background thread drives the LEDs when an
        // effect is active (Off by default, so it just idles).
        let anim = Arc::new(Mutex::new(Anim::default()));
        // Restore the persisted board RGB state (constant effect + press fx).
        if let Ok(mut a) = anim.lock() {
            let r = &cfg.rgb;
            a.effect = r.effect;
            a.color = r.color;
            a.speed = r.speed;
            a.brightness = r.brightness;
            a.press_effect = r.press_effect;
            a.press_color = r.press_color;
            a.custom_name = r.custom_name.clone();
            a.custom = cfg
                .custom_fx
                .iter()
                .find(|c| c.name == r.custom_name)
                .map(|c| c.steps.clone())
                .unwrap_or_default();
            if a.effect == Effect::Custom && a.custom.is_empty() {
                a.effect = Effect::Off;
            }
        }
        let anim_handle = rgb_anim::spawn(anim.clone(), cmd_tx.clone(), cc.egui_ctx.clone());
        let mut app = App {
            egui_ctx: cc.egui_ctx.clone(),
            erx,
            cmd_tx,
            connected: None,
            layout: None,
            heat: None,
            active_layer: 0,
            view_layer: 0,
            follow: true,
            pressed: vec![false; key_count],
            // KEYJITSU_TAB lets tooling/screenshots open straight on a tab.
            tab: match std::env::var("KEYJITSU_TAB").as_deref() {
                Ok("heatmap") => Tab::Heatmap,
                Ok("peek") => Tab::Peek,
                Ok("layers") => Tab::Layers,
                Ok("fx") => Tab::Fx,
                Ok("keys") | Ok("library") => Tab::Tools,
                Ok("perf") => Tab::Perf,
                Ok("autolayer") => Tab::Auto,
                Ok("tools") | Ok("settings") => Tab::Tools,
                _ => Tab::Live,
            },
            // KEYJITSU_FX=custom<i> preselects a custom effect (QA screenshots).
            fx_sel: match std::env::var("KEYJITSU_FX").ok().and_then(|v| v.strip_prefix("custom").and_then(|n| n.parse::<usize>().ok())) {
                Some(i) if i < cfg.custom_fx.len() => FxSel::Custom(i),
                _ => FxSel::Press(PressEffect::Ripple),
            },
            fx_lib: if std::env::var("KEYJITSU_FX").is_ok() { FxLib::Custom } else { FxLib::Press },
            overlay_chord: if cfg.overlay_chord.is_empty() {
                cfg.overlay_trigger.map(|t| vec![t]).unwrap_or_default()
            } else {
                cfg.overlay_chord.clone()
            },
            binding_overlay: false,
            binding_draft: Vec::new(),
            hidden_shortcuts: cfg.hidden_shortcuts.clone(),
            active_profile: cfg.active_profile.clone(),
            prof_new_open: false,
            keys_cat: None,
            keys_search: String::new(),
            custom_shortcuts: cfg.custom_shortcuts.clone(),
            keys_adding: false,
            profile_draft: String::new(),
            autostart_error: None,
            update_rx: None,
            update_state: None,
            draft_sc: config::CustomShortcut { category: String::new(), keys: String::new(), desc: String::new(), high: true },
            // KEYJITSU_SEL=<key index> preselects a key (QA screenshots).
            selected_key: std::env::var("KEYJITSU_SEL").ok().and_then(|v| v.parse().ok()).filter(|&i| i < key_count),
            heat_layer: None,
            confirm_reset: false,
            layout_hash: None,
            custom_layers: Vec::new(),
            synth_layers: Vec::new(),
            new_layer_open: false,
            new_layer_name: String::new(),
            glow_work: HashMap::new(),
            glow_saved: HashMap::new(),
            edit_color: [160, 90, 255],
            sync_glow: false,
            needs_push: false,
            show_flash: false,
            key_edits: HashMap::new(),
            edit_slots: [None, None, None, None],
            slot_added: [false; 4],
            picker_slot: 0,
            edit_synced: None,
            key_dances: HashMap::new(),
            build_rx: None,
            build_log: String::new(),
            build_busy: false,
            build_flash_after: true,
            build_open: false,
            build_phase: String::new(),
            build_progress: 0.0,
            build_compiles: 0,
            build_result: None,
            last_build_bin: None,
            build_cancel: Arc::new(AtomicBool::new(false)),
            flash_cancel: Arc::new(AtomicBool::new(false)),
            env: localbuild::detect_env(),
            monitors_cache: fetch_monitors(),
            monitors_checked: Instant::now(),
            picker_open: false,
            picker_cat: 0,
            picker_search: String::new(),
            picker_layer_arg: 1,
            peek: cfg.peek.clone(),
            peek_until: None,
            peek_layer: 0,
            anim,
            _anim_handle: anim_handle,
            key_fx: HashMap::new(),
            last_press_at: HashMap::new(),
            combo_down: HashMap::new(),
            combo_log: std::collections::VecDeque::new(),
            #[cfg(target_os = "macos")]
            guard: None,
            guard_enabled: cfg.guard_enabled,
            guard_error: None,
            rules: cfg.autolayer_rules,
            rules_dirty: false,
            autolayer_enabled: cfg.autolayer_enabled,
            autolayer: None,
            flash_rx: None,
            flash_state: None,
            flash_input: String::new(),
            perf_sampler: perf::CpuSampler::new(),
            perf_live: 0.0,
            perf_tick: Instant::now(),
            perf_run: None,
            perf_last: None,
            show_cpu_header: cfg.show_cpu_header,
            auto_update_check: !cfg.skip_update_check_on_start,
            app_started: Instant::now(),
            fx_color: [140, 108, 246],
            fx_speed: 1.0,
            fx_bright: 0.9,
            fx_playing: true,
            custom_fx: cfg.custom_fx.clone(),
            fx_step: 0,
            fx_board_restore: None,
            fx_t0: Instant::now(),
            fx_events: Vec::new(),
            fx_last_fire: Instant::now(),
            csv_saved: None,
        };
        // No keyboard yet? Show the last layout from cache instead of a wall of
        // blank keys.
        app.load_last_layout();
        // One background check for a newer release (opt-out in Settings).
        if app.auto_update_check {
            app.update_rx = Some(spawn_update_check());
        }
        app
    }

    fn geometry(&self) -> &'static Geometry {
        geometry::voyager()
    }

    /// Number of layers that come from the Oryx source (not counting the
    /// user's own custom layers).
    fn oryx_layer_count(&self) -> u8 {
        self.layout.as_ref().map(|l| l.revision.layers.len() as u8).unwrap_or(0)
    }

    fn layer_def(&self, n: u8) -> Option<&Layer> {
        let oryx = self.oryx_layer_count();
        if n < oryx {
            self.layout
                .as_ref()
                .and_then(|l| l.revision.layers.iter().find(|la| la.position == n))
        } else {
            self.synth_layers.get((n - oryx) as usize)
        }
    }

    fn layer_count(&self) -> u8 {
        (self.oryx_layer_count() + self.custom_layers.len() as u8)
            .max(1)
            .max(self.active_layer + 1)
    }

    /// True if layer `n` is one the user authored (editable in place, no Oryx
    /// source behind it). Requires a loaded layout (else there's no baseline).
    fn is_custom_layer(&self, n: u8) -> bool {
        self.layout.is_some() && n >= self.oryx_layer_count()
    }

    /// Index into `custom_layers` for layer `n`, if it's a custom one.
    fn custom_index(&self, n: u8) -> Option<usize> {
        let oryx = self.oryx_layer_count();
        (n >= oryx).then(|| (n - oryx) as usize).filter(|&i| i < self.custom_layers.len())
    }

    /// Load the current layout's custom layers from config and (re)synthesize.
    fn hydrate_custom_layers(&mut self, hash: &str) {
        let cfg = config::load();
        self.custom_layers = cfg.custom_layers.into_iter().filter(|c| c.layout == hash).collect();
        self.rebuild_synth_layers();
    }

    /// Persist custom layers for this layout (replacing its slice).
    fn save_custom_layers(&mut self) {
        let Some(hash) = self.layout_hash.clone() else { return };
        let mut cfg = config::load();
        cfg.custom_layers.retain(|c| c.layout != hash);
        for c in &self.custom_layers {
            cfg.custom_layers.push(c.clone());
        }
        let _ = config::save(&cfg);
        self.rebuild_synth_layers();
    }

    /// Append a new empty custom layer and view it.
    fn add_custom_layer(&mut self, name: String) {
        let Some(hash) = self.layout_hash.clone() else { return };
        self.custom_layers.push(config::CustomLayer { layout: hash, name, keys: Vec::new() });
        self.save_custom_layers();
        self.view_layer = self.layer_count() - 1;
        self.follow = false;
    }

    /// Remove custom layer `n`, renumbering everything that referred to a
    /// higher layer down by one - so colors/effects/staged edits and
    /// layer-switch keycodes don't silently point at the wrong layer.
    fn remove_custom_layer(&mut self, del: u8) {
        let Some(i) = self.custom_index(del) else { return };
        self.custom_layers.remove(i);

        // 1. (layer, key)-keyed maps: drop the deleted layer, shift higher down.
        fn shift<V>(map: &mut HashMap<(u8, usize), V>, del: u8) {
            let taken = std::mem::take(map);
            *map = taken
                .into_iter()
                .filter(|((l, _), _)| *l != del)
                .map(|((l, k), v)| ((if l > del { l - 1 } else { l }, k), v))
                .collect();
        }
        shift(&mut self.glow_work, del);
        shift(&mut self.glow_saved, del);
        shift(&mut self.key_fx, del);
        shift(&mut self.key_edits, del);
        shift(&mut self.key_dances, del);

        // 2. Layer-switch keycode strings pointing above `del` shift down.
        for cl in &mut self.custom_layers {
            for k in &mut cl.keys {
                k.code = renumber_layer_ref(&k.code, del);
            }
        }
        for code in self.key_edits.values_mut() {
            *code = renumber_layer_ref(code, del);
        }
        for slots in self.key_dances.values_mut() {
            for slot in slots.iter_mut().flatten() {
                *slot = renumber_layer_ref(slot, del);
            }
        }

        self.save_custom_layers();
        self.save_glow();
        self.save_key_fx();
        self.save_staged(); // key_edits/key_dances were shifted + renumbered above
        self.view_layer = self.view_layer.min(self.layer_count().saturating_sub(1));
        self.edit_synced = None; // re-hydrate the editor for the new indices
    }

    fn rename_custom_layer(&mut self, n: u8, name: String) {
        if let Some(i) = self.custom_index(n) {
            self.custom_layers[i].name = name;
            self.save_custom_layers();
        }
    }

    /// Assign a keycode to a key on a custom layer (persisted). Empty/KC_NO
    /// clears it.
    fn set_custom_key(&mut self, n: u8, key: usize, code: &str) {
        let Some(i) = self.custom_index(n) else { return };
        let cl = &mut self.custom_layers[i];
        cl.keys.retain(|k| k.key != key as u16);
        if !code.is_empty() && code != "KC_NO" && code != "KC_TRANSPARENT" {
            cl.keys.push(config::CustomKey { key: key as u16, code: code.to_string() });
        }
        self.save_custom_layers();
    }

    /// Build display `Layer`s from `custom_layers`: a transparent board with
    /// the user's assigned keycodes rendered as legends.
    fn rebuild_synth_layers(&mut self) {
        let oryx = self.oryx_layer_count();
        let n_keys = self.geometry().len();
        self.synth_layers = self
            .custom_layers
            .iter()
            .enumerate()
            .map(|(i, cl)| {
                let mut keys: Vec<OryxKey> = (0..n_keys).map(|_| OryxKey::default()).collect();
                for ck in &cl.keys {
                    if let Some(slot) = keys.get_mut(ck.key as usize) {
                        *slot = synth_key(&ck.code);
                    }
                }
                Layer {
                    title: Some(cl.name.clone()),
                    position: oryx + i as u8,
                    color: None,
                    keys,
                }
            })
            .collect();
    }

    // --- glow editor ---------------------------------------------------

    /// With no keyboard attached, show the last layout we saw (from cache)
    /// instead of a grid of blank keys. Cache-only, so it never blocks on the
    /// network; a no-op on the very first run (nothing cached yet).
    fn load_last_layout(&mut self) {
        if self.layout.is_some() {
            return;
        }
        // Prefer the remembered serial; fall back to any layout already cached
        // (covers users who cached a layout before this feature existed).
        let found = config::load()
            .last_layout
            .and_then(|s| LayoutId::from_serial(&s).ok())
            .and_then(|id| crate::oryx_api::cached_layout(&id, "voyager").map(|l| (id, l)))
            .or_else(|| crate::oryx_api::any_cached_layout("voyager"));
        let Some((id, layout)) = found else { return };
        self.layout = Some(layout);
        self.hydrate_glow(&id.hash); // also sets self.layout_hash
        self.hydrate_key_fx(&id.hash);
        self.hydrate_custom_layers(&id.hash);
        self.hydrate_staged(&id.hash);
        self.heat = HeatmapStore::load(&id.hash, self.geometry().len()).ok();
        self.push_anim_base();
    }

    /// Load saved glow overrides for a layout into the working + saved maps.
    fn hydrate_glow(&mut self, hash: &str) {
        let cfg = config::load();
        let map: HashMap<(u8, usize), [u8; 3]> = cfg
            .glow_overrides
            .iter()
            .filter(|o| o.layout == hash)
            .map(|o| ((o.layer, o.key as usize), o.rgb))
            .collect();
        self.layout_hash = Some(hash.to_string());
        self.glow_saved = map.clone();
        self.glow_work = map;
        if !self.glow_work.is_empty() && self.sync_glow {
            self.needs_push = true;
        }
    }

    /// The layout's own glow color for a key: its explicit `glowColor`, or the
    /// layer's default color (that's how the firmware lights plain keys).
    fn layout_glow(&self, layer: u8, key: usize) -> Option<Color32> {
        let l = self.layer_def(layer)?;
        l.keys
            .get(key)?
            .glow_color
            .as_deref()
            .and_then(parse_hex)
            .or_else(|| l.color.as_deref().and_then(parse_hex))
    }

    /// Effective glow (override wins over layout) for every key of a layer.
    fn glow_colors(&self, layer: u8) -> Vec<Option<Color32>> {
        (0..self.geometry().len())
            .map(|i| {
                self.glow_work
                    .get(&(layer, i))
                    .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
                    .or_else(|| self.layout_glow(layer, i))
            })
            .collect()
    }

    fn current_key_srgb(&self, layer: u8, key: usize) -> [u8; 3] {
        if let Some(c) = self.glow_work.get(&(layer, key)) {
            *c
        } else if let Some(c) = self.layout_glow(layer, key) {
            [c.r(), c.g(), c.b()]
        } else {
            [0, 0, 0]
        }
    }

    fn set_glow(&mut self, layer: u8, key: usize, rgb: [u8; 3]) {
        self.glow_work.insert((layer, key), rgb);
        if self.sync_glow && layer == self.active_layer {
            self.needs_push = true;
        }
        self.push_anim_base();
    }

    fn clear_glow(&mut self, layer: u8, key: usize) {
        self.glow_work.remove(&(layer, key));
        if self.sync_glow && layer == self.active_layer {
            self.needs_push = true;
        }
        self.push_anim_base();
    }

    // --- per-key press effects -----------------------------------------

    fn hydrate_key_fx(&mut self, hash: &str) {
        let cfg = config::load();
        self.key_fx = cfg
            .key_fx
            .iter()
            .filter(|f| f.layout == hash)
            .map(|f| ((f.layer, f.key as usize), (f.trigger, f.effect, f.color, f.custom.clone())))
            .collect();
    }

    /// Load this layout's staged (not-yet-built) remaps and tap dances into the
    /// working maps, so pending changes survive a restart / profile switch.
    fn hydrate_staged(&mut self, hash: &str) {
        let cfg = config::load();
        self.key_edits = cfg
            .staged_edits
            .iter()
            .filter(|e| e.layout == hash)
            .map(|e| ((e.layer, e.key as usize), e.code.clone()))
            .collect();
        self.key_dances = cfg
            .staged_dances
            .iter()
            .filter(|d| d.layout == hash)
            .map(|d| ((d.layer, d.key as usize), d.slots.clone()))
            .collect();
    }

    /// Persist this layout's staged remaps + tap dances. Mirrors [`Self::save_glow`]:
    /// they used to live only in memory until the next build, so a restart
    /// silently dropped them.
    fn save_staged(&self) {
        let Some(hash) = self.layout_hash.clone() else { return };
        let mut cfg = config::load();
        cfg.staged_edits.retain(|e| e.layout != hash);
        cfg.staged_dances.retain(|d| d.layout != hash);
        for (&(layer, key), code) in &self.key_edits {
            cfg.staged_edits.push(StagedEdit { layout: hash.clone(), layer, key: key as u16, code: code.clone() });
        }
        for (&(layer, key), slots) in &self.key_dances {
            cfg.staged_dances.push(StagedDance { layout: hash.clone(), layer, key: key as u16, slots: slots.clone() });
        }
        let _ = config::save(&cfg);
    }

    /// Persist the current key_fx map for this layout.
    fn save_key_fx(&self) {
        let Some(hash) = self.layout_hash.clone() else { return };
        let mut cfg = config::load();
        cfg.key_fx.retain(|f| f.layout != hash);
        for (&(layer, key), (trigger, effect, color, custom)) in &self.key_fx {
            cfg.key_fx.push(config::KeyFx {
                layout: hash.clone(),
                layer,
                key: key as u16,
                trigger: *trigger,
                effect: *effect,
                color: *color,
                custom: custom.clone(),
            });
        }
        let _ = config::save(&cfg);
    }

    /// Persist the user-built custom effects.
    fn save_custom_fx(&self) {
        let mut cfg = config::load();
        cfg.custom_fx = self.custom_fx.clone();
        let _ = config::save(&cfg);
    }

    /// Translate a custom-effect step by one grid unit; keys that would land
    /// off the board are dropped (a line "walks off" the edge).
    fn shift_step(&mut self, fx: usize, step: usize, dx: f32, dy: f32) {
        let pos: Vec<(f32, f32)> = {
            let g = self.geometry();
            g.keys.iter().map(|k| (k.x, k.y)).collect()
        };
        let Some(s) = self.custom_fx.get_mut(fx).and_then(|c| c.steps.get_mut(step)) else { return };
        let mut out: Vec<u16> = s
            .keys
            .iter()
            .filter_map(|&k| {
                let &(x, y) = pos.get(k as usize)?;
                let (tx, ty) = (x + dx, y + dy);
                pos.iter()
                    .position(|&(px, py)| (px - tx).abs() < 0.45 && (py - ty).abs() < 0.45)
                    .map(|j| j as u16)
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        s.keys = out;
        self.save_custom_fx();
    }

    /// Classify a just-released key into a gesture and append to the combo
    /// log: single/double tap, and whether the final press was a hold. A
    /// second tap of the same key within the window upgrades the last entry.
    fn record_combo(&mut self, idx: usize) {
        let now = Instant::now();
        let down = self.combo_down.remove(&idx);
        let held = down.map(|d| now.duration_since(d).as_millis() > HOLD_MS).unwrap_or(false);
        let down_at = down.unwrap_or(now);
        let hold_ms = down.map(|d| now.duration_since(d).as_millis()).unwrap_or(0);
        // Double-tap is measured PRESS-to-PRESS (like a double-click), so the
        // hold time of the first tap doesn't eat the window.
        let gap_to_prev = self
            .combo_log
            .back()
            .and_then(|e| down.map(|d| (e.key, e.count, d.saturating_duration_since(e.down_at).as_millis())));
        let merged = combo_merges(gap_to_prev, idx);
        if merged {
            let gap = gap_to_prev.map(|(_, _, g)| g).unwrap_or(0);
            if let Some(last) = self.combo_log.back_mut() {
                last.count = 2;
                last.held = held;
                last.hold_ms = hold_ms;
                last.gap_ms = gap;
                last.at = now;
            }
        } else {
            self.combo_log.push_back(ComboEntry { key: idx, count: 1, held, hold_ms, gap_ms: 0, down_at, at: now });
            while self.combo_log.len() > 10 {
                self.combo_log.pop_front();
            }
        }
        // Keep the HUD up so the just-finalized chip (⇩ / ×2) stays visible.
        if self.peek.show_combo && self.peek.enabled {
            self.peek_layer = self.active_layer;
            self.peek_until = Some(now + Duration::from_millis(1600));
        }
    }

    /// Recent combo chips (oldest→newest) with their measured timings:
    /// finalized presses (dropped after ~2.5s) plus any key currently held,
    /// shown live with a counting-up hold duration.
    fn combo_recent(&self) -> Vec<ComboChip> {
        let now = Instant::now();
        let layer = self.layer_def(self.active_layer);
        let label = |k: usize| {
            layer
                .and_then(|l| l.keys.get(k))
                .map(|key| labels_for(key).tap)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("k{k}"))
        };
        let mut out: Vec<ComboChip> = self
            .combo_log
            .iter()
            .filter(|e| now.duration_since(e.at) < Duration::from_millis(2500))
            .rev()
            .take(6)
            .map(|e| ComboChip {
                label: label(e.key),
                count: e.count,
                held: e.held,
                live: false,
                ms: e.hold_ms,
                gap_ms: e.gap_ms,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // Live "holding" chips for keys still physically down past threshold,
        // with the hold duration counting up in real time.
        let mut live: Vec<(usize, u128)> = self
            .combo_down
            .iter()
            .filter(|(_, &d)| {
                let ms = now.duration_since(d).as_millis();
                ms > HOLD_MS && ms < 8000 // ignore stale (a missed KeyUp)
            })
            .map(|(&k, &d)| (k, now.duration_since(d).as_millis()))
            .collect();
        live.sort_by_key(|&(_, ms)| ms);
        for (k, ms) in live {
            out.push(ComboChip { label: label(k), count: 1, held: true, live: true, ms, gap_ms: 0 });
        }
        out
    }

    /// On a key press: fire its per-key effect (honoring the double-press
    /// trigger) or fall back to the global press effect.
    fn fire_key_fx(&mut self, idx: usize) {
        let now = Instant::now();
        let is_double = self
            .last_press_at
            .insert(idx, now)
            .is_some_and(|prev| now.duration_since(prev) < Duration::from_millis(400));

        let per_key = self.key_fx.get(&(self.active_layer, idx)).cloned();
        let fired = match per_key {
            Some((FxTrigger::Press, effect, color, custom)) => Some((effect, color, custom)),
            Some((FxTrigger::DoublePress, effect, color, custom)) if is_double => Some((effect, color, custom)),
            _ => None,
        };

        // A per-key "★ custom" assignment plays the sequence once.
        let seq = fired.as_ref().and_then(|(_, _, custom)| {
            custom.as_deref().and_then(|name| {
                self.custom_fx
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| std::sync::Arc::new(c.steps.clone()))
            })
        });

        let Ok(mut a) = self.anim.lock() else { return };
        let (effect, color) = match &fired {
            Some((e, c, _)) => (*e, *c),
            // Global fallback (FX Studio → board RGB → on key press).
            None if a.press_effect != PressEffect::None => (a.press_effect, a.press_color),
            None => return,
        };
        if effect == PressEffect::None && seq.is_none() {
            return;
        }
        // Seed from the wall clock so sparkle/matrix patterns differ per press.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ (idx as u64) << 32)
            .unwrap_or(idx as u64);
        a.events.push(FxEvent { key: idx, effect, color, at: now, seed, seq });
    }

    // --- performance sampler -------------------------------------------

    fn perf_state(&self) -> perf::PerfState {
        let (anim, press_fx) = self
            .anim
            .lock()
            .map(|a| (a.effect != Effect::Off, !a.events.is_empty()))
            .unwrap_or((false, false));
        perf::PerfState {
            anim,
            press_fx,
            peek: self.peek_until.is_some_and(|u| Instant::now() < u),
            glow_sync: self.sync_glow,
            connected: self.connected.is_some(),
        }
    }

    fn set_anim_effect(&self, effect: Effect) {
        if let Ok(mut a) = self.anim.lock() {
            a.effect = effect;
        }
    }

    /// Save the CURRENT config under `name` (profiles/<name>.json).
    fn snapshot_profile(&self, name: &str) {
        let name = safe_profile_name(name);
        let name = name.as_str();
        if let Some(dir) = profiles_dir() {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(json) = serde_json::to_vec_pretty(&config::load()) {
                let _ = std::fs::write(dir.join(format!("{name}.json")), json);
            }
        }
    }

    /// Switch to `target` (None = default): snapshot the active profile first
    /// so nothing is lost, then load the target's config.
    fn switch_profile(&mut self, target: Option<String>) {
        let current = self.active_profile.clone().unwrap_or_else(|| "default".into());
        self.snapshot_profile(&current);
        let name = safe_profile_name(&target.clone().unwrap_or_else(|| "default".into()));
        if let Some(dir) = profiles_dir() {
            if let Ok(bytes) = std::fs::read(dir.join(format!("{name}.json"))) {
                if let Ok(cfg) = serde_json::from_slice::<config::Config>(&bytes) {
                    let _ = config::save(&cfg);
                    self.apply_config(cfg);
                }
            }
        }
        self.active_profile = target.clone();
        let mut cfg = config::load();
        cfg.active_profile = target;
        let _ = config::save(&cfg);
    }

    /// Sidebar profile switcher: default + saved profiles, with new/clone/
    /// delete actions. Switching snapshots the active profile first.
    fn profile_bar(&mut self, ui: &mut egui::Ui) {
        let active = self.active_profile.clone();
        let active_label = active.clone().unwrap_or_else(|| "default".into());
        ui.horizontal(|ui| {
            let mut switch: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("profile_sel")
                .width(112.0)
                .selected_text(RichText::new(format!("💾 {active_label}")).size(11.5))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(active.is_none(), "default").clicked() && active.is_some() {
                        switch = Some(None);
                    }
                    for name in list_profiles() {
                        if name == "default" {
                            continue;
                        }
                        let is = active.as_deref() == Some(name.as_str());
                        if ui.selectable_label(is, &name).clicked() && !is {
                            switch = Some(Some(name.clone()));
                        }
                    }
                });
            if let Some(t) = switch {
                self.switch_profile(t);
            }
            ui.menu_button(RichText::new("＋").size(12.0), |ui| {
                if ui.button("New profile from current…").clicked() {
                    self.prof_new_open = true;
                    self.profile_draft.clear();
                    ui.close_menu();
                }
                if ui.button(format!("Clone '{active_label}'")).clicked() {
                    let clone = format!("{active_label} copy");
                    self.snapshot_profile(&clone);
                    ui.close_menu();
                }
                if active.is_some() && ui.button(format!("🗑 Delete '{active_label}'")).clicked() {
                    if let Some(dir) = profiles_dir() {
                        let _ = std::fs::remove_file(dir.join(format!("{}.json", safe_profile_name(&active_label))));
                    }
                    self.active_profile = None;
                    let mut cfg = config::load();
                    cfg.active_profile = None;
                    let _ = config::save(&cfg);
                    ui.close_menu();
                }
            });
        });
        if self.prof_new_open {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.profile_draft).hint_text("name…").desired_width(96.0));
                let ok = !self.profile_draft.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("✓")).clicked() {
                    let name = self.profile_draft.trim().to_string();
                    self.snapshot_profile(&name);
                    self.active_profile = Some(name.clone());
                    let mut cfg = config::load();
                    cfg.active_profile = Some(name);
                    let _ = config::save(&cfg);
                    self.prof_new_open = false;
                    self.profile_draft.clear();
                }
                if ui.button("✕").clicked() {
                    self.prof_new_open = false;
                }
            });
        }
    }

    /// Sub-items rendered in the sidebar under the ACTIVE tab: layers for
    /// Live/Heatmap/Peek, library categories for FX Studio.
    fn nav_children(&mut self, ui: &mut egui::Ui, tab: Tab) {
        let names: Vec<String> = (0..self.layer_count()).map(|n| self.layer_name(n)).collect();
        let active = self.active_layer;
        match tab {
            Tab::Live => {
                let has_layout = self.layout.is_some();
                let oryx = self.oryx_layer_count();
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    // Custom layers get a ★ marker (only meaningful with a layout).
                    let label = if has_layout && n >= oryx { format!("★ {name}") } else { name.clone() };
                    if sub_item(ui, self.view_layer == n, n == active, &label) {
                        self.view_layer = n;
                        self.follow = false;
                    }
                }
                // Authoring (add/rename/delete) lives in the Layers tab.
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    let mut f = self.follow;
                    if toggle(ui, &mut f).on_hover_text("view follows the keyboard's active layer").changed() {
                        self.follow = f;
                        if f {
                            self.view_layer = self.active_layer;
                        }
                    }
                    ui.label(RichText::new("follow board").size(11.5).color(pal::TEXT_DIM));
                });
                ui.add_space(2.0);
            }
            Tab::Heatmap => {
                if sub_item(ui, self.heat_layer.is_none(), false, "all layers") {
                    self.heat_layer = None;
                }
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    if sub_item(ui, self.heat_layer == Some(n), n == active, name) {
                        self.heat_layer = Some(n);
                    }
                }
            }
            Tab::Peek => {
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    if sub_item(ui, self.peek_layer == n, n == active, name) {
                        self.peek_layer = n;
                        let c = self.peek.clone();
                        self.arm_preview(&c, 2000);
                    }
                }
            }
            Tab::Fx => {
                for (lib, label) in [
                    (FxLib::Const, "constant"),
                    (FxLib::Press, "on press"),
                    (FxLib::Custom, "custom"),
                    (FxLib::Apply, "▶ board RGB"),
                ] {
                    if sub_item(ui, self.fx_lib == lib, false, label) {
                        self.fx_lib = lib;
                    }
                }
            }
            Tab::Layers | Tab::Perf | Tab::Auto | Tab::Tools => {}
        }
        ui.add_space(4.0);
    }

    fn save_custom_shortcuts(&self) {
        let mut cfg = config::load();
        cfg.custom_shortcuts = self.custom_shortcuts.clone();
        let _ = config::save(&cfg);
    }

    /// The Shortcuts tab: a searchable cheatsheet of ready-made shortcuts
    /// (category chips in the panel) plus the user's own entries.
    fn ui_shortcuts(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("🔎").size(14.0));
            ui.add(egui::TextEdit::singleline(&mut self.keys_search).hint_text("search the cheatsheet…").desired_width(240.0));
            if !self.keys_search.is_empty() && ui.button("✕").clicked() {
                self.keys_search.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("＋ add shortcut").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                    self.keys_adding = true;
                    self.draft_sc.category = self.keys_cat.clone().unwrap_or_default();
                }
                if !self.hidden_shortcuts.is_empty()
                    && ui
                        .button(RichText::new(format!("↺ restore {} hidden", self.hidden_shortcuts.len())).size(11.5))
                        .clicked()
                {
                    self.hidden_shortcuts.clear();
                    let mut cfg = config::load();
                    cfg.hidden_shortcuts.clear();
                    let _ = config::save(&cfg);
                }
            });
        });
        ui.add_space(6.0);

        // Category chips (all + every category present).
        let mut cats: Vec<String> = Vec::new();
        for d in crate::shortcuts::builtin() {
            if !cats.contains(&d.category) {
                cats.push(d.category.clone());
            }
        }
        for c in &self.custom_shortcuts {
            if !cats.contains(&c.category) && !c.category.is_empty() {
                cats.push(c.category.clone());
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.keys_cat.is_none(), "all").clicked() {
                self.keys_cat = None;
            }
            for cat in &cats {
                if ui.selectable_label(self.keys_cat.as_deref() == Some(cat.as_str()), cat).clicked() {
                    self.keys_cat = Some(cat.clone());
                }
            }
        });
        ui.add_space(6.0);

        // Add/edit form.
        if self.keys_adding {
            card(ui, "New shortcut", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("category").size(11.5).color(pal::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.draft_sc.category).hint_text("e.g. Burp").desired_width(140.0));
                    ui.label(RichText::new("keys").size(11.5).color(pal::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.draft_sc.keys).hint_text("Cmd + Shift + X").desired_width(170.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("does").size(11.5).color(pal::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.draft_sc.desc).hint_text("what it does").desired_width(340.0));
                    ui.checkbox(&mut self.draft_sc.high, "essential");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let ok = !self.draft_sc.category.trim().is_empty() && !self.draft_sc.keys.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                        self.custom_shortcuts.push(self.draft_sc.clone());
                        self.save_custom_shortcuts();
                        self.keys_adding = false;
                        self.draft_sc = config::CustomShortcut { category: String::new(), keys: String::new(), desc: String::new(), high: true };
                    }
                    if ui.button("Cancel").clicked() {
                        self.keys_adding = false;
                    }
                });
            });
            ui.add_space(6.0);
        }

        // Collect the visible rows: (category, keys, desc, high, custom-index).
        let q = self.keys_search.to_lowercase();
        let cat = self.keys_cat.clone();
        let matches = |c: &str, k: &str, d: &str| {
            (cat.as_deref().is_none_or(|w| w == c))
                && (q.is_empty() || k.to_lowercase().contains(&q) || d.to_lowercase().contains(&q) || c.to_lowercase().contains(&q))
        };
        let mut rows: Vec<(String, String, String, bool, Option<usize>)> = Vec::new();
        for (i, c) in self.custom_shortcuts.iter().enumerate() {
            if matches(&c.category, &c.keys, &c.desc) {
                rows.push((c.category.clone(), c.keys.clone(), c.desc.clone(), c.high, Some(i)));
            }
        }
        for d in crate::shortcuts::builtin() {
            let id = format!("{}|{}|{}", d.category, d.keys, d.desc);
            if !self.hidden_shortcuts.contains(&id) && matches(&d.category, &d.keys, &d.desc) {
                rows.push((d.category.clone(), d.keys.clone(), d.desc.clone(), d.high, None));
            }
        }

        ui.label(RichText::new(format!("{} shortcuts", rows.len())).size(11.0).color(pal::TEXT_DIM));
        ui.add_space(4.0);

        let mut delete: Option<usize> = None;
        let mut hide: Option<String> = None;
        let mut last_cat = String::new();
        for (rcat, keys, desc, high, custom_i) in &rows {
            // Group header when browsing "all".
            if cat.is_none() && *rcat != last_cat {
                last_cat = rcat.clone();
                ui.add_space(8.0);
                ui.label(RichText::new(rcat.to_uppercase()).size(10.5).strong().color(pal::VIOLET_HI));
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                // Key chip.
                egui::Frame::new()
                    .fill(pal::INPUT)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(keys).monospace().size(12.0).color(pal::TEXT));
                    });
                ui.add_space(6.0);
                ui.label(RichText::new(desc).size(12.5).color(pal::TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match custom_i {
                        Some(i) => {
                            if ui.button(RichText::new("✕").size(10.0)).on_hover_text("delete your shortcut").clicked() {
                                delete = Some(*i);
                            }
                            ui.label(RichText::new("yours").size(10.0).color(pal::CYAN));
                        }
                        None => {
                            if ui.button(RichText::new("✕").size(10.0)).on_hover_text("hide this shortcut").clicked() {
                                hide = Some(format!("{rcat}|{keys}|{desc}"));
                            }
                        }
                    }
                    if *high {
                        ui.label(RichText::new("●").size(9.0).color(pal::VIOLET));
                    }
                });
            });
        }
        if let Some(i) = delete {
            self.custom_shortcuts.remove(i);
            self.save_custom_shortcuts();
        }
        if let Some(id) = hide {
            self.hidden_shortcuts.push(id);
            let mut cfg = config::load();
            cfg.hidden_shortcuts = self.hidden_shortcuts.clone();
            let _ = config::save(&cfg);
        }
        ui.add_space(10.0);
    }

    fn start_perf_observe(&mut self) {
        let now = Instant::now();
        self.perf_last = None;
        self.perf_run = Some(PerfRun {
            samples: Vec::new(),
            sampler: perf::CpuSampler::new(),
            started: now,
            next_sample: now + Duration::from_millis(1000),
            end_at: now + Duration::from_secs(300),
            phases: Vec::new(),
            phase_i: 0,
            phase_until: now,
            restore: None,
        });
    }

    fn start_perf_compare(&mut self) {
        let now = Instant::now();
        let phases = vec![
            PerfPhase { label: "idle".into(), secs: 12, anim: Effect::Off, peek: false },
            PerfPhase { label: "layout RGB".into(), secs: 12, anim: Effect::Layout, peek: false },
            PerfPhase { label: "rainbow".into(), secs: 12, anim: Effect::Rainbow, peek: false },
            PerfPhase { label: "rainbow+peek".into(), secs: 12, anim: Effect::Rainbow, peek: true },
        ];
        let restore = self.anim.lock().map(|a| a.effect).ok();
        // Apply the first phase immediately.
        self.set_anim_effect(phases[0].anim);
        self.perf_last = None;
        self.perf_run = Some(PerfRun {
            samples: Vec::new(),
            sampler: perf::CpuSampler::new(),
            started: now,
            next_sample: now + Duration::from_millis(1000),
            end_at: now,
            phase_until: now + Duration::from_secs(phases[0].secs),
            phase_i: 0,
            phases,
            restore,
        });
    }

    fn finish_perf(&mut self) {
        if let Some(run) = self.perf_run.take() {
            let secs = run.started.elapsed().as_secs();
            self.perf_last = Some(perf::summarize(&run.samples, secs));
            if let Some(eff) = run.restore {
                self.set_anim_effect(eff);
                self.peek_until = None;
            }
        }
    }

    fn tick_perf(&mut self) {
        // Live CPU read (cheap, sub-Hz).
        if self.perf_tick.elapsed() >= Duration::from_millis(600) {
            self.perf_live = self.perf_sampler.sample();
            self.perf_tick = Instant::now();
        }
        if self.perf_run.is_none() {
            return;
        }
        let now = Instant::now();

        // Decide phase transitions without holding a borrow across self calls.
        let (ended, advance_to) = {
            let run = self.perf_run.as_ref().unwrap();
            if run.phases.is_empty() {
                (now >= run.end_at, None)
            } else if now >= run.phase_until {
                let next = run.phase_i + 1;
                if next >= run.phases.len() {
                    (true, None)
                } else {
                    (false, Some(next))
                }
            } else {
                (false, None)
            }
        };

        if ended {
            self.finish_perf();
            return;
        }

        if let Some(next) = advance_to {
            let ph = self.perf_run.as_ref().unwrap().phases[next].clone();
            self.set_anim_effect(ph.anim);
            if ph.peek {
                self.peek_layer = self.active_layer.max(1);
                self.peek_until = Some(now + Duration::from_secs(ph.secs + 1));
            } else {
                self.peek_until = None;
            }
            let run = self.perf_run.as_mut().unwrap();
            run.phase_i = next;
            run.phase_until = now + Duration::from_secs(ph.secs);
        }

        // Take a sample once per second.
        if self.perf_run.as_ref().unwrap().next_sample <= now {
            let cpu = self.perf_run.as_mut().unwrap().sampler.sample();
            let label = {
                let run = self.perf_run.as_ref().unwrap();
                if run.phases.is_empty() {
                    self.perf_state().label()
                } else {
                    run.phases[run.phase_i].label.clone()
                }
            };
            let run = self.perf_run.as_mut().unwrap();
            run.samples.push((label, cpu));
            run.next_sample = now + Duration::from_millis(1000);
        }
    }

    /// Board size in key units for layout math: width = max key x + 1; height =
    /// max key y + 1.6 (the extra 0.6 is the thumb cluster's downward swing).
    fn board_units(&self) -> (f32, f32) {
        let g = self.geometry();
        (
            g.keys.iter().map(|k| k.x).fold(0.0f32, f32::max) + 1.0,
            g.keys.iter().map(|k| k.y).fold(0.0f32, f32::max) + 1.6,
        )
    }

    /// A layer's effective glow as raw RGB triples (unlit keys → black) - the
    /// form the LED thread and previews consume.
    fn glow_rgb(&self, layer: u8) -> Vec<[u8; 3]> {
        self.glow_colors(layer)
            .into_iter()
            .map(|c| c.map(|c| [c.r(), c.g(), c.b()]).unwrap_or([0, 0, 0]))
            .collect()
    }

    /// Feed the LED thread the current layer's colors (base for effects).
    fn push_anim_base(&mut self) {
        let base = self.glow_rgb(self.active_layer);
        if let Ok(mut a) = self.anim.lock() {
            a.base = base;
        }
    }

    /// Keys whose working color differs from the saved snapshot.
    fn unsaved_count(&self) -> usize {
        let mut keys: std::collections::HashSet<(u8, usize)> =
            self.glow_work.keys().copied().collect();
        keys.extend(self.glow_saved.keys().copied());
        keys.into_iter()
            .filter(|k| self.glow_work.get(k) != self.glow_saved.get(k))
            .count()
    }

    fn save_glow(&mut self) {
        let Some(hash) = self.layout_hash.clone() else { return };
        let mut cfg = config::load();
        cfg.glow_overrides.retain(|o| o.layout != hash);
        for (&(layer, key), &rgb) in &self.glow_work {
            cfg.glow_overrides.push(GlowOverride { layout: hash.clone(), layer, key: key as u16, rgb });
        }
        if config::save(&cfg).is_ok() {
            self.glow_saved = self.glow_work.clone();
        }
    }

    fn discard_glow(&mut self) {
        self.glow_work = self.glow_saved.clone();
        if self.sync_glow {
            self.needs_push = true;
        }
    }

    /// Push the active layer's effective colors to the physical keyboard.
    /// Primed with one SetRgbLedAll of the dominant color (fills the whole LED
    /// array instantly and auto-enables control - no black flash), then only
    /// the differing keys.
    fn push_glow(&self) {
        let colors = self.glow_rgb(self.active_layer);
        let mut dominant = [0u8, 0, 0];
        let mut best = 0;
        for c in &colors {
            let n = colors.iter().filter(|x| *x == c).count();
            if n > best {
                best = n;
                dominant = *c;
            }
        }
        let _ = self.cmd_tx.send(KbCmd::SetRgbLedAll {
            r: dominant[0],
            g: dominant[1],
            b: dominant[2],
        });
        for (i, c) in colors.iter().enumerate() {
            if *c != dominant {
                // LED chain follows Oryx visual order.
                let _ = self.cmd_tx.send(KbCmd::SetRgbLed { led: i as u8, r: c[0], g: c[1], b: c[2] });
            }
        }
    }

    fn drain_events(&mut self) {
        let key_count = self.geometry().len();
        while let Ok(ev) = self.erx.try_recv() {
            match ev {
                DevEvent::Connected { model, serial } => {
                    if let Ok(id) = LayoutId::from_serial(&serial) {
                        self.heat = HeatmapStore::load(&id.hash, key_count).ok();
                        self.hydrate_glow(&id.hash);
                        self.hydrate_key_fx(&id.hash);
                        self.hydrate_custom_layers(&id.hash);
                        self.hydrate_staged(&id.hash);
                        // Remember it so the Live view can show this layout from
                        // cache next time, before any keyboard is plugged in.
                        let mut cfg = config::load();
                        if cfg.last_layout.as_deref() != Some(serial.as_str()) {
                            cfg.last_layout = Some(serial.clone());
                            let _ = config::save(&cfg);
                        }
                    }
                    self.connected = Some((model, serial));
                    self.push_anim_base();
                }
                DevEvent::LayoutLoaded(layout) => {
                    self.layout = Some(*layout);
                    // Oryx layer count is known now - re-place custom layers.
                    if let Some(hash) = self.layout_hash.clone() {
                        self.hydrate_custom_layers(&hash);
                    }
                    self.push_anim_base();
                }
                DevEvent::Disconnected => {
                    self.connected = None;
                    self.pressed.iter_mut().for_each(|p| *p = false);
                    if let Some(h) = &mut self.heat {
                        let _ = h.save();
                    }
                }
                DevEvent::Hid(Event::Layer(n)) => {
                    let changed = self.active_layer != n;
                    self.active_layer = n;
                    if self.follow {
                        self.view_layer = n;
                    }
                    if self.sync_glow {
                        self.needs_push = true; // physical board now shows another layer
                    }
                    if changed {
                        self.maybe_peek(n);
                        self.push_anim_base();
                    }
                }
                DevEvent::Hid(Event::KeyDown { col, row }) => {
                    // Peek shortcut: bind mode collects the whole combo (all
                    // keys held before the first release); otherwise holding
                    // the full chord shows the minimap.
                    if self.binding_overlay {
                        if !self.binding_draft.contains(&[row, col]) {
                            self.binding_draft.push([row, col]);
                        }
                    } else if !self.overlay_chord.is_empty() && self.overlay_chord.contains(&[row, col]) {
                        let all_down = self.overlay_chord.iter().all(|&[r, c]| {
                            (r == row && c == col)
                                || self
                                    .geometry()
                                    .key_index(r, c)
                                    .is_some_and(|k| self.pressed.get(k).copied().unwrap_or(false))
                        });
                        if all_down {
                            self.peek_layer = self.active_layer;
                            self.peek_until = Some(Instant::now() + Duration::from_secs(3600));
                        }
                    }
                    if let Some(idx) = self.geometry().key_index(row, col) {
                        self.pressed[idx] = true;
                        self.combo_down.insert(idx, Instant::now());
                        // Keystroke HUD: surface the minimap the moment a key
                        // goes down (independent of only-non-base).
                        if self.peek.show_combo && self.peek.enabled {
                            self.peek_layer = self.active_layer;
                            self.peek_until = Some(Instant::now() + Duration::from_millis(1600));
                        }
                        if let Some(h) = &mut self.heat {
                            h.record(self.active_layer, idx, key_count);
                            let _ = h.autosave();
                        }
                        // Pressing a key selects it for the config panel below.
                        if self.selected_key != Some(idx) {
                            self.selected_key = Some(idx);
                            self.edit_color = self.current_key_srgb(self.view_layer, idx);
                            self.sync_editor_from_key(self.view_layer, idx);
                        }
                        // Resolve which effect this press fires (per-key first,
                        // global fallback) and queue it for the LED thread.
                        self.fire_key_fx(idx);
                    }
                }
                DevEvent::Hid(Event::KeyUp { col, row }) => {
                    // First release while binding commits the collected combo.
                    if self.binding_overlay && !self.binding_draft.is_empty() {
                        self.binding_overlay = false;
                        self.overlay_chord = std::mem::take(&mut self.binding_draft);
                        let mut cfg = config::load();
                        cfg.overlay_chord = self.overlay_chord.clone();
                        cfg.overlay_trigger = self.overlay_chord.first().copied();
                        let _ = config::save(&cfg);
                    } else if self.overlay_chord.contains(&[row, col]) {
                        self.peek_until = Some(Instant::now());
                    }
                    if let Some(idx) = self.geometry().key_index(row, col) {
                        self.pressed[idx] = false;
                        self.record_combo(idx);
                    }
                }
                DevEvent::Hid(_) => {}
            }
        }
        if let Some(rx) = &self.flash_rx {
            while let Ok(s) = rx.try_recv() {
                self.flash_state = Some(s);
            }
            // Drive the build modal's phase/progress from the flash stage.
            match &self.flash_state {
                Some(FlashState::Downloading) => {
                    self.build_phase = "Downloading firmware…".into();
                    self.build_progress = self.build_progress.max(0.97);
                }
                Some(FlashState::WaitingForBootloader) => {
                    self.build_phase = "Press the Voyager's reset button…".into();
                    self.build_progress = self.build_progress.max(0.97);
                }
                Some(FlashState::Working { phase, fraction }) => {
                    self.build_phase = format!("Flashing: {phase}");
                    self.build_progress = 0.97 + 0.03 * fraction.clamp(0.0, 1.0);
                }
                Some(FlashState::Done) => {
                    self.build_busy = false;
                    self.build_phase = "Flashed ✓".into();
                    self.build_progress = 1.0;
                    self.build_result = Some(Ok("Firmware flashed - the keyboard will reconnect.".into()));
                }
                Some(FlashState::Failed(e)) => {
                    self.build_busy = false;
                    self.build_phase = "Flash failed".into();
                    self.build_result = Some(Err(e.clone()));
                }
                None => {}
            }
        }
        if let Some(rx) = &self.build_rx {
            let mut msgs = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
            for msg in msgs {
                match msg {
                    BuildMsg::Log(line) => {
                        self.note_build_phase(&line);
                        self.build_log.push_str(&line);
                        self.build_log.push('\n');
                    }
                    BuildMsg::Built(bin) => {
                        self.last_build_bin = Some(bin.clone());
                        if self.build_flash_after {
                            self.build_phase = "Compiled - flashing…".into();
                            self.build_progress = 0.97;
                            self.build_log.push_str("✓ compiled - flashing…\n");
                            self.flash_state = None;
                            self.flash_cancel = Arc::new(AtomicBool::new(false));
                            self.flash_rx = Some(worker::spawn_flash(
                                Some(bin.to_string_lossy().into_owned()),
                                false,
                                self.flash_cancel.clone(),
                                self.egui_ctx.clone(),
                            ));
                        } else {
                            self.build_busy = false;
                            self.build_phase = "Done".into();
                            self.build_progress = 1.0;
                            self.build_result = Some(Ok(format!("Built {}", bin.display())));
                            self.build_log.push_str(&format!("✓ built: {}\n", bin.display()));
                        }
                    }
                    BuildMsg::Failed(e) => {
                        self.build_log.push_str(&format!("✗ {e}\n"));
                        self.build_busy = false;
                        self.build_phase = "Failed".into();
                        self.build_result = Some(Err(e));
                    }
                }
            }
        }
    }

    /// Update the build phase + progress estimate from a streamed log line.
    fn note_build_phase(&mut self, line: &str) {
        let (phase, prog): (&str, f32) = if line.contains("Fetching generated source") {
            ("Fetching layout source…", 0.08)
        } else if line.contains("Applying") || line.contains("Adding layer") || line.contains("Generating") || line.starts_with("Enabled") {
            ("Patching firmware source…", 0.20)
        } else if line.contains("Compiling with qmk") {
            ("Compiling firmware…", 0.30)
        } else if line.contains("Compiling:") || line.contains("Compiling ") {
            self.build_compiles += 1;
            // Asymptotic ramp across the compile band (0.30 → 0.90).
            let c = self.build_compiles as f32;
            ("Compiling firmware…", 0.30 + 0.60 * (c / (c + 40.0)))
        } else if line.contains("Linking") {
            ("Linking…", 0.93)
        } else if line.contains("Creating") || line.contains("Copying") {
            ("Finishing…", 0.96)
        } else {
            return;
        };
        self.build_phase = phase.to_string();
        self.build_progress = self.build_progress.max(prog);
    }

    fn reconcile_background_jobs(&mut self, ctx: &egui::Context) {
        // Guard: seize while enabled AND a keyboard is connected.
        #[cfg(target_os = "macos")]
        {
            let want = self.guard_enabled && self.connected.is_some();
            if want && self.guard.is_none() {
                match crate::macos_kb::ensure_input_monitoring()
                    .and_then(|()| crate::macos_kb::seize_builtin())
                {
                    Ok(g) => {
                        self.guard_error = None;
                        self.guard = Some(g);
                    }
                    Err(e) => {
                        self.guard_error = Some(format!("{e:#}"));
                        self.guard_enabled = false;
                    }
                }
            } else if !want && self.guard.is_some() {
                self.guard = None;
            }
        }

        // Glow: re-push to the physical keyboard when something changed - but
        // not while an RGB animation owns the LEDs (they'd fight).
        let anim_active = self
            .anim
            .lock()
            .map(|a| a.effect != Effect::Off || !a.events.is_empty())
            .unwrap_or(false);
        if self.sync_glow && !anim_active && self.connected.is_some() && self.needs_push {
            self.push_glow();
            self.needs_push = false;
        }

        // Autolayer watcher lifecycle.
        if self.autolayer_enabled && self.autolayer.is_none() {
            #[cfg(target_os = "macos")]
            {
                self.autolayer = Some(worker::spawn_autolayer(
                    self.rules.clone(),
                    self.cmd_tx.clone(),
                    ctx.clone(),
                ));
            }
        } else if !self.autolayer_enabled && self.autolayer.is_some() {
            self.autolayer = None;
        }
    }
}

impl eframe::App for App {
    /// Clear to fully transparent so the peek viewport's low-alpha content
    /// shows the desktop through it. The main window stays opaque because its
    /// panels paint solid fills over every pixel.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Clean shutdown: hand the LEDs back to the firmware and restore the
    /// built-in keyboard, before threads are torn down by process exit. The
    /// anim thread's own release is racy at exit; this makes it reliable.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.cmd_tx.send(KbCmd::RgbRelease);
        self.guard = None; // Drop restores the built-in keyboard
        #[cfg(target_os = "macos")]
        crate::macos_kb::force_restore_if_active();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.reconcile_background_jobs(ctx);
        self.tick_perf();
        if let Some(rx) = &self.update_rx {
            if let Ok(r) = rx.try_recv() {
                self.update_state = Some(r);
                self.update_rx = None;
            }
        }
        // Combo HUD: keep the minimap up while any key is physically held, so
        // long holds don\'t vanish before release.
        if self.peek.show_combo && self.peek.enabled && self.pressed.iter().any(|&p| p) {
            self.peek_layer = self.active_layer;
            self.peek_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        // A "test on keyboard" run of a custom sequence auto-reverts.
        if let Some((until, prev)) = self.fx_board_restore {
            if Instant::now() >= until {
                self.set_anim_effect(prev);
                self.fx_board_restore = None;
            }
        }
        // Idle poll to drain channels; cheap now that per-frame work is cached.
        ctx.request_repaint_after(Duration::from_millis(250));
        // Refresh the monitor list occasionally (cheap, but not per frame).
        if self.monitors_checked.elapsed() > Duration::from_secs(2) {
            self.monitors_cache = fetch_monitors();
            self.monitors_checked = Instant::now();
        }

        // Navigation lives in a left sidebar (not a top header): vertical space
        // is the scarce direction - this keeps keyboard + inspector fully
        // visible without scrolling. The active tab expands its sub-items
        // (layers, heat scope, FX categories) directly beneath it.
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(178.0)
            .frame(egui::Frame::new().fill(pal::SURFACE).stroke(egui::Stroke::new(1.0, pal::BORDER)).inner_margin(egui::Margin::symmetric(12, 12)))
            .show(ctx, |ui| {
                ui.label(RichText::new("Keyjitsu").strong().size(19.0).color(pal::VIOLET));
                ui.label(
                    RichText::new(format!("Voyager keyboard mapper v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.0)
                        .color(pal::TEXT_DIM),
                );
                ui.add_space(6.0);
                self.connection_pill(ui);
                ui.add_space(6.0);
                self.profile_bar(ui);
                ui.add_space(10.0);
                let nav_h = ui.available_height() - 34.0; // keep room for the CPU pill
                egui::ScrollArea::vertical().max_height(nav_h).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.spacing_mut().interact_size.y = 20.0;
                    for (tab, name, icon) in [
                        (Tab::Live, "Live", "⌨"),
                        (Tab::Layers, "Layers", "▤"),
                        (Tab::Heatmap, "Heatmap", "🔥"),
                        (Tab::Peek, "Peek", "👁"),
                        (Tab::Fx, "FX Studio (exp)", "✨"),
                        (Tab::Perf, "Performance (exp)", "📈"),
                        (Tab::Auto, "Autolayer", "⇆"),
                        (Tab::Tools, "Settings", "⚙"),
                    ] {
                        nav_item(ui, &mut self.tab, tab, icon, name);
                        if self.tab == tab {
                            self.nav_children(ui, tab);
                        }
                    }
                });
                // Status chips pinned to the bottom of the sidebar: things
                // that are "on" right now, plus an update notice.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(4.0);
                    if let Some(UpdateCheck::Available { tag, .. }) = &self.update_state {
                        let tag = tag.clone();
                        let resp = egui::Frame::new()
                            .show(ui, |ui| status_pill(ui, &format!("⬆ {tag} available"), pal::AMBER))
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("A newer keyjitsu is out. Open Settings for the release link.");
                        if resp.clicked() {
                            self.tab = Tab::Tools;
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        #[cfg(target_os = "macos")]
                        if self.guard.is_some() {
                            egui::Frame::new()
                                .show(ui, |ui| status_pill(ui, "🔒 guard", pal::GREEN))
                                .response
                                .on_hover_text("The built-in keyboard is disabled while the Voyager is connected.");
                        }
                        if self.autolayer_enabled {
                            egui::Frame::new()
                                .show(ui, |ui| status_pill(ui, "⇆ autolayer", pal::GREEN))
                                .response
                                .on_hover_text("Layers follow the frontmost app.");
                        }
                    });
                    if self.show_cpu_header {
                        let c = self.perf_live;
                        let resp = egui::Frame::new()
                            .show(ui, |ui| status_pill(ui, &format!("{c:.1}% CPU"), if c > 25.0 { pal::AMBER } else { pal::TEXT_DIM }))
                            .response;
                        resp.on_hover_text("keyjitsu\u{2019}s own CPU · % of one core (not system load)");
                    }
                });
            });

        // Bottom key-config panel (inspector), only on the Live tab.
        if self.tab == Tab::Live {
            // Deterministic height per state (egui panels otherwise keep their
            // first-frame size): compact hint bar when nothing is selected, a
            // capped editor when a key is - the canvas above shrinks to match,
            // so the whole editor is visible without scrolling.
            // Height = what the content actually needs: one compact header
            // row + the visible slot rows (+ a tap-dance note when present).
            let cap = (ctx.screen_rect().height() * 0.44).clamp(170.0, 300.0);
            let h = if self.selected_key.is_some() {
                let rows = 1 + (1..4)
                    .filter(|&sl| self.edit_slots[sl].is_some() || self.slot_added[sl])
                    .count();
                let warn = if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() { 18.0 } else { 0.0 };
                (148.0 + rows as f32 * 34.0 + warn).min(cap)
            } else {
                46.0
            };
            egui::TopBottomPanel::bottom("keycfg")
                .resizable(false)
                .exact_height(h)
                .frame(
                    egui::Frame::new()
                        .fill(pal::SURFACE)
                        .stroke(egui::Stroke::new(1.0, pal::BORDER))
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show(ui, |ui| self.ui_key_panel(ui));
                });
        }

        // One consistent panel colour across tabs. The keyboard sits on its own
        // raised card (lighter + a soft shadow), so it still reads as the focus
        // without a darker background band next to the sidebar.
        let canvas = egui::Frame::new().fill(pal::SURFACE);
        egui::CentralPanel::default().frame(canvas).show(ctx, |ui| {
            {
                match self.tab {
                    // The board is sized to the real remaining height (with a
                    // legibility floor), so keyboard + inspector share the
                    // window; the scroll only matters at extreme sizes.
                    Tab::Live => {
                        let h = ui.available_height();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.ui_live(ui, h));
                    }
                    Tab::Layers => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_layers(ui));
                    }
                    Tab::Heatmap => {
                        let h = ui.available_height();
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_heatmap(ui, h));
                    }
                    Tab::Peek => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_peek_page(ui));
                    }
                    Tab::Fx => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_fx_studio(ui));
                    }
                    Tab::Perf => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_perf_page(ui));
                    }
                    Tab::Auto => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_auto_page(ui));
                    }
                    Tab::Tools => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.ui_tools(ui));
                    }
                }
            }
        });

        self.ui_picker(ctx);
        self.ui_build_modal(ctx);

        // Layer-peek HUD: show while its timer is live, then let it close.
        if let Some(until) = self.peek_until {
            let now = Instant::now();
            if now < until {
                self.show_peek(ctx);
                ctx.request_repaint_after(until - now);
            } else {
                self.peek_until = None;
            }
        }
    }
}

/// Render a wrapped grid of keycode buttons; sets `pick` to the chosen code.
/// For templated (layer) entries, `{n}` is replaced with `layer_arg`.
fn keycode_grid(
    ui: &mut egui::Ui,
    keys: &[keycodes::KeyDef],
    templated: bool,
    layer_arg: u8,
    layer_name: &str,
    pick: &mut Option<String>,
) {
    ui.horizontal_wrapped(|ui| {
        for k in keys {
            let code = if templated {
                k.code.replace("{n}", &layer_arg.to_string())
            } else {
                k.code.to_string()
            };
            // Labels read by layer NAME, not number: "Momentary L{n}" →
            // "Momentary VimLife". The raw code shows on hover.
            let label = if templated {
                k.label.replace("L{n}", layer_name).replace("{n}", layer_name)
            } else {
                k.label.to_string()
            };
            let btn = egui::Button::new(label).min_size(egui::vec2(58.0, 26.0));
            if ui.add(btn).on_hover_text(&code).clicked() {
                *pick = Some(code);
            }
        }
    });
}

/// Platform-agnostic monitor info for the peek's monitor picker.
#[derive(Clone)]
struct MonitorInfo {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    name: Option<String>,
}

impl MonitorInfo {
    fn label(&self, index: usize) -> String {
        match &self.name {
            Some(n) => format!("{n} · {}×{}", self.w as i32, self.h as i32),
            None => format!("Monitor {} · {}×{}", index + 1, self.w as i32, self.h as i32),
        }
    }
}

fn fetch_monitors() -> Vec<MonitorInfo> {
    #[cfg(target_os = "macos")]
    {
        crate::macos_display::monitors()
            .into_iter()
            .map(|m| MonitorInfo { x: m.x, y: m.y, w: m.w, h: m.h, name: m.name })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// A small colored status pill (e.g. "Ready", "Off", "4.2% CPU").
fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(egui::CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).strong().color(color));
        });
}

/// A settings-dashboard card: icon + title + status pill on one line, muted
/// description, then the body.
fn tool_card(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    desc: &str,
    pill: Option<(String, egui::Color32)>,
    app: &mut App,
    body: impl FnOnce(&mut egui::Ui, &mut App),
) {
    egui::Frame::new()
        .fill(pal::CARD)
        .stroke(egui::Stroke::new(1.0, pal::BORDER))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(17.0));
                ui.label(RichText::new(title).strong().size(16.0).color(pal::TEXT));
                if let Some((t, c)) = pill {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        status_pill(ui, &t, c);
                    });
                }
            });
            if !desc.is_empty() {
                ui.label(RichText::new(desc).size(12.5).color(pal::TEXT_DIM));
            }
            ui.add_space(10.0);
            body(ui, app);
        });
    ui.add_space(14.0);
}

/// A sidebar sub-item (indented under the active tab). Returns clicked.
/// `dot` marks the layer the keyboard is physically on.
fn sub_item(ui: &mut egui::Ui, selected: bool, dot: bool, label: &str) -> bool {
    let (fill, text) = if selected {
        (pal::VIOLET.gamma_multiply(0.35), pal::TEXT)
    } else {
        (egui::Color32::TRANSPARENT, pal::TEXT_DIM)
    };
    let label = if dot { format!("● {label}") } else { label.to_string() };
    nav_row(ui, 20.0, 16.0, 6.0, fill, text, 11.5, &label, None).clicked()
}

/// One sidebar row, painted by hand so the label is always left-aligned
/// (egui buttons centre their text, which made indented sub-items look
/// ragged) and an optional small badge can sit at the right edge.
#[allow(clippy::too_many_arguments)]
fn nav_row(
    ui: &mut egui::Ui,
    height: f32,
    indent: f32,
    radius: f32,
    fill: egui::Color32,
    color: egui::Color32,
    size: f32,
    label: &str,
    badge: Option<&str>,
) -> egui::Response {
    let (full, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let rect = egui::Rect::from_min_max(full.min + egui::vec2(indent, 0.0), full.max);
    let fill = if fill == egui::Color32::TRANSPARENT && resp.hovered() {
        pal::HOVER.gamma_multiply(0.55)
    } else {
        fill
    };
    let p = ui.painter();
    if fill != egui::Color32::TRANSPARENT {
        p.rect_filled(rect, radius, fill);
    }
    p.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(size),
        color,
    );
    if let Some(b) = badge {
        let galley = p.layout_no_wrap(b.to_string(), egui::FontId::proportional(9.5), pal::TEXT_DIM);
        let pad = egui::vec2(5.0, 2.0);
        let bsize = galley.size() + pad * 2.0;
        let brect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 8.0 - bsize.x, rect.center().y - bsize.y / 2.0),
            bsize,
        );
        p.rect_filled(brect, 4.0, pal::INPUT);
        p.galley(brect.min + pad, galley, pal::TEXT_DIM);
    }
    resp
}

/// A sidebar navigation row: full-width, filled violet when active.
fn nav_item(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, icon: &str, name: &str) {
    let active = *current == tab;
    // "(exp)" in the name becomes a small badge instead of cluttering the label.
    let (name, badge) = match name.strip_suffix(" (exp)") {
        Some(n) => (n, Some("exp")),
        None => (name, None),
    };
    let (fill, text) = if active {
        (pal::VIOLET, egui::Color32::WHITE)
    } else {
        (egui::Color32::TRANSPARENT, pal::TEXT_MUTED)
    };
    if nav_row(ui, 30.0, 0.0, 8.0, fill, text, 13.5, &format!("{icon}  {name}"), badge).clicked() {
        *current = tab;
    }
    ui.add_space(1.0);
}

/// A titled card grouping settings (no App needed, unlike `section`).
fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(pal::CARD)
        .stroke(egui::Stroke::new(1.0, pal::BORDER))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(14.0).color(pal::VIOLET_HI));
            ui.add_space(8.0);
            body(ui);
        });
    ui.add_space(12.0);
}

/// A form row: a fixed-width label on the left, the control on the right.
fn labeled(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, ui.spacing().interact_size.y],
            egui::Label::new(egui::RichText::new(label).color(pal::TEXT_MUTED)).halign(egui::Align::LEFT),
        );
        add(ui);
    });
}

/// A group heading: bold light title + optional muted description.
fn group_header(ui: &mut egui::Ui, title: &str, desc: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).size(15.0).strong().color(pal::TEXT));
    if !desc.is_empty() {
        ui.label(RichText::new(desc).size(12.0).color(pal::TEXT_DIM));
    }
    ui.add_space(6.0);
}

/// An iOS-style toggle switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let radius = rect.height() * 0.5;
    let bg = pal::INPUT.lerp_to_gamma(pal::VIOLET, t);
    ui.painter().rect_filled(rect, egui::CornerRadius::same(radius as u8), bg);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(radius as u8),
        egui::Stroke::new(1.0, if *on { pal::VIOLET } else { pal::BORDER }),
        egui::StrokeKind::Inside,
    );
    let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter().circle_filled(egui::pos2(cx, rect.center().y), radius * 0.72, egui::Color32::WHITE);
    resp
}

/// A labeled toggle row: label left, switch pushed right. Returns changed.
fn toggle_row(ui: &mut egui::Ui, label: &str, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(pal::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = toggle(ui, on).changed();
        });
    });
    changed
}

/// A checkerboard (transparency indicator) behind the peek preview.
fn draw_checkerboard(painter: &egui::Painter, rect: egui::Rect) {
    let s = 11.0;
    let (a, b) = (egui::Color32::from_rgb(40, 42, 50), egui::Color32::from_rgb(28, 30, 37));
    painter.rect_filled(rect, egui::CornerRadius::same(8), b);
    let cols = (rect.width() / s).ceil() as i32;
    let rows = (rect.height() / s).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 0 {
                let x = rect.left() + c as f32 * s;
                let y = rect.top() + r as f32 * s;
                let cell =
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(s, s)).intersect(rect);
                painter.rect_filled(cell, egui::CornerRadius::ZERO, a);
            }
        }
    }
}

/// A 3×3 anchor grid (corners, edges, center) for the peek position.
fn position_grid(ui: &mut egui::Ui, valign: &mut VAlign, halign: &mut HAlign) {
    let rows = [
        (VAlign::Top, [("↖", HAlign::Left), ("↑", HAlign::Center), ("↗", HAlign::Right)]),
        (VAlign::Middle, [("←", HAlign::Left), ("•", HAlign::Center), ("→", HAlign::Right)]),
        (VAlign::Bottom, [("↙", HAlign::Left), ("↓", HAlign::Center), ("↘", HAlign::Right)]),
    ];
    egui::Frame::new()
        .fill(pal::INPUT)
        .stroke(egui::Stroke::new(1.0, pal::BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            egui::Grid::new("posgrid").spacing([5.0, 5.0]).show(ui, |ui| {
                for (v, cells) in rows {
                    for (glyph, h) in cells {
                        let selected = *valign == v && *halign == h;
                        let (fill, fg) = if selected {
                            (pal::VIOLET, Color32::WHITE)
                        } else {
                            (pal::CARD, pal::TEXT_MUTED)
                        };
                        let resp = egui::Frame::new()
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(6))
                            .show(ui, |ui| {
                                ui.add_sized(
                                    [38.0, 34.0],
                                    egui::Label::new(RichText::new(glyph).size(17.0).color(fg))
                                        .selectable(false),
                                );
                            })
                            .response
                            .interact(egui::Sense::click());
                        if resp.clicked() {
                            *valign = v;
                            *halign = h;
                        }
                    }
                    ui.end_row();
                }
            });
        });
}

/// Reveal a path in Finder (macOS) - `open` selects/creates the folder view.
fn reveal_in_finder(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    {
        // Open the folder itself if it exists, else its parent.
        let target = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("open").arg(target).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// 14207 → "14,207".
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A tiny horizontal bar (0..1) for the performance table.
fn perf_bar(ui: &mut egui::Ui, frac: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(70.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, egui::CornerRadius::same(3), pal::INPUT);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, egui::CornerRadius::same(3), color);
}

/// LaunchAgent plist path for autostart at login.
fn autostart_plist() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Library/LaunchAgents/com.keyjitsu.gui.plist"))
}

/// Directory holding saved profile snapshots.
fn profiles_dir() -> Option<std::path::PathBuf> {
    crate::oryx_api::cache_dir().ok().map(|d| d.join("profiles"))
}

/// Whether a fresh tap of `key` should merge into the previous log entry as a
/// double-tap: same key, still a single, within the press-to-press window
/// (`gap` = ms between the two DOWN presses, like a double-click).
fn combo_merges(prev: Option<(usize, u8, u128)>, key: usize) -> bool {
    const DOUBLE_MS: u128 = 500;
    matches!(prev, Some((k, 1, gap)) if k == key && gap < DOUBLE_MS)
}

/// Draw a horizontal strip of recent-press chips (combo HUD). `a` is the
/// overlay alpha (0..1). Each entry: key label + gesture suffix
/// (⇩ hold, ×2 double-tap, ×2⇩ double-tap-hold).
fn combo_strip(ui: &mut egui::Ui, entries: &[ComboChip], a: f32, accent: Color32, show_ms: bool) {
    let alpha = (a * 255.0) as u8;
    let ink = Color32::from_rgba_unmultiplied(235, 236, 242, alpha);
    let dim = Color32::from_rgba_unmultiplied(150, 152, 165, alpha);
    ui.horizontal(|ui| {
        if entries.is_empty() {
            ui.label(RichText::new("press a key…").size(12.0).color(dim));
            return;
        }
        for c in entries {
            // A live "holding" chip glows in the accent; finalized holds get
            // an accent border; plain taps a neutral border.
            let (fill, border) = if c.live {
                (Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), (alpha as f32 * 0.35) as u8),
                 Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha))
            } else if c.held {
                (Color32::from_rgba_unmultiplied(30, 32, 42, alpha),
                 Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha))
            } else {
                (Color32::from_rgba_unmultiplied(30, 32, 42, alpha),
                 Color32::from_rgba_unmultiplied(90, 94, 112, alpha))
            };
            egui::Frame::new()
                .fill(fill)
                .stroke(egui::Stroke::new(if c.held { 1.6 } else { 1.0 }, border))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(7, 3))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    ui.label(RichText::new(&c.label).strong().size(13.0).color(ink));
                    if c.count == 2 {
                        ui.label(RichText::new("×2").size(11.0).strong().color(accent));
                        if show_ms && c.gap_ms > 0 {
                            ui.label(RichText::new(format!("Δ{}ms", c.gap_ms)).size(10.0).color(dim));
                        }
                    }
                    if c.held {
                        ui.label(RichText::new(if c.live { "hold" } else { "⇩" }).size(11.0).strong().color(accent));
                    }
                    // Measurement readout: the press/hold duration in ms.
                    if show_ms {
                        ui.label(RichText::new(format!("{}ms", c.ms)).size(10.0).color(if c.held { accent } else { dim }));
                    }
                });
        }
    });
}

/// Rewrite the target layer in a layer-switch keycode after layer `del` was
/// removed: a reference to a layer ABOVE `del` shifts down by one; references
/// to `del` or below (and non-layer codes) are unchanged.
fn renumber_layer_ref(code: &str, del: u8) -> String {
    let shift = |n: &str| -> String {
        match n.trim().parse::<u8>() {
            Ok(v) if v > del => (v - 1).to_string(),
            _ => n.trim().to_string(),
        }
    };
    for fam in ["MO", "TO", "TG", "TT", "OSL", "DF"] {
        if let Some(rest) = code.strip_prefix(fam).and_then(|r| r.strip_prefix('(')).and_then(|r| r.strip_suffix(')')) {
            return format!("{fam}({})", shift(rest));
        }
    }
    if let Some(rest) = code.strip_prefix("LT(").and_then(|r| r.strip_suffix(')')) {
        if let Some((n, tap)) = rest.split_once(',') {
            return format!("LT({},{})", shift(n), tap.trim());
        }
    }
    code.to_string()
}

/// Turn a QMK keycode string into an `OryxKey` for display: layer-switch
/// families render as `CODE → layer` (via the layer field), everything else
/// as its plain legend. A dual-role `LT(n,tap)` shows the tap with a hold hint.
fn synth_key(code: &str) -> OryxKey {
    let mut k = OryxKey::default();
    for fam in ["MO", "TO", "TG", "TT", "OSL", "DF"] {
        if let Some(rest) = code.strip_prefix(fam).and_then(|r| r.strip_prefix('(')) {
            if let Some(inner) = rest.strip_suffix(')') {
                if let Ok(layer) = inner.trim().parse::<u8>() {
                    k.tap = Some(KeyAction { code: Some(fam.to_string()), layer: Some(layer), description: None });
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
                k.tap = Some(KeyAction { code: Some(tap.trim().to_string()), layer: None, description: None });
                k.hold = Some(KeyAction { code: Some("MO".into()), layer: Some(layer), description: None });
                return k;
            }
        }
    }
    k.tap = Some(KeyAction { code: Some(code.to_string()), layer: None, description: None });
    k
}

/// A profile name that is safe as a file stem (no separators/dots tricks).
fn safe_profile_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() { "default".into() } else { trimmed }
}

/// Sorted names of saved profiles.
fn list_profiles() -> Vec<String> {
    let Some(dir) = profiles_dir() else { return Vec::new() };
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().and_then(|x| x.to_str()) == Some("json"))
                        .then(|| p.file_stem().and_then(|x| x.to_str()).map(str::to_string))
                        .flatten()
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn autostart_enabled() -> bool {
    autostart_plist().is_some_and(|p| p.exists())
}

/// Enable/disable start-at-login via a per-user LaunchAgent (RunAtLoad).
fn set_autostart(on: bool) -> anyhow::Result<()> {
    let path = autostart_plist().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if on {
        let exe = std::env::current_exe()?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.keyjitsu.gui</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>gui</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict>
</plist>
"#,
            exe.display()
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, plist)?;
    } else if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Result of a manual "check for updates" against GitHub Releases.
#[derive(Debug, Clone)]
pub enum UpdateCheck {
    UpToDate,
    Available { tag: String, url: String },
    Error(String),
}

const RELEASES_API: &str = "https://api.github.com/repos/martinezooo/keyjitsu/releases/latest";

/// Ask GitHub for the newest release on a background thread. Only ever runs
/// when the user clicks the button: the app makes no network calls on its own
/// apart from the anonymous Oryx layout read.
fn spawn_update_check() -> std::sync::mpsc::Receiver<UpdateCheck> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<UpdateCheck, String> {
            let resp: serde_json::Value = ureq::get(RELEASES_API)
                .set("User-Agent", concat!("keyjitsu/", env!("CARGO_PKG_VERSION")))
                .set("Accept", "application/vnd.github+json")
                .timeout(Duration::from_secs(8))
                .call()
                .map_err(|e| e.to_string())?
                .into_json()
                .map_err(|e| e.to_string())?;
            let tag = resp
                .get("tag_name")
                .and_then(|t| t.as_str())
                .ok_or("no tag_name in the response")?
                .to_string();
            let url = resp
                .get("html_url")
                .and_then(|u| u.as_str())
                .unwrap_or("https://github.com/martinezooo/keyjitsu/releases")
                .to_string();
            Ok(if version_newer(&tag, env!("CARGO_PKG_VERSION")) {
                UpdateCheck::Available { tag, url }
            } else {
                UpdateCheck::UpToDate
            })
        })();
        let _ = tx.send(result.unwrap_or_else(UpdateCheck::Error));
    });
    rx
}

/// `true` if `latest` (e.g. "v0.9.2") is a newer semver than `current`.
/// Unparseable parts compare as 0, so a weird tag never reports an update.
fn version_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, p) in v.trim().trim_start_matches('v').split('.').take(3).enumerate() {
            out[i] = p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
        }
        out
    }
    parts(latest) > parts(current)
}

fn status_dot(ui: &mut egui::Ui, ok: bool) {
    let (c, s) = if ok {
        (pal::GREEN, "●")
    } else {
        (Color32::from_rgb(230, 150, 90), "○")
    };
    ui.colored_label(c, s);
}

impl App {
    /// Compact connection status as a colored pill.
    fn connection_pill(&self, ui: &mut egui::Ui) {
        let (dot, text) = match &self.connected {
            Some((model, serial)) => (
                pal::GREEN,
                match &self.layout {
                    Some(l) => format!("{model} · {}", l.title),
                    None => format!("{model} · {serial}"),
                },
            ),
            None => (pal::RED, "No keyboard".to_string()),
        };
        let hover = if self.connected.is_some() {
            text.clone()
        } else {
            "Plug in your Voyager and quit Keymapp (the HID channel is exclusive).".to_string()
        };
        egui::Frame::new()
            .fill(pal::RAISED)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(20))
            .inner_margin(egui::Margin::symmetric(11, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(dot, RichText::new("●").size(11.0));
                    // Truncate long text (disconnected hint, long layout titles)
                    // instead of overflowing: content wider than the sidebar
                    // makes egui reserve the overflow as an unpainted strip
                    // next to the panel. Full text stays readable on hover.
                    ui.add(
                        egui::Label::new(RichText::new(&text).color(pal::TEXT_DIM)).truncate(),
                    )
                    .on_hover_text(hover);
                });
            });
    }


    /// The Layers tab: overview + management of every layer (Oryx + custom).
    fn ui_layers(&mut self, ui: &mut egui::Ui) {
        // Centered column.
        let full = ui.available_width();
        let w = full.min(920.0);
        let pad = ((full - w) / 2.0).max(12.0);
        ui.add_space(14.0);
        let mut go_live: Option<u8> = None;
        let mut do_add: Option<String> = None;
        let mut do_remove: Option<u8> = None;
        let mut do_rename: Option<(u8, String)> = None;

        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(w - 24.0);
                ui.label(RichText::new("Layers").strong().size(21.0).color(pal::TEXT));
                ui.label(
                    RichText::new("Every layer of your layout. Oryx layers are the base; ★ layers are yours - authored and built locally.")
                        .size(12.5)
                        .color(pal::TEXT_DIM),
                );
                ui.add_space(14.0);

                if self.layout.is_none() {
                    card(ui, "No layout", |ui| {
                        ui.label(RichText::new("Connect the Voyager (and quit Keymapp) so keyjitsu can read your layout - then you can add and edit layers here.").size(12.5).color(pal::TEXT_MUTED));
                    });
                    return;
                }

                let oryx = self.oryx_layer_count();
                let active = self.active_layer;
                let count = self.layer_count();
                for n in 0..count {
                    let custom = n >= oryx;
                    let name = self.layer_name(n);
                    let keycount = self
                        .layer_def(n)
                        .map(|l| l.keys.iter().filter(|k| {
                            k.tap.as_ref().and_then(|a| a.code.as_deref()).is_some_and(|c| c != "KC_NO" && c != "KC_TRANSPARENT")
                                || k.hold.is_some()
                        }).count())
                        .unwrap_or(0);
                    egui::Frame::new()
                        .fill(pal::CARD)
                        .stroke(egui::Stroke::new(1.0, if n == self.view_layer { pal::VIOLET } else { pal::BORDER }))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                // Index badge.
                                egui::Frame::new()
                                    .fill(pal::INPUT)
                                    .corner_radius(egui::CornerRadius::same(7))
                                    .inner_margin(egui::Margin::symmetric(10, 5))
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(format!("{n}")).strong().size(15.0).color(pal::TEXT));
                                    });
                                ui.add_space(6.0);
                                if custom {
                                    // Editable name for custom layers.
                                    let mut nm = name.clone();
                                    if ui.add(egui::TextEdit::singleline(&mut nm).desired_width(150.0)).changed() {
                                        do_rename = Some((n, nm));
                                    }
                                    status_pill(ui, "★ custom", pal::VIOLET_HI);
                                } else {
                                    ui.label(RichText::new(&name).strong().size(15.0).color(pal::TEXT));
                                    status_pill(ui, "Oryx", pal::TEXT_DIM);
                                }
                                ui.label(RichText::new(format!("{keycount} keys")).size(11.5).color(pal::TEXT_DIM));
                                if n == active {
                                    ui.colored_label(pal::GREEN, RichText::new("● on board").size(11.0));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if custom && ui.button("🗑").on_hover_text("delete this layer").clicked() {
                                        do_remove = Some(n);
                                    }
                                    if ui.button("View & edit").clicked() {
                                        go_live = Some(n);
                                    }
                                });
                            });
                        });
                    ui.add_space(6.0);
                }

                ui.add_space(4.0);
                // Add a new custom layer.
                if self.new_layer_open {
                    card(ui, "New layer", |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.new_layer_name).hint_text("layer name (e.g. Symbols)").desired_width(220.0));
                            let ok = !self.new_layer_name.trim().is_empty();
                            if ui.add_enabled(ok, egui::Button::new(RichText::new("Create").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                                do_add = Some(self.new_layer_name.trim().to_string());
                            }
                            if ui.button("Cancel").clicked() {
                                self.new_layer_open = false;
                            }
                        });
                        ui.label(RichText::new("Starts empty (all transparent). Fill its keys in Live, then add a switch key (Hold → this layer) somewhere.").size(11.5).color(pal::TEXT_DIM));
                    });
                } else if ui.add(egui::Button::new(RichText::new("＋ Add layer").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                    self.new_layer_open = true;
                    self.new_layer_name.clear();
                }
            });
        });

        if let Some(name) = do_add {
            self.add_custom_layer(name);
            self.new_layer_open = false;
            self.new_layer_name.clear();
            self.tab = Tab::Live; // jump to edit the fresh layer
        }
        if let Some((n, name)) = do_rename {
            self.rename_custom_layer(n, name);
        }
        if let Some(n) = do_remove {
            self.remove_custom_layer(n);
        }
        if let Some(n) = go_live {
            self.view_layer = n;
            self.follow = false;
            self.tab = Tab::Live;
        }
    }

    fn ui_live(&mut self, ui: &mut egui::Ui, avail_h: f32) {
        self.ui_edit_bar(ui);
        ui.add_space(4.0);
        // No layout at all (first run, nothing cached): a friendly hint beats a
        // grid of blank keys.
        if self.layout.is_none() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("⌨").size(46.0).color(pal::TEXT_DIM));
                ui.add_space(10.0);
                ui.label(RichText::new("No keyboard connected").strong().size(18.0).color(pal::TEXT));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Plug in your Voyager (and quit Keymapp) so keyjitsu can read your layout. It remembers the last one, so next time you can view and plan it here even with the keyboard unplugged.")
                        .size(12.5)
                        .color(pal::TEXT_MUTED),
                );
            });
            return;
        }
        // Fit the board to BOTH dimensions: the canvas shrinks (down to the
        // widget's 34px/unit legibility floor) so keyboard + inspector share
        // the window without scrolling; `avail_h` is measured by the caller
        // before any scroll wrapper, so it's the true remaining height.
        let (cols, rows) = self.board_units();
        let chrome = 88.0; // edit bar + canvas margins/shadow
        let unit_h = ((avail_h - chrome).max(120.0)) / rows;
        let board_w = (unit_h.clamp(34.0, 62.0) * cols + 48.0).min(ui.available_width());

        let view = self.view_layer;
        // Mirror the LEDs: while the animation engine drives the keyboard, show
        // its live frame; otherwise the static per-layer glow.
        let anim_frame: Option<Vec<Option<Color32>>> = self.anim.lock().ok().and_then(|a| {
            if a.frame.is_empty() {
                None
            } else {
                Some(
                    a.frame
                        .iter()
                        .map(|c| (*c != [0, 0, 0]).then(|| Color32::from_rgb(c[0], c[1], c[2])))
                        .collect(),
                )
            }
        });
        let glow = anim_frame.unwrap_or_else(|| self.glow_colors(view));
        let layer = self.layer_def(view);
        let sel = self.selected_key;
        // The keyboard sits on its own raised canvas card with a soft top
        // sheen + shadow, so it reads as the main object.
        let clicked = {
            let frame = egui::Frame::new()
                .fill(pal::CARD)
                .stroke(egui::Stroke::new(1.0, pal::BORDER))
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(14, 16))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(70),
                });
            // Center a height-budgeted canvas card; the board fills its width.
            let pad = ((ui.available_width() - board_w) / 2.0).max(0.0);
            let out = ui
                .horizontal(|ui| {
                    ui.add_space(pad);
                    frame.show(ui, |ui| {
                        ui.set_width(board_w - 28.0);
                        draw_keyboard(ui, self.geometry(), layer, &glow, &self.pressed, sel, 1.0, false).clicked
                    })
                })
                .inner;
            // Subtle top-light gradient over the card (very low alpha sheen).
            let r = out.response.rect;
            let sheen = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), 70.0));
            let mut mesh = egui::Mesh::default();
            let top = Color32::from_rgba_unmultiplied(255, 255, 255, 6);
            let bottom = Color32::TRANSPARENT;
            let idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(sheen.left_top(), top);
            mesh.colored_vertex(sheen.right_top(), top);
            mesh.colored_vertex(sheen.right_bottom(), bottom);
            mesh.colored_vertex(sheen.left_bottom(), bottom);
            mesh.add_triangle(idx, idx + 1, idx + 2);
            mesh.add_triangle(idx, idx + 2, idx + 3);
            ui.painter().add(egui::Shape::mesh(mesh));
            out.inner
        };

        // Clicking a key selects it for the bottom config panel.
        if let Some(i) = clicked {
            self.selected_key = Some(i);
            self.edit_color = self.current_key_srgb(view, i);
            self.sync_editor_from_key(view, i);
        }

        // Flash lives in its own window now, not in the main flow.
        if self.show_flash {
            let mut open = self.show_flash;
            egui::Window::new("⚡ Flash firmware")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .show(ui.ctx(), |ui| self.flash_controls(ui));
            self.show_flash = open;
        }
    }

    /// Bottom panel: configure whichever key is selected (pressed or clicked).
    fn ui_key_panel(&mut self, ui: &mut egui::Ui) {
        let view = self.view_layer;
        let Some(i) = self.selected_key else {
            ui.add_space(4.0);
            if self.connected.is_some() {
                ui.weak("No key selected - press a key on the keyboard, or click one above.");
            } else {
                ui.weak("No key selected - click a key above.");
            }
            ui.add_space(4.0);
            return;
        };
        // Hydrate the slot editor whenever the inspected key/layer changes
        // (covers layer switches, profile loads and env preselection).
        if self.edit_synced != Some((view, i)) {
            self.sync_editor_from_key(view, i);
            self.edit_color = self.current_key_srgb(view, i);
            self.edit_synced = Some((view, i));
        }
        let tap = match self.layer_def(view).and_then(|l| l.keys.get(i)) {
            Some(key) => labels_for(key).tap,
            None => format!("key {i}"),
        };

        let pos = self.geometry().keys[i].layout_pos as usize;
        let staged = self.key_edits.get(&(view, i)).cloned();
        let key_col = self.layout_glow(view, i).unwrap_or(pal::VIOLET);

        // --- header: ONE compact row - badge · identity · status · actions --
        let (preview, warns) = self.compose_slots();
        let staged_dance = self.key_dances.contains_key(&(view, i));
        ui.add_space(2.0);
        egui::Frame::new()
            .fill(pal::CARD)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(9))
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(pal::INPUT)
                        .stroke(egui::Stroke::new(2.0, key_col))
                        .corner_radius(egui::CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(9, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(if tap.is_empty() { "-".into() } else { tap.clone() }).size(16.0).strong().color(pal::TEXT));
                        });
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("{} · key {i}", self.layer_name(view))).size(12.5).color(pal::TEXT_MUTED));
                    let assigned = self
                        .layer_def(view)
                        .and_then(|l| l.keys.get(i))
                        .map(|k| self.describe_assignment(k))
                        .unwrap_or_else(|| "No assignment".into());
                    ui.label(RichText::new(assigned).strong().size(13.0).color(pal::VIOLET_HI));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.inspector_build_actions(ui);
                        // Staged/preview status lives here, not on its own row.
                        if staged_dance {
                            ui.colored_label(pal::AMBER, format!("staged: tap dance (#{pos})"));
                            if ui.small_button("✕").on_hover_text("unstage").clicked() {
                                self.key_dances.remove(&(view, i));
                                self.save_staged();
                                self.sync_editor_from_key(view, i);
                            }
                        } else if let Some(sc) = &staged {
                            ui.colored_label(pal::AMBER, format!("staged: {sc} (#{pos})"));
                            if ui.small_button("✕").on_hover_text("unstage").clicked() {
                                self.key_edits.remove(&(view, i));
                                self.save_staged();
                                self.sync_editor_from_key(view, i);
                            }
                        } else {
                            ui.label(RichText::new(format!("→ {preview}")).size(11.5).color(pal::TEXT_DIM));
                        }
                    });
                });
            });
        ui.add_space(6.0);

        // --- binding rows: one per action slot ------------------------------
        // Columns: type | key (click → picker) | glow | on-press | ✕.
        let mut open_picker: Option<usize> = None;
        let mut clear_slot: Option<usize> = None;
        egui::Grid::new("slot_rows")
            .num_columns(5)
            .spacing([14.0, 6.0])
            .with_row_color(|row, _style| if row == 0 { None } else { Some(Color32::from_rgb(0x23, 0x26, 0x32)) })
            .show(ui, |ui| {
            let head = |ui: &mut egui::Ui, t: &str| {
                ui.label(RichText::new(t).size(10.5).color(pal::TEXT_DIM));
            };
            head(ui, "TYPE");
            head(ui, "KEY");
            head(ui, "GLOW");
            head(ui, "ON PRESS");
            head(ui, "");
            ui.end_row();

            let mut first = true;
            for slot in 0..4 {
                let visible = slot == 0 || self.edit_slots[slot].is_some() || self.slot_added[slot];
                if !visible {
                    continue;
                }
                // Type badge: a distinct color per action tier, clearly visible.
                let tc = SLOT_COLORS[slot];
                egui::Frame::new()
                    .fill(tc.gamma_multiply(0.28))
                    .stroke(egui::Stroke::new(1.2, tc))
                    .corner_radius(egui::CornerRadius::same(7))
                    .inner_margin(egui::Margin::symmetric(10, 3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(SLOT_LABELS[slot]).size(12.0).strong().color(pal::TEXT));
                    });

                // Key chip - click to change via the picker.
                let chip = match &self.edit_slots[slot] {
                    Some(c) => self.slot_chip_label(c),
                    None => "- pick…".to_string(),
                };
                let chip_btn = egui::Button::new(RichText::new(chip).size(13.0).color(pal::TEXT))
                    .fill(pal::INPUT)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .min_size(egui::vec2(96.0, 22.0));
                if ui.add(chip_btn).on_hover_text("click to pick a key").clicked() {
                    open_picker = Some(slot);
                }

                if first {
                    // Glow color (key-level).
                    ui.horizontal(|ui| {
                        if ui.color_edit_button_srgb(&mut self.edit_color).changed() {
                            self.set_glow(view, i, self.edit_color);
                        }
                        if ui.small_button("↺").on_hover_text("reset to layout color").clicked() {
                            self.clear_glow(view, i);
                            self.edit_color = self.current_key_srgb(view, i);
                        }
                    });
                    // On-press effect (key-level): built-ins + ★ sequences.
                    ui.horizontal(|ui| {
                        let mut fx = self
                            .key_fx
                            .get(&(view, i))
                            .cloned()
                            .unwrap_or((FxTrigger::Press, PressEffect::None, [255, 255, 255], None));
                        let mut changed = false;
                        let custom_names: Vec<String> = self.custom_fx.iter().map(|c| c.name.clone()).collect();
                        let sel_text = match &fx.3 {
                            Some(n) => format!("★ {n}"),
                            None => fx.1.label().to_string(),
                        };
                        egui::ComboBox::from_id_salt(("keyfx", i))
                            .width(170.0)
                            .selected_text(sel_text)
                            .show_ui(ui, |ui| {
                                for (e, label) in PressEffect::ALL {
                                    let is = fx.3.is_none() && fx.1 == e;
                                    if ui.selectable_label(is, label).clicked() {
                                        fx.1 = e;
                                        fx.3 = None;
                                        changed = true;
                                    }
                                }
                                if !custom_names.is_empty() {
                                    ui.separator();
                                }
                                for name in &custom_names {
                                    let is = fx.3.as_deref() == Some(name.as_str());
                                    if ui.selectable_label(is, format!("★ {name}")).clicked() {
                                        fx.3 = Some(name.clone());
                                        fx.1 = PressEffect::None;
                                        changed = true;
                                    }
                                }
                            });
                        if fx.1 != PressEffect::None || fx.3.is_some() {
                            egui::ComboBox::from_id_salt(("keyfxtrig", i))
                                .width(110.0)
                                .selected_text(fx.0.label())
                                .show_ui(ui, |ui| {
                                    for (t, label) in FxTrigger::ALL {
                                        changed |= ui.selectable_value(&mut fx.0, t, label).changed();
                                    }
                                });
                            if fx.3.is_none() && fx.1.uses_color() {
                                changed |= ui.color_edit_button_srgb(&mut fx.2).changed();
                            }
                        }
                        if changed {
                            if fx.1 == PressEffect::None && fx.3.is_none() {
                                self.key_fx.remove(&(view, i));
                            } else {
                                self.key_fx.insert((view, i), fx);
                            }
                            self.save_key_fx();
                        }
                    });
                } else {
                    ui.label("");
                    ui.label("");
                }

                if slot > 0 {
                    if ui.small_button("✕").on_hover_text("remove this action").clicked() {
                        clear_slot = Some(slot);
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
                first = false;
            }
        });

        // ＋ under the table, centered.
        let missing: Vec<usize> = (1..4)
            .filter(|&sl| self.edit_slots[sl].is_none() && !self.slot_added[sl])
            .collect();
        if !missing.is_empty() {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.menu_button(RichText::new("＋ add action").size(12.0), |ui| {
                    for sl in missing {
                        if ui.button(SLOT_LABELS[sl]).clicked() {
                            self.slot_added[sl] = true;
                            open_picker = Some(sl);
                            ui.close_menu();
                        }
                    }
                });
            });
        }
        // Make the dual-role obvious: Tap + Hold together = "tap sends one,
        // holding sends the other" (LT/mod-tap). The double-* rows are the
        // extra tap-dance actions.
        if self.edit_slots[1].is_some() && self.edit_slots[2].is_none() && self.edit_slots[3].is_none() {
            ui.add_space(2.0);
            ui.label(RichText::new("Tap + Hold = dual-role: tapping sends the Tap key, holding does the Hold action.").size(10.5).color(pal::TEXT_DIM));
        }

        if let Some(slot) = clear_slot {
            self.edit_slots[slot] = None;
            self.slot_added[slot] = false;
            self.stage_slots(view, i);
        }
        if let Some(slot) = open_picker {
            self.picker_slot = slot;
            self.picker_open = true;
            self.picker_search.clear();
        }
        for w in warns {
            ui.colored_label(pal::AMBER, RichText::new(w).size(11.0));
        }

        ui.add_space(4.0);
    }

    /// Floating build/flash modal: phase, progress bar, and readable logs.
    fn ui_build_modal(&mut self, ctx: &egui::Context) {
        if !self.build_open {
            return;
        }
        let mut open = true;
        egui::Window::new("⚙ Build & flash")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .max_height(ctx.screen_rect().height() * 0.8)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(520.0);
                // Phase line.
                ui.horizontal(|ui| {
                    if self.build_busy {
                        ui.spinner();
                    }
                    let (icon, col) = match &self.build_result {
                        Some(Ok(_)) => ("✓", pal::GREEN),
                        Some(Err(_)) => ("✗", pal::RED),
                        None => ("", pal::VIOLET_HI),
                    };
                    if !icon.is_empty() {
                        ui.label(RichText::new(icon).strong().color(col));
                    }
                    ui.label(RichText::new(&self.build_phase).strong().size(15.0).color(pal::TEXT));
                });
                ui.add_space(6.0);
                // Progress bar (animated while running).
                ui.add(
                    egui::ProgressBar::new(self.build_progress.clamp(0.0, 1.0))
                        .desired_height(10.0)
                        .fill(if self.build_result.as_ref().is_some_and(|r| r.is_err()) { pal::RED } else { pal::VIOLET })
                        .animate(self.build_busy),
                );
                ui.add_space(8.0);

                // Result card (success/failure).
                if let Some(res) = self.build_result.clone() {
                    match res {
                        Ok(msg) => {
                            ui.colored_label(pal::GREEN, RichText::new(msg).size(12.5));
                        }
                        Err(e) => {
                            ui.colored_label(pal::RED, RichText::new(format!("Failed: {e}")).size(12.5));
                        }
                    }
                    ui.add_space(6.0);
                }

                // Bootloader hint during the wait.
                if matches!(self.flash_state, Some(FlashState::WaitingForBootloader)) {
                    ui.colored_label(pal::AMBER, "→ Press the small reset button on the Voyager now (don't unplug it).");
                    ui.add_space(6.0);
                }

                // Readable, auto-scrolled log.
                egui::CollapsingHeader::new("Logs")
                    .default_open(self.build_result.is_some())
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(pal::BG)
                            .stroke(egui::Stroke::new(1.0, pal::BORDER))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(220.0)
                                    .auto_shrink([false, false])
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        for line in self.build_log.lines() {
                                            let col = if line.contains('✗') || line.to_lowercase().contains("error") {
                                                pal::RED
                                            } else if line.contains("[OK]") || line.contains('✓') {
                                                pal::GREEN
                                            } else if line.contains("Compiling") {
                                                pal::TEXT_DIM
                                            } else {
                                                pal::TEXT_MUTED
                                            };
                                            ui.label(RichText::new(line).monospace().size(11.0).color(col));
                                        }
                                    });
                            });
                        if ui.button("⧉ Copy logs").clicked() {
                            ui.ctx().copy_text(self.build_log.clone());
                        }
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.build_busy {
                        if ui.button("✕ Cancel").clicked() {
                            self.build_cancel.store(true, Ordering::SeqCst);
                            self.flash_cancel.store(true, Ordering::SeqCst);
                            self.build_log.push_str("canceling…\n");
                        }
                    } else if ui.add(egui::Button::new(RichText::new("Close").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                        self.build_open = false;
                    }
                });
            });
        // Window's own ✕ closes it (only when not busy).
        if !open && !self.build_busy {
            self.build_open = false;
        }
    }

    /// The build/unsaved indicator + actions shown in the inspector header.
    fn inspector_build_actions(&mut self, ui: &mut egui::Ui) {
        let edits = self.key_edits.len() + self.key_dances.len();
        if self.build_busy {
            ui.spinner();
            if ui.button("✕ cancel").clicked() {
                self.build_cancel.store(true, Ordering::SeqCst);
                self.build_log.push_str("canceling…\n");
            }
            return;
        }
        if edits == 0 {
            return;
        }
        let ready = self.env.is_ready();
        if ui
            .add_enabled(ready, egui::Button::new(RichText::new("⚙ Build & flash").color(Color32::WHITE)).fill(pal::VIOLET))
            .clicked()
        {
            self.start_local_build(true);
        }
        if ui.add_enabled(ready, egui::Button::new("Build only")).on_hover_text("compile without flashing").clicked() {
            self.start_local_build(false);
        }
        if ui.button("Clear").clicked() {
            self.key_edits.clear();
            self.key_dances.clear();
            self.save_staged();
        }
        if !ready {
            ui.weak("set up QMK →");
        }
        ui.colored_label(pal::AMBER, RichText::new(format!("● {edits} unsaved")).strong());
    }

    /// Human name of a layer ("VimLife"), falling back to "Layer n".
    fn layer_name(&self, n: u8) -> String {
        self.layer_def(n)
            .and_then(|l| l.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Layer {n}"))
    }

    /// Human description of what a key currently does, using layer NAMES -
    /// e.g. "Hold → VimLife", "Tap 1 · Hold ⇧", "Switch to Numpad".
    fn describe_assignment(&self, key: &crate::oryx_api::OryxKey) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(tap) = &key.tap {
            if let Some(n) = tap.layer {
                let name = self.layer_name(n);
                let what = match tap.code.as_deref() {
                    Some("TO") => format!("Switch to {name}"),
                    Some("TG") => format!("Toggle {name}"),
                    Some("TT") => format!("Tap-toggle {name}"),
                    Some("OSL") => format!("One-shot {name}"),
                    Some("DF") => format!("Default {name}"),
                    _ => format!("Momentary {name}"),
                };
                parts.push(what);
            } else if let Some(code) = tap.code.as_deref() {
                if code != "KC_TRANSPARENT" && code != "KC_NO" {
                    parts.push(format!("Tap {}", legend::keycode_label(code)));
                }
            }
        }
        if let Some(hold) = &key.hold {
            if let Some(n) = hold.layer {
                parts.push(format!("Hold → {}", self.layer_name(n)));
            } else if let Some(code) = hold.code.as_deref() {
                parts.push(format!("Hold {}", legend::keycode_label(code)));
            }
        }
        // Some keys carry a third "tap-hold" action (e.g. a second momentary
        // layer) - surface it so the key's full behavior is visible.
        if let Some(th) = &key.tap_hold {
            if let Some(n) = th.layer {
                let name = self.layer_name(n);
                if !parts.iter().any(|p| p.contains(&name)) {
                    parts.push(format!("2×tap-hold → {name}"));
                }
            }
        }
        if parts.is_empty() {
            "No assignment".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// Prefill the editor (behavior/mod/layer/tap) from the key's CURRENT
    /// assignment, so the inspector reflects reality when a key is selected.
    fn sync_editor_from_key(&mut self, layer: u8, i: usize) {
        self.slot_added = [false; 4];
        self.edit_slots = [None, None, None, None];
        let Some(key) = self.layer_def(layer).and_then(|l| l.keys.get(i)) else { return };
        // An action becomes a working keycode: layer actions render as
        // `CODE(n)` (MO(1), OSL(2)…), plain actions as their code.
        let conv = |a: &Option<crate::oryx_api::KeyAction>| -> Option<String> {
            let a = a.as_ref()?;
            match (a.code.as_deref(), a.layer) {
                (Some(c), Some(n)) => Some(format!("{c}({n})")),
                (None, Some(n)) => Some(format!("MO({n})")),
                (Some(c), None) if c != "KC_TRANSPARENT" && c != "KC_NO" => Some(c.to_string()),
                _ => None,
            }
        };
        self.edit_slots = [conv(&key.tap), conv(&key.hold), conv(&key.double_tap), conv(&key.tap_hold)];
    }

    /// Human chip label for a slot's working keycode ("MO → VimLife", "⇧"…).
    fn slot_chip_label(&self, code: &str) -> String {
        for p in ["MO", "OSL", "TO", "TG", "TT", "DF", "LT"] {
            if let Some(rest) = code.strip_prefix(p).and_then(|r| r.strip_prefix('(')) {
                if let Some(inner) = rest.strip_suffix(')') {
                    if let Ok(n) = inner.split(',').next().unwrap_or("").trim().parse::<u8>() {
                        return format!("{p} → {}", self.layer_name(n));
                    }
                }
            }
        }
        legend::keycode_label(code)
    }

    /// Compose the buildable QMK keycode from the slot rows + any warnings
    /// about parts the local build can't express yet.
    fn compose_slots(&self) -> (String, Vec<String>) {
        let mut warns = Vec::new();
        // No tap + a hold action = the hold code itself (a plain MO/mod key).
        if self.edit_slots[0].is_none() {
            if let Some(h) = self.edit_slots[1].clone() {
                if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
                    warns.push("double-tap / double-tap-hold → keyjitsu generates a tap dance in the firmware".to_string());
                }
                return (h, warns);
            }
        }
        let tap = self.edit_slots[0].clone().unwrap_or_else(|| "KC_NO".to_string());
        let code = match self.edit_slots[1].as_deref() {
            None => tap.clone(),
            Some(h) => match hold_wrap(h, &tap) {
                Some(c) => c,
                None => {
                    warns.push(format!("hold: {} isn't a modifier or MO(layer) - skipped in the build", self.slot_chip_label(h)));
                    tap.clone()
                }
            },
        };
        if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
            warns.push("double-tap / double-tap-hold → keyjitsu generates a tap dance in the firmware".to_string());
        }
        (code, warns)
    }

    /// Re-stage after a slot change: plain keys become one keycode, keys with
    /// double-tap / tap+hold become a generated tap dance.
    fn stage_slots(&mut self, layer: u8, key: usize) {
        // Custom layers persist directly (they have no Oryx source to patch);
        // dances aren't wired for them yet, so use the composed base code.
        if self.is_custom_layer(layer) {
            let (code, _) = self.compose_slots();
            self.set_custom_key(layer, key, &code);
            return;
        }
        if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
            self.key_dances.insert((layer, key), self.edit_slots.clone());
            self.key_edits.remove(&(layer, key));
            self.save_staged();
            return;
        }
        self.key_dances.remove(&(layer, key));
        let (code, _) = self.compose_slots();
        if code == "KC_NO" {
            self.key_edits.remove(&(layer, key));
        } else {
            self.key_edits.insert((layer, key), code);
        }
        self.save_staged();
    }

    /// Floating Oryx-style keycode picker for the selected key.
    fn ui_picker(&mut self, ctx: &egui::Context) {
        if !self.picker_open {
            return;
        }
        let Some(key) = self.selected_key else {
            self.picker_open = false;
            return;
        };
        let view = self.view_layer;
        let mut open = self.picker_open;
        let mut pick: Option<String> = None;

        egui::Window::new(format!("Assign - {}", SLOT_LABELS[self.picker_slot.min(3)]))
            .open(&mut open)
            .default_width(520.0)
            .default_height(420.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("search:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.picker_search)
                            .hint_text("filter all keycodes…")
                            .desired_width(220.0),
                    );
                    if !self.picker_search.is_empty() && ui.button("clear").clicked() {
                        self.picker_search.clear();
                    }
                });
                ui.separator();

                let query = self.picker_search.trim().to_lowercase();
                if query.is_empty() {
                    // Category tabs.
                    ui.horizontal_wrapped(|ui| {
                        for (idx, cat) in keycodes::CATALOG.iter().enumerate() {
                            ui.selectable_value(&mut self.picker_cat, idx, cat.name);
                        }
                    });
                    let cat = &keycodes::CATALOG[self.picker_cat.min(keycodes::CATALOG.len() - 1)];
                    let templated = cat.templated;
                    let cat_keys = cat.keys;
                    if templated {
                        let names: Vec<String> = (0..self.layer_count().max(1)).map(|n| self.layer_name(n)).collect();
                        if self.picker_layer_arg as usize >= names.len() {
                            self.picker_layer_arg = 0;
                        }
                        ui.horizontal(|ui| {
                            ui.label("target layer:");
                            egui::ComboBox::from_id_salt("picker_target_layer")
                                .selected_text(format!("{} · {}", self.picker_layer_arg, names.get(self.picker_layer_arg as usize).cloned().unwrap_or_default()))
                                .show_ui(ui, |ui| {
                                    for (i, nm) in names.iter().enumerate() {
                                        ui.selectable_value(&mut self.picker_layer_arg, i as u8, format!("{i} · {nm}"));
                                    }
                                });
                        });
                    }
                    ui.separator();
                    let arg = self.picker_layer_arg;
                    let lname = self.layer_name(arg);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        keycode_grid(ui, cat_keys, templated, arg, &lname, &mut pick);
                    });
                } else {
                    // Flat search across every category.
                    let lname = self.layer_name(self.picker_layer_arg);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let arg = self.picker_layer_arg;
                        for cat in keycodes::CATALOG {
                            let hits: Vec<&keycodes::KeyDef> = cat
                                .keys
                                .iter()
                                .filter(|k| {
                                    k.code.to_lowercase().contains(&query)
                                        || k.label.to_lowercase().contains(&query)
                                })
                                .collect();
                            if hits.is_empty() {
                                continue;
                            }
                            ui.label(RichText::new(cat.name).weak().size(11.0));
                            let refs: Vec<keycodes::KeyDef> = hits
                                .iter()
                                .map(|k| keycodes::KeyDef { code: k.code, label: k.label })
                                .collect();
                            keycode_grid(ui, &refs, cat.templated, arg, &lname, &mut pick);
                        }
                    });
                }
            });

        if let Some(code) = pick {
            // The picked code lands in whichever slot row opened the picker;
            // the buildable keycode is recomposed from all slots.
            let slot = self.picker_slot.min(3);
            self.edit_slots[slot] = Some(code);
            self.slot_added[slot] = false;
            self.stage_slots(view, key);
            open = false;
        }
        self.picker_open = open;
    }

    /// Arm the peek HUD for layer `n` if the config wants it.
    fn maybe_peek(&mut self, n: u8) {
        if !self.peek.enabled {
            return;
        }
        if self.peek.only_non_base && n == 0 {
            // Returning to base: dismiss any active peek immediately.
            self.peek_until = None;
            return;
        }
        self.peek_layer = n;
        // "Only outside the base layer" = the minimap stays up for the whole
        // stay on the layer (dismissed by the return to base above); otherwise
        // it's a timed flash.
        self.peek_until = Some(if self.peek.only_non_base {
            Instant::now() + Duration::from_secs(3600)
        } else {
            Instant::now() + Duration::from_millis(self.peek.duration_ms)
        });
    }

    /// Draw the transparent, click-through layer peek as its own polished,
    /// card-like viewport, positioned on the chosen monitor.
    fn show_peek(&self, ctx: &egui::Context) {
        let geo = self.geometry();
        let scale = self.peek.scale.clamp(0.5, 1.6);
        // Card padding + header add to the raw keyboard size.
        let pad = 16.0;
        let header = if self.peek.show_layer_name { 40.0 } else { 0.0 };
        let kb_w = 620.0 * scale;
        // draw_keyboard sizes by width, so derive the unit from it.
        let unit = kb_w / PEEK_BOARD_UNITS_WIDE;
        let width = kb_w + pad * 2.0;
        let combo_h = if self.peek.show_combo { 42.0 } else { 0.0 };
        let height = unit * PEEK_BOARD_UNITS_TALL + header + combo_h + pad * 2.0;

        // Position on the selected monitor (falls back to the main display).
        let (mx, my, mw, mh) = self.peek_monitor_rect(ctx);
        let edge = 48.0;
        let x = mx
            + match self.peek.halign {
                HAlign::Left => edge,
                HAlign::Center => (mw - width) / 2.0,
                HAlign::Right => mw - width - edge,
            }
            + self.peek.offset[0];
        let y = my
            + match self.peek.valign {
                VAlign::Top => edge,
                VAlign::Middle => (mh - height) / 2.0,
                VAlign::Bottom => mh - height - edge - 24.0,
            }
            + self.peek.offset[1];

        let builder = egui::ViewportBuilder::default()
            .with_title("keyjitsu peek")
            .with_inner_size([width, height])
            .with_position([x, y])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_taskbar(false)
            .with_mouse_passthrough(true)
            .with_always_on_top();

        let layer_def = self.layer_def(self.peek_layer);
        let glow = self.glow_colors(self.peek_layer);
        let legends = if self.peek.show_legends { layer_def } else { None };
        let title = layer_def
            .and_then(|l| l.title.clone())
            .unwrap_or_else(|| format!("Layer {}", self.peek_layer));
        // Overall translucency, applied to the whole overlay so it reads like
        // frosted glass (panel + keys + text fade together) rather than a solid
        // panel with opaque keys.
        let opacity = self.peek.opacity.clamp(0.08, 1.0);
        let accent = Color32::from_rgb(self.peek.accent[0], self.peek.accent[1], self.peek.accent[2]);
        // With the combo HUD on, mirror physically-held keys so a hold lights
        // up live on the minimap; otherwise no press highlight.
        let no_press = if self.peek.show_combo { self.pressed.clone() } else { vec![false; geo.len()] };
        let peek_layer = self.peek_layer;
        let show_name = self.peek.show_layer_name;
        let show_bg = self.peek.show_background;
        let mono = self.peek.monochrome;
        let show_combo = self.peek.show_combo;
        let show_combo_ms = self.peek.show_combo_ms;
        let combo = if show_combo { self.combo_recent() } else { Vec::new() };

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("keyjitsu_peek"),
            builder,
            move |ctx, _class| {
                // The window is transparent; the opacity is baked directly into
                // every color's alpha (card, keys, text), so the whole overlay
                // genuinely becomes see-through as the slider goes down - the
                // desktop shows through more, not a fade-to-black.
                let a = (opacity * 255.0) as u8;
                // Background (dark card) is optional - off = keys float on pure
                // transparency. Colors are optional too (monochrome high-contrast).
                let card_fill = if show_bg {
                    Color32::from_rgba_unmultiplied(17, 18, 24, a)
                } else {
                    Color32::TRANSPARENT
                };
                let card_stroke = if show_bg {
                    egui::Stroke::new(
                        1.0,
                        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), (a as f32 * 0.6) as u8),
                    )
                } else {
                    egui::Stroke::NONE
                };
                let clear = egui::Frame::new().fill(Color32::TRANSPARENT);
                egui::CentralPanel::default().frame(clear).show(ctx, |ui| {
                    let card = egui::Frame::new()
                        .fill(card_fill)
                        .stroke(card_stroke)
                        .inner_margin(egui::Margin::same(pad as i8))
                        .corner_radius(egui::CornerRadius::same(16))
                        .shadow(if show_bg {
                            egui::epaint::Shadow {
                                offset: [0, 6],
                                blur: 22,
                                spread: 0,
                                color: Color32::from_black_alpha((90.0 * opacity) as u8),
                            }
                        } else {
                            egui::epaint::Shadow::NONE
                        });
                    card.show(ui, |ui| {
                        if show_name {
                            ui.horizontal(|ui| {
                                egui::Frame::new()
                                    .fill(Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), a))
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(9, 4))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(format!("L{peek_layer}"))
                                                .strong()
                                                .color(Color32::from_rgba_unmultiplied(255, 255, 255, a)),
                                        );
                                    });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(&title)
                                        .size(16.0)
                                        .color(Color32::from_rgba_unmultiplied(235, 236, 242, a)),
                                );
                            });
                            ui.add_space(8.0);
                        }
                        draw_keyboard(ui, geo, legends, &glow, &no_press, None, opacity, mono);
                        if show_combo {
                            ui.add_space(6.0);
                            combo_strip(ui, &combo, opacity, accent, show_combo_ms);
                        }
                    });
                });
            },
        );
    }

    /// (x, y, w, h) of the monitor to show the peek on, in egui point space.
    fn peek_monitor_rect(&self, ctx: &egui::Context) -> (f32, f32, f32, f32) {
        if let Some(m) = self
            .monitors_cache
            .get(self.peek.monitor)
            .or_else(|| self.monitors_cache.first())
        {
            return (m.x, m.y, m.w, m.h);
        }
        let mon = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1440.0, 900.0));
        (0.0, 0.0, mon.x, mon.y)
    }

    /// A flash is actively downloading / waiting / writing (not a terminal state).
    fn flash_in_progress(&self) -> bool {
        self.flash_rx.is_some()
            && matches!(
                self.flash_state,
                None | Some(FlashState::Downloading)
                    | Some(FlashState::WaitingForBootloader)
                    | Some(FlashState::Working { .. })
            )
    }

    fn start_local_build(&mut self, flash_after: bool) {
        // Don't start a build while one is running, or on top of a flash that's
        // still writing to the device (would spawn a second concurrent flasher).
        if self.build_busy || self.flash_in_progress() {
            return;
        }
        let Some((_, serial)) = &self.connected else { return };
        let Ok(id) = LayoutId::from_serial(serial) else { return };
        let n_keys = self.geometry().len();
        // Translate (layer, visual key) edits into (layer, LAYOUT position).
        let edits: Vec<KeyEdit> = self
            .key_edits
            .iter()
            .filter(|(&(_, key), _)| key < n_keys)
            .map(|(&(layer, key), code)| KeyEdit {
                layer,
                position: self.geometry().keys[key].layout_pos as usize,
                keycode: code.clone(),
            })
            .collect();
        let dances: Vec<crate::keymap::DanceSpec> = self
            .key_dances
            .iter()
            .filter(|(&(_, key), _)| key < n_keys)
            .map(|(&(layer, key), slots)| crate::keymap::DanceSpec {
                layer,
                position: self.geometry().keys[key].layout_pos as usize,
                tap: slots[0].clone(),
                hold: slots[1].clone(),
                double_tap: slots[2].clone(),
                tap_hold: slots[3].clone(),
            })
            .collect();
        // User-authored layers → new LAYOUT blocks (keys visual→LAYOUT pos).
        let oryx = self.oryx_layer_count();
        let new_layers: Vec<localbuild::NewLayer> = self
            .custom_layers
            .iter()
            .enumerate()
            .map(|(i, cl)| localbuild::NewLayer {
                position: oryx + i as u8,
                keys: cl
                    .keys
                    .iter()
                    // Skip any out-of-geometry key from a hand-edited/foreign
                    // config instead of panicking on the index.
                    .filter_map(|k| {
                        self.geometry()
                            .keys
                            .get(k.key as usize)
                            .map(|g| (g.layout_pos as usize, k.code.clone()))
                    })
                    .collect(),
            })
            .collect();
        self.build_log.clear();
        self.build_busy = true;
        self.build_flash_after = flash_after;
        self.build_open = true;
        self.build_phase = "Starting…".into();
        self.build_progress = 0.02;
        self.build_compiles = 0;
        self.build_result = None;
        // Clear any terminal flash state from a PREVIOUS run so the modal
        // doesn't immediately read "Flashed ✓" over a build that's still going.
        self.flash_state = None;
        self.flash_rx = None;
        self.build_cancel = Arc::new(AtomicBool::new(false));
        self.build_rx = Some(localbuild::spawn_build(
            id.revision.clone(),
            edits,
            dances,
            new_layers,
            self.build_cancel.clone(),
            self.egui_ctx.clone(),
        ));
    }

    /// The "unsaved changes" bar + glow-sync toggle.
    fn ui_edit_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let unsaved = self.unsaved_count();
            if ui
                .checkbox(&mut self.sync_glow, "show glow on keyboard")
                .on_hover_text("mirror these colors onto the physical LEDs (takes RGB control)")
                .changed()
            {
                if self.sync_glow {
                    self.needs_push = true;
                } else {
                    let _ = self.cmd_tx.send(KbCmd::RgbRelease);
                }
            }
            ui.separator();
            if unsaved == 0 {
                ui.weak("no unsaved changes");
            } else {
                ui.colored_label(
                    pal::AMBER,
                    format!("● {unsaved} unsaved change{}", if unsaved == 1 { "" } else { "s" }),
                );
                if ui.button("save").clicked() {
                    self.save_glow();
                }
                if ui.button("discard").clicked() {
                    self.discard_glow();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚡ Flash firmware…").clicked() {
                    self.show_flash = true;
                }
                if let Some(FlashState::Working { .. } | FlashState::WaitingForBootloader | FlashState::Downloading) = self.flash_state {
                    status_pill(ui, "flashing…", pal::AMBER);
                }
                if self.layout.is_none() && self.connected.is_some() {
                    ui.label(RichText::new("no Oryx layout - keys light without legends").size(11.5).color(pal::TEXT_DIM));
                }
            });
        });
    }

    fn ui_heatmap(&mut self, ui: &mut egui::Ui, avail_h: f32) {
        let key_count = self.geometry().len();
        if self.heat.is_none() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label("Connect the keyboard to collect statistics."));
            return;
        }
        let (counts, total) = {
            let heat = self.heat.as_ref().unwrap();
            (heat.counts(self.heat_layer, key_count), heat.total_presses())
        };
        let norm = normalize(&counts);
        let layer_total: u64 = counts.iter().sum();
        let mut ranked: Vec<(usize, u64)> =
            counts.iter().copied().enumerate().filter(|(_, c)| *c > 0).collect();
        ranked.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        let layer = self.heat_layer.unwrap_or(self.view_layer);
        let top_label = ranked.first().map(|(idx, _)| {
            self.layer_def(layer)
                .and_then(|l| l.keys.get(*idx))
                .map(|k| labels_for(k).tap)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("key {idx}"))
        });

        // --- Stats summary card --------------------------------------------
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(pal::CARD)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let stat = |ui: &mut egui::Ui, value: String, label: &str| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(value).strong().size(20.0).color(pal::TEXT));
                            ui.label(RichText::new(label).size(11.0).color(pal::TEXT_DIM));
                        });
                        ui.add_space(26.0);
                    };
                    stat(ui, format_thousands(total), "total presses");
                    stat(ui, top_label.unwrap_or_else(|| "-".into()), "most used key");
                    let mins = self.app_started.elapsed().as_secs() / 60;
                    stat(ui, format!("{mins} min"), "this session");
                    // Scope (all layers / per layer) is picked in the sidebar.
                    stat(ui, match self.heat_layer {
                        None => "all layers".to_string(),
                        Some(n) => self.layer_name(n),
                    }, "scope");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.confirm_reset {
                            if ui.button(RichText::new("Really delete?").color(pal::RED)).clicked() {
                                if let Some((_, serial)) = &self.connected {
                                    if let Ok(id) = LayoutId::from_serial(serial) {
                                        let _ = HeatmapStore::reset(&id.hash);
                                        self.heat = HeatmapStore::load(&id.hash, key_count).ok();
                                    }
                                }
                                self.confirm_reset = false;
                            }
                            if ui.button("Keep").clicked() {
                                self.confirm_reset = false;
                            }
                        } else if ui.button("Reset stats").clicked() {
                            self.confirm_reset = true;
                        }
                        if ui.button("⬇ Export CSV").clicked() {
                            self.csv_saved = self.export_heatmap_csv(&counts, layer_total).ok();
                        }
                    });
                });
                if let Some(p) = &self.csv_saved {
                    ui.horizontal(|ui| {
                        ui.colored_label(pal::GREEN, "✓ saved:");
                        if ui.link(RichText::new(p.display().to_string()).size(11.5).monospace()).clicked() {
                            reveal_in_finder(p);
                        }
                    });
                }
            });
        ui.add_space(10.0);

        // --- Canvas (left) + ranking card (right) ---------------------------
        // Both columns share the height that's actually left below the stats
        // card, so nothing runs past the window edge.
        let budget = (avail_h - 118.0).max(200.0);
        let full = ui.available_width();
        let right_w = 250.0f32.min(full * 0.3);
        let left_w = full - right_w - 14.0;
        let (g_cols, g_rows) = self.board_units();
        // Board width capped by the height budget (34px/unit legibility floor).
        let board_cap = (((budget - 76.0) / g_rows).clamp(34.0, 62.0) * g_cols + 48.0).min(left_w);
        let rank_rows = (((budget - 84.0) / 27.0) as usize).clamp(5, 20);
        let layer_def = self.layer_def(layer);
        let glow: Vec<Option<Color32>> =
            norm.iter().map(|&t| (t > 0.0).then(|| widget::heat_color(t))).collect();

        ui.horizontal_top(|ui| {
            // Keyboard canvas card + legend (centered, height-budgeted).
            ui.vertical(|ui| {
                ui.set_width(left_w);
                let pad = ((left_w - board_cap) / 2.0).max(0.0);
                ui.horizontal(|ui| {
                ui.add_space(pad);
                egui::Frame::new()
                    .fill(pal::CARD)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(14))
                    .inner_margin(egui::Margin::symmetric(14, 16))
                    .show(ui, |ui| {
                        // The frame sits in a horizontal wrapper (for the
                        // centering pad) - lay its content out vertically.
                        ui.vertical(|ui| {
                        ui.set_width(board_cap - 28.0);
                        let no_press = vec![false; key_count];
                        let kb = draw_keyboard(ui, self.geometry(), layer_def, &glow, &no_press, None, 1.0, false);
                        if let Some(i) = kb.hovered {
                            let label = layer_def
                                .and_then(|l| l.keys.get(i))
                                .map(|k| labels_for(k).tap)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| format!("key {i}"));
                            let c = counts.get(i).copied().unwrap_or(0);
                            let pct = c as f64 / layer_total.max(1) as f64 * 100.0;
                            egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new("heat_tip"), |ui| {
                                ui.label(RichText::new(label).strong());
                                ui.label(format!("{} presses · {pct:.1}%", format_thousands(c)));
                            });
                        }
                        ui.add_space(8.0);
                        // Legend: low → high gradient bar.
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("low").size(11.0).color(pal::TEXT_DIM));
                            let (bar, _) = ui.allocate_exact_size(egui::vec2(160.0, 8.0), egui::Sense::hover());
                            let steps = 32;
                            for s in 0..steps {
                                let t0 = s as f32 / steps as f32;
                                let mut seg = bar;
                                seg.min.x = bar.left() + bar.width() * t0;
                                seg.max.x = bar.left() + bar.width() * (t0 + 1.0 / steps as f32);
                                ui.painter().rect_filled(seg, egui::CornerRadius::ZERO, widget::heat_color(t0 as f64));
                            }
                            ui.label(RichText::new("high").size(11.0).color(pal::TEXT_DIM));
                        });
                        });
                    });
                });
            });
            ui.add_space(10.0);
            // Ranking card.
            ui.vertical(|ui| {
                ui.set_width(right_w);
                egui::Frame::new()
                    .fill(pal::CARD)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Ranking").strong().size(14.5).color(pal::TEXT));
                        ui.add_space(8.0);
                        if layer_total == 0 {
                            ui.label(RichText::new("No presses recorded yet for this view.").size(12.0).color(pal::TEXT_DIM));
                        }
                        let max = ranked.first().map(|(_, c)| *c).unwrap_or(1) as f32;
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.spacing_mut().interact_size.y = 18.0;
                        for (rank, (idx, count)) in ranked.iter().take(rank_rows).enumerate() {
                            let label = layer_def
                                .and_then(|l| l.keys.get(*idx))
                                .map(|k| labels_for(k).tap)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| format!("key {idx}"));
                            let pct = *count as f64 / layer_total.max(1) as f64 * 100.0;
                            ui.horizontal(|ui| {
                                ui.add_sized([18.0, 16.0], egui::Label::new(RichText::new(format!("{}", rank + 1)).size(11.0).color(pal::TEXT_DIM)));
                                ui.add_sized([40.0, 16.0], egui::Label::new(RichText::new(label).strong().size(13.0)).halign(egui::Align::LEFT));
                                perf_bar(ui, *count as f32 / max, widget::heat_color((*count as f32 / max) as f64));
                                ui.label(RichText::new(format_thousands(*count)).size(11.5).color(pal::TEXT));
                                ui.label(RichText::new(format!("· {pct:.1}%")).size(11.0).color(pal::TEXT_DIM));
                            });
                        }
                    });
            });
        });
    }

    /// FX Studio: effect library | editor | live on-screen preview.
    fn ui_fx_studio(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        // "board RGB" (sidebar) = the application panel: what the physical
        // keyboard actually runs, moved here from Tools.
        if self.fx_lib == FxLib::Apply {
            ui.label(
                RichText::new("What the board runs right now - constant background effect plus the global press reaction.")
                    .size(12.0)
                    .color(pal::TEXT_DIM),
            );
            ui.add_space(8.0);
            card(ui, "Board RGB", |ui| self.ui_rgb_effects(ui));
            return;
        }
        ui.label(
            RichText::new("Sandbox - pick an effect, tune its color, test it in the preview or on the board.")
                .size(12.0)
                .color(pal::TEXT_DIM),
        );
        ui.add_space(8.0);

        // --- Library: one category at a time (picked in the sidebar), so the
        //     list stays a single short row of chips.
        card(ui, "Library", |ui| {
            match self.fx_lib {
                FxLib::Const => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("CONSTANT").size(10.5).color(pal::TEXT_DIM));
                        for (e, label) in Effect::ALL {
                            if e == Effect::Off {
                                continue;
                            }
                            let name = label.split(" -").next().unwrap_or(label).split(" (").next().unwrap_or(label);
                            if ui.selectable_label(self.fx_sel == FxSel::Const(e), name).clicked() {
                                self.fx_sel = FxSel::Const(e);
                                self.fx_t0 = Instant::now();
                            }
                        }
                    });
                }
                FxLib::Press => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("ON PRESS").size(10.5).color(pal::TEXT_DIM));
                        for (e, label) in PressEffect::ALL {
                            if e == PressEffect::None {
                                continue;
                            }
                            let name = label.replace("This key - ", "").replace("Whole board - ", "🌐 ");
                            if ui.selectable_label(self.fx_sel == FxSel::Press(e), name).clicked() {
                                self.fx_sel = FxSel::Press(e);
                                self.fx_events.clear();
                                self.fx_t0 = Instant::now();
                            }
                        }
                    });
                }
                FxLib::Apply => unreachable!(),
                FxLib::Custom => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("CUSTOM").size(10.5).color(pal::TEXT_DIM));
                        let mut select: Option<usize> = None;
                        for (i, c) in self.custom_fx.iter().enumerate() {
                            if ui.selectable_label(self.fx_sel == FxSel::Custom(i), format!("★ {}", c.name)).clicked() {
                                select = Some(i);
                            }
                        }
                        if let Some(i) = select {
                            self.fx_sel = FxSel::Custom(i);
                            self.fx_step = 0;
                            self.fx_t0 = Instant::now();
                        }
                        if ui.button("＋ new").clicked() {
                            let n = self.custom_fx.len() + 1;
                            self.custom_fx.push(CustomFx {
                                name: format!("my effect {n}"),
                                steps: vec![FxStep { keys: Vec::new(), color: [138, 92, 246], ms: 220 }],
                            });
                            self.fx_sel = FxSel::Custom(self.custom_fx.len() - 1);
                            self.fx_step = 0;
                            self.fx_playing = false; // start in paint mode
                            self.save_custom_fx();
                        }
                    });
                }
            }
        });
        ui.add_space(8.0);

        // --- Tune + Preview: side by side when there's room, stacked when not
        //     - sized from the available width so nothing is ever cut off.
        let full = ui.available_width();
        let ed_w = 250.0;
        if full - ed_w - 16.0 >= 540.0 {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ed_w);
                    self.fx_tune_card(ui);
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(full - ed_w - 16.0);
                    self.fx_preview_card(ui);
                });
            });
        } else {
            self.fx_tune_card(ui);
            ui.add_space(8.0);
            self.fx_preview_card(ui);
        }
    }

    /// FX Studio: the tuning card - the effect's knobs plus a way to test it
    /// on the physical board. Assigning lives elsewhere (key editor / Tools).
    fn fx_tune_card(&mut self, ui: &mut egui::Ui) {
        if let FxSel::Custom(i) = self.fx_sel {
            if i < self.custom_fx.len() {
                self.fx_custom_editor(ui, i);
            } else {
                self.fx_sel = FxSel::Press(PressEffect::Ripple);
            }
            return;
        }
        card(ui, "Tune", |ui| {
            let (name, uses_color, is_press) = match self.fx_sel {
                FxSel::Const(e) => (e.label().to_string(), e.uses_color(), false),
                FxSel::Press(e) => (e.label().to_string(), e.uses_color(), true),
                FxSel::Custom(_) => unreachable!(),
            };
            ui.label(RichText::new(name).strong().size(15.0).color(pal::TEXT));
            ui.label(
                RichText::new(if is_press { "plays from a key, over your RGB" } else { "whole board, runs continuously" })
                    .size(11.5)
                    .color(pal::TEXT_DIM),
            );
            ui.add_space(8.0);
            if uses_color {
                labeled(ui, "Color", |ui| {
                    ui.color_edit_button_srgb(&mut self.fx_color);
                });
            }
            if !is_press {
                labeled(ui, "Speed", |ui| {
                    ui.add(egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false));
                });
                labeled(ui, "Brightness", |ui| {
                    ui.add(egui::Slider::new(&mut self.fx_bright, 0.05..=1.0).show_value(false));
                });
            }
            if is_press {
                ui.add_space(8.0);
                let on = self.connected.is_some();
                if ui
                    .add_enabled(on, egui::Button::new(RichText::new("⚡ Test on keyboard").color(Color32::WHITE)).fill(pal::VIOLET))
                    .clicked()
                {
                    if let (FxSel::Press(e), Ok(mut a)) = (self.fx_sel, self.anim.lock()) {
                        let now = Instant::now();
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos() as u64)
                            .unwrap_or(0);
                        // Fire from a central key; board-wide effects ignore it.
                        a.events.push(FxEvent { key: 16, effect: e, color: self.fx_color, at: now, seed, seq: None });
                    }
                }
                if !on {
                    ui.label(RichText::new("connect the keyboard to test on it").size(11.0).color(pal::TEXT_DIM));
                }
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("USE IT").size(10.5).color(pal::TEXT_DIM));
            ui.label(
                RichText::new("per key: Live → click a key → On press\nwhole board: Tools → RGB effects")
                    .size(11.5)
                    .color(pal::TEXT_MUTED),
            );
        });
    }

    /// FX Studio: the step-sequencer editor for a user-built effect. Paint
    /// keys in the preview, duplicate the step, nudge it a row up - repeat.
    fn fx_custom_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        card(ui, "Sequence", |ui| {
            let mut dirty = false;
            let mut name = self.custom_fx[i].name.clone();
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY)).changed() {
                self.custom_fx[i].name = name;
                dirty = true;
            }
            ui.add_space(6.0);

            // A sequence loaded from a hand-edited config could have no steps;
            // seed one so the per-step indexing below can't panic.
            if self.custom_fx[i].steps.is_empty() {
                self.custom_fx[i].steps.push(FxStep { keys: Vec::new(), color: [138, 92, 246], ms: 220 });
            }
            // Step chips: select the one being painted; ＋ duplicates it.
            let n_steps = self.custom_fx[i].steps.len();
            self.fx_step = self.fx_step.min(n_steps.saturating_sub(1));
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("STEPS").size(10.5).color(pal::TEXT_DIM));
                for s in 0..n_steps {
                    if ui.selectable_label(self.fx_step == s, format!("{}", s + 1)).clicked() {
                        self.fx_step = s;
                        self.fx_playing = false;
                    }
                }
                if ui.button("＋").on_hover_text("duplicate this step (then nudge it)").clicked() {
                    let copy = self.custom_fx[i].steps[self.fx_step].clone();
                    self.custom_fx[i].steps.insert(self.fx_step + 1, copy);
                    self.fx_step += 1;
                    self.fx_playing = false;
                    dirty = true;
                }
            });
            ui.add_space(4.0);

            // Active step controls - compact rows so the card fits the window.
            let s = self.fx_step;
            let step = &mut self.custom_fx[i].steps[s];
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("step {} · {} keys", s + 1, step.keys.len())).size(12.0).color(pal::TEXT_MUTED));
                dirty |= ui.color_edit_button_srgb(&mut step.color).changed();
                dirty |= ui
                    .add(egui::DragValue::new(&mut step.ms).range(40..=2000).speed(10).suffix(" ms"))
                    .changed();
            });
            ui.horizontal(|ui| {
                let mut mv: Option<(f32, f32)> = None;
                if ui.button("←").on_hover_text("nudge left").clicked() { mv = Some((-1.0, 0.0)); }
                if ui.button("↑").on_hover_text("nudge up").clicked() { mv = Some((0.0, -1.0)); }
                if ui.button("↓").on_hover_text("nudge down").clicked() { mv = Some((0.0, 1.0)); }
                if ui.button("→").on_hover_text("nudge right").clicked() { mv = Some((1.0, 0.0)); }
                if let Some((dx, dy)) = mv {
                    self.shift_step(i, s, dx, dy);
                }
                if ui.button("clear").on_hover_text("unpaint all keys of this step").clicked() {
                    self.custom_fx[i].steps[s].keys.clear();
                    dirty = true;
                }
                if n_steps > 1 && ui.button("✕").on_hover_text("delete this step").clicked() {
                    self.custom_fx[i].steps.remove(s);
                    self.fx_step = self.fx_step.saturating_sub(1);
                    dirty = true;
                }
            });
            labeled(ui, "tempo", |ui| {
                ui.add(egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false));
            });
            ui.add_space(6.0);

            let on = self.connected.is_some();
            if ui
                .add_enabled(on, egui::Button::new(RichText::new("⚡ Test 5 s on keyboard").color(Color32::WHITE)).fill(pal::VIOLET))
                .clicked()
            {
                if let Ok(mut a) = self.anim.lock() {
                    let prev = if a.effect == Effect::Custom { Effect::Off } else { a.effect };
                    a.custom = self.custom_fx[i].steps.clone();
                    a.custom_name = self.custom_fx[i].name.clone();
                    a.speed = self.fx_speed;
                    a.effect = Effect::Custom;
                    self.fx_board_restore = Some((Instant::now() + Duration::from_secs(5), prev));
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Use: ▶ board RGB → constant effect → ★").size(11.0).color(pal::TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(RichText::new("🗑").size(11.5)).on_hover_text("delete this effect").clicked() {
                        self.custom_fx.remove(i);
                        self.fx_sel = FxSel::Press(PressEffect::Ripple);
                        dirty = true;
                    }
                });
            });
            if dirty {
                self.save_custom_fx();
            }
        });
    }

    /// FX Studio: the preview card (pure `compute`, never touches the LEDs).
    fn fx_preview_card(&mut self, ui: &mut egui::Ui) {
        // Fit the preview into the window's remaining height (the widget keeps
        // its own legible floor), so the studio fits without scrolling.
        let (cols, rows) = self.board_units();
        // Reserve room for the card header/margins and the play-controls row.
        let budget = (ui.ctx().screen_rect().height() - ui.cursor().top() - 118.0).max(170.0);
        card(ui, "Preview", |ui| {
            // Match the widget's own legible floor (34px/unit) so the container
            // is never narrower than what draw_keyboard will actually paint.
            let w = ui.available_width().min((budget / rows).max(34.0) * cols + 24.0);
            let pad = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    self.fx_preview(ui);
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if self.fx_playing { "⏸ Pause" } else { "▶ Play" };
                if ui.button(label).clicked() {
                    self.fx_playing = !self.fx_playing;
                    self.fx_t0 = Instant::now();
                }
                let hint = match self.fx_sel {
                    FxSel::Custom(_) if !self.fx_playing => format!("painting step {} - click keys to toggle them", self.fx_step + 1),
                    FxSel::Custom(_) => "playing the loop - pause to paint · clicks still paint".to_string(),
                    _ => "click keys in the preview to fire the effect".to_string(),
                };
                ui.label(RichText::new(hint).size(11.0).color(pal::TEXT_DIM));
            });
        });
    }

    /// The FX Studio live preview: renders the engine's pure `compute` on the
    /// on-screen keyboard - never touches the physical LEDs.
    fn fx_preview(&mut self, ui: &mut egui::Ui) {
        let geo = self.geometry();
        let n = geo.len();
        let now = Instant::now();
        // Auto-fire press effects periodically so the preview animates itself.
        if self.fx_playing {
            if let FxSel::Press(e) = self.fx_sel {
                self.fx_events.retain(|ev| now.duration_since(ev.at).as_secs_f32() < 1.5);
                if now.duration_since(self.fx_last_fire) > Duration::from_millis(1400) {
                    self.fx_last_fire = now;
                    let key = [16usize, 30, 8, 42][(now.elapsed().subsec_nanos() as usize) % 4];
                    self.fx_events.push(FxEvent { key: key % n, effect: e, color: self.fx_color, at: now, seed: now.elapsed().subsec_nanos() as u64, seq: None });
                }
            }
        }

        let base = self.glow_rgb(self.view_layer);
        let t = self.fx_t0.elapsed().as_secs_f32();
        let frame = match self.fx_sel {
            FxSel::Const(e) => rgb_anim::compute(e, self.fx_color, self.fx_speed, self.fx_bright, if self.fx_playing { t } else { 0.35 }, &base, &[], &[], geo),
            FxSel::Press(_) => rgb_anim::compute(Effect::Off, [0, 0, 0], 1.0, 1.0, t, &base, &self.fx_events, &[], geo),
            FxSel::Custom(ci) => {
                let steps = self.custom_fx.get(ci).map(|c| c.steps.clone()).unwrap_or_default();
                if self.fx_playing {
                    rgb_anim::compute(Effect::Custom, [0, 0, 0], self.fx_speed, self.fx_bright, t, &base, &[], &steps, geo)
                } else {
                    // Paint mode: the active step bright, the previous step as
                    // a dim onion-skin so nudged copies line up visually.
                    let mut fr = vec![[0u8, 0, 0]; n];
                    if self.fx_step > 0 {
                        if let Some(p) = steps.get(self.fx_step - 1) {
                            for &k in &p.keys {
                                if (k as usize) < n {
                                    fr[k as usize] = [p.color[0] / 4, p.color[1] / 4, p.color[2] / 4];
                                }
                            }
                        }
                    }
                    if let Some(sdef) = steps.get(self.fx_step) {
                        for &k in &sdef.keys {
                            if (k as usize) < n {
                                fr[k as usize] = sdef.color;
                            }
                        }
                    }
                    fr
                }
            }
        };
        let glow: Vec<Option<Color32>> = frame
            .iter()
            .map(|c| (*c != [0, 0, 0]).then(|| Color32::from_rgb(c[0], c[1], c[2])))
            .collect();
        let no_press = vec![false; n];
        let layer = self.layer_def(self.view_layer);
        let kb = draw_keyboard(ui, geo, layer, &glow, &no_press, None, 1.0, false);
        // Clicking the preview: fire the press effect, or paint the custom step.
        if let Some(i) = kb.clicked {
            match self.fx_sel {
                FxSel::Press(e) => {
                    self.fx_events.push(FxEvent { key: i, effect: e, color: self.fx_color, at: now, seed: now.elapsed().subsec_nanos() as u64, seq: None });
                }
                FxSel::Custom(ci) => {
                    let step = self.fx_step;
                    if let Some(st) = self.custom_fx.get_mut(ci).and_then(|c| c.steps.get_mut(step)) {
                        match st.keys.iter().position(|&k| k as usize == i) {
                            Some(p) => {
                                st.keys.remove(p);
                            }
                            None => st.keys.push(i as u16),
                        }
                        self.save_custom_fx();
                    }
                }
                FxSel::Const(_) => {}
            }
        }
        if self.fx_playing {
            ui.ctx().request_repaint();
        }
    }

    /// Write the current heatmap view to ~/Downloads as a CSV.
    fn export_heatmap_csv(&self, counts: &[u64], layer_total: u64) -> anyhow::Result<std::path::PathBuf> {
        use std::io::Write as _;
        let layer = self.heat_layer.unwrap_or(self.view_layer);
        let layer_def = self.layer_def(layer);
        let scope = match self.heat_layer {
            None => "all-layers".to_string(),
            Some(n) => format!("layer{n}"),
        };
        let dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("keyjitsu-heatmap-{scope}-{stamp}.csv"));
        let mut f = std::fs::File::create(&path)?;
        writeln!(f, "rank,key_index,label,presses,percent")?;
        let mut ranked: Vec<(usize, u64)> = counts.iter().copied().enumerate().collect();
        ranked.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (rank, (idx, count)) in ranked.iter().enumerate() {
            let label = layer_def
                .and_then(|l| l.keys.get(*idx))
                .map(|k| labels_for(k).tap)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("key {idx}"));
            let pct = *count as f64 / layer_total.max(1) as f64 * 100.0;
            // Quote labels - some are commas/quotes themselves.
            writeln!(f, "{},{},\"{}\",{},{:.2}", rank + 1, idx, label.replace('"', "\"\""), count, pct)?;
        }
        Ok(path)
    }

    fn ui_tools(&mut self, ui: &mut egui::Ui) {
        // Centered settings-dashboard column (max ~1000px on wide screens).
        let full = ui.available_width();
        let w = full.min(1000.0);
        let x = ((full - w) / 2.0).max(12.0);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(x);
            ui.vertical(|ui| {
                ui.set_width(w - 24.0);
                ui.label(RichText::new("Settings").strong().size(21.0).color(pal::TEXT));
                ui.label(RichText::new("Firmware, guard, profiles and app housekeeping.").size(12.5).color(pal::TEXT_DIM));
                ui.add_space(14.0);

                let fw_pill = if self.env.is_ready() {
                    ("Ready".to_string(), pal::GREEN)
                } else {
                    ("Setup required".to_string(), pal::AMBER)
                };
                tool_card(ui, "⚙", "Firmware build", "Remap keys and compile firmware 100% locally with QMK - no login, no cloud.", Some(fw_pill), self, |ui, app| app.ui_localbuild(ui));

                let guard_pill = if self.guard.is_some() {
                    ("Active".to_string(), pal::GREEN)
                } else if self.guard_enabled {
                    ("Waiting for keyboard".to_string(), pal::AMBER)
                } else {
                    ("Off".to_string(), pal::TEXT_DIM)
                };
                tool_card(ui, "🔒", "Keyboard guard", "Disables the Mac's built-in keyboard while the ZSA board is connected.", Some(guard_pill), self, |ui, app| app.ui_guard(ui));

                let app_pill = if autostart_enabled() {
                    ("Autostart on".to_string(), pal::GREEN)
                } else {
                    ("Manual start".to_string(), pal::TEXT_DIM)
                };
                tool_card(ui, "🚀", "App", "Launch keyjitsu automatically when you log in.", Some(app_pill), self, |ui, app| app.ui_app_card(ui));

                // Reference material, not keyboard state: lives here rather
                // than in the main menu so it can't be mistaken for the keys
                // on the board.
                tool_card(
                    ui,
                    "📚",
                    "Shortcut library",
                    "A reference list of common shortcuts (macOS, editors, terminals, tools) to borrow from when planning a layer. Not what is on your keyboard: edit keys in Live.",
                    Some(("Reference".to_string(), pal::TEXT_DIM)),
                    self,
                    |ui, app| {
                        egui::CollapsingHeader::new("Browse the library")
                            .id_salt("shortcut_library")
                            .default_open(false)
                            .show(ui, |ui| app.ui_shortcuts(ui));
                    },
                );
                ui.add_space(10.0);
            });
        });
    }

    /// Performance as its own page (same card style as Settings).
    fn ui_perf_page(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_width();
        let w = full.min(1000.0);
        let x = ((full - w) / 2.0).max(12.0);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(x);
            ui.vertical(|ui| {
                ui.set_width(w - 24.0);
                let pill = {
                    let c = self.perf_live;
                    (format!("{c:.1}% CPU"), if c > 25.0 { pal::AMBER } else { pal::GREEN })
                };
                tool_card(ui, "📈", "Performance", "keyjitsu samples its own CPU and tags each sample with what it was doing.", Some(pill), self, |ui, app| app.ui_performance(ui));
            });
        });
    }

    /// Autolayer as its own page.
    fn ui_auto_page(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_width();
        let w = full.min(1000.0);
        let x = ((full - w) / 2.0).max(12.0);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(x);
            ui.vertical(|ui| {
                ui.set_width(w - 24.0);
                let pill = if self.autolayer_enabled {
                    (format!("On · {} rule{}", self.rules.len(), if self.rules.len() == 1 { "" } else { "s" }), pal::GREEN)
                } else {
                    ("Off".to_string(), pal::TEXT_DIM)
                };
                tool_card(ui, "⇆", "Autolayer", "Switches layers automatically based on the frontmost app.", Some(pill), self, |ui, app| app.ui_autolayer(ui));
            });
        });
    }

    /// Re-seed the running app from a freshly loaded config (profile switch).
    fn apply_config(&mut self, cfg: config::Config) {
        self.rules = cfg.autolayer_rules.clone();
        self.peek = cfg.peek.clone();
        self.show_cpu_header = cfg.show_cpu_header;
        self.auto_update_check = !cfg.skip_update_check_on_start;
        self.guard_enabled = cfg.guard_enabled;
        self.autolayer_enabled = cfg.autolayer_enabled;
        self.overlay_chord = if cfg.overlay_chord.is_empty() {
            cfg.overlay_trigger.map(|t| vec![t]).unwrap_or_default()
        } else {
            cfg.overlay_chord.clone()
        };
        self.hidden_shortcuts = cfg.hidden_shortcuts.clone();
        self.custom_fx = cfg.custom_fx.clone();
        self.custom_shortcuts = cfg.custom_shortcuts.clone();
        if let Some(hash) = self.layout_hash.clone() {
            self.hydrate_glow(&hash);
            self.hydrate_key_fx(&hash);
            // Reload this layout's custom layers from the new profile - else
            // the previous profile's layers linger and a later edit would
            // overwrite the target profile's layers.
            self.hydrate_custom_layers(&hash);
            self.hydrate_staged(&hash);
        } else {
            self.custom_layers.clear();
            self.rebuild_synth_layers();
        }
        if let Ok(mut a) = self.anim.lock() {
            let r = &cfg.rgb;
            a.effect = r.effect;
            a.color = r.color;
            a.speed = r.speed;
            a.brightness = r.brightness;
            a.press_effect = r.press_effect;
            a.press_color = r.press_color;
            a.custom_name = r.custom_name.clone();
            a.custom = cfg.custom_fx.iter().find(|c| c.name == r.custom_name).map(|c| c.steps.clone()).unwrap_or_default();
            if a.effect == Effect::Custom && a.custom.is_empty() {
                a.effect = Effect::Off;
            }
        }
        // Restart the autolayer watcher so it picks up the new rules.
        self.autolayer = None;
        self.needs_push = true;
    }

    fn ui_app_card(&mut self, ui: &mut egui::Ui) {
        let mut on = autostart_enabled();
        if toggle_row(ui, "Start keyjitsu at login (GUI)", &mut on) {
            self.autostart_error = set_autostart(on).err().map(|e| format!("{e:#}"));
        }
        if let Some(e) = &self.autostart_error {
            ui.colored_label(pal::RED, format!("autostart failed: {e}"));
        }
        if let Some(p) = autostart_plist() {
            ui.label(RichText::new(format!("LaunchAgent: {}", p.display())).size(11.0).color(pal::TEXT_DIM));
        }
        ui.label(
            RichText::new("Points at this binary - re-toggle after moving/rebuilding the app to refresh the path.")
                .size(11.0)
                .color(pal::TEXT_DIM),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("Updates").strong().color(pal::TEXT));
        ui.label(
            RichText::new(format!(
                "You are on v{}. Checking asks GitHub for the latest release tag. Nothing is downloaded or installed.",
                env!("CARGO_PKG_VERSION")
            ))
            .size(11.5)
            .color(pal::TEXT_DIM),
        );
        if toggle_row(ui, "Check for updates when keyjitsu starts", &mut self.auto_update_check) {
            let mut cfg = config::load();
            cfg.skip_update_check_on_start = !self.auto_update_check;
            let _ = config::save(&cfg);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.update_rx.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Check for updates")).clicked() {
                self.update_rx = Some(spawn_update_check());
                self.update_state = None;
            }
            if busy {
                ui.spinner();
                ui.label(RichText::new("checking…").size(11.5).color(pal::TEXT_DIM));
            }
        });
        match &self.update_state {
            Some(UpdateCheck::UpToDate) => {
                ui.colored_label(pal::GREEN, "You are up to date.");
            }
            Some(UpdateCheck::Available { tag, url }) => {
                let (tag, url) = (tag.clone(), url.clone());
                ui.colored_label(pal::AMBER, format!("{tag} is available."));
                ui.horizontal(|ui| {
                    if ui.button("Open release page").clicked() {
                        let _ = std::process::Command::new("open").arg(&url).spawn();
                    }
                    ui.label(RichText::new("then rebuild: ").size(11.5).color(pal::TEXT_DIM));
                    ui.code("scripts/bundle.sh --install");
                });
            }
            Some(UpdateCheck::Error(e)) => {
                ui.colored_label(pal::RED, format!("could not check: {e}"));
            }
            None => {}
        }
    }

    fn ui_performance(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let c = self.perf_live;
            let col = if c > 25.0 { pal::AMBER } else { pal::GREEN };
            ui.label("app CPU:");
            ui.label(RichText::new(format!("{c:.1}%")).strong().size(16.0).color(col));
            ui.weak("keyjitsu only · % of one core");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("state: {}", self.perf_state().label()));
            });
        });
        ui.add_space(4.0);
        if toggle_row(ui, "Show CPU in the header (always visible)", &mut self.show_cpu_header) {
            let mut cfg = config::load();
            cfg.show_cpu_header = self.show_cpu_header;
            let _ = config::save(&cfg);
        }
        ui.add_space(6.0);

        if let Some(run) = &self.perf_run {
            let now = Instant::now();
            let (phase, remaining) = if run.phases.is_empty() {
                ("5-min sample".to_string(), run.end_at.saturating_duration_since(now))
            } else {
                (
                    format!("compare · {}", run.phases[run.phase_i].label),
                    run.phase_until.saturating_duration_since(now),
                )
            };
            let n = run.samples.len();
            let mut stop = false;
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("sampling - {phase}")).color(pal::VIOLET_HI));
                ui.weak(format!("{}s left · {n} samples", remaining.as_secs()));
                stop = ui.button("stop").clicked();
            });
            if stop {
                self.finish_perf();
            }
        } else {
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("▶ Start 5-min sample").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                    self.start_perf_observe();
                }
                if ui
                    .button("⚖ Compare modes")
                    .on_hover_text("cycles idle → layout RGB → rainbow → rainbow+peek (~12s each) and measures CPU in each")
                    .clicked()
                {
                    self.start_perf_compare();
                }
            });
        }

        if let Some(sum) = &self.perf_last {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new("Last results").strong());
            ui.weak(format!("{} samples over {}s", sum.n, sum.secs));
            ui.horizontal(|ui| {
                ui.label(format!("overall avg {:.1}%", sum.avg));
                ui.separator();
                ui.label(format!("peak {:.1}%", sum.max));
            });
            ui.add_space(6.0);
            let scale = sum.modes.iter().map(|m| m.max).fold(1.0f32, f32::max);
            egui::Grid::new("perf_modes").num_columns(3).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
                ui.strong("mode");
                ui.strong("avg");
                ui.strong("peak");
                ui.end_row();
                for m in &sum.modes {
                    ui.label(&m.label).on_hover_text(format!("{} samples", m.n));
                    // avg as a small bar + number
                    ui.horizontal(|ui| {
                        perf_bar(ui, m.avg / scale, pal::VIOLET);
                        ui.label(format!("{:.1}%", m.avg));
                    });
                    ui.horizontal(|ui| {
                        perf_bar(ui, m.max / scale, pal::AMBER);
                        ui.label(format!("{:.1}%", m.max));
                    });
                    ui.end_row();
                }
            });
        }
    }

    fn ui_rgb_effects(&mut self, ui: &mut egui::Ui) {
        if self.connected.is_none() {
            ui.weak("connect the keyboard to use effects");
            return;
        }

        let Ok(mut a) = self.anim.lock() else { return };
        let wide = 220.0;
        let before = (a.effect, a.color, a.speed, a.brightness, a.press_effect, a.press_color, a.custom_name.clone());

        labeled(ui, "constant effect", |ui| {
            let sel_text = if a.effect == Effect::Custom {
                format!("★ {}", a.custom_name)
            } else {
                a.effect.label().to_string()
            };
            egui::ComboBox::from_id_salt("rgbfx")
                .width(wide)
                .selected_text(sel_text)
                .show_ui(ui, |ui| {
                    for (e, label) in Effect::ALL {
                        ui.selectable_value(&mut a.effect, e, label);
                    }
                    if !self.custom_fx.is_empty() {
                        ui.separator();
                    }
                    for c in &self.custom_fx {
                        let is = a.effect == Effect::Custom && a.custom_name == c.name;
                        if ui.selectable_label(is, format!("★ {}", c.name)).clicked() {
                            a.effect = Effect::Custom;
                            a.custom = c.steps.clone();
                            a.custom_name = c.name.clone();
                        }
                    }
                });
        });
        if a.effect.uses_color() {
            labeled(ui, "effect color", |ui| {
                ui.color_edit_button_srgb(&mut a.color);
            });
        }
        if a.effect != Effect::Off {
            labeled(ui, "speed", |ui| {
                ui.add_sized([wide, 20.0], egui::Slider::new(&mut a.speed, 0.2..=3.0).show_value(false));
            });
        }
        labeled(ui, "brightness", |ui| {
            ui.add_sized([wide, 20.0], egui::Slider::new(&mut a.brightness, 0.05..=1.0).show_value(false));
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new("On key press").strong());
        ui.weak("Instead of a static color, play an effect from the key you press.");
        ui.add_space(2.0);
        labeled(ui, "press effect", |ui| {
            egui::ComboBox::from_id_salt("pressfx")
                .width(wide)
                .selected_text(a.press_effect.label())
                .show_ui(ui, |ui| {
                    for (e, label) in rgb_anim::PressEffect::ALL {
                        ui.selectable_value(&mut a.press_effect, e, label);
                    }
                });
        });
        if a.press_effect != rgb_anim::PressEffect::None {
            labeled(ui, "press color", |ui| {
                ui.color_edit_button_srgb(&mut a.press_color);
            });
        }

        if a.effect != Effect::Off {
            // A constant effect owns the LEDs; disable glow sync to avoid a fight.
            self.sync_glow = false;
        }

        // Persist the board RGB state so restarts (and profiles) restore it.
        let after = (a.effect, a.color, a.speed, a.brightness, a.press_effect, a.press_color, a.custom_name.clone());
        if after != before {
            let mut cfg = config::load();
            cfg.rgb = config::RgbState {
                effect: a.effect,
                color: a.color,
                speed: a.speed,
                brightness: a.brightness,
                press_effect: a.press_effect,
                press_color: a.press_color,
                custom_name: a.custom_name.clone(),
            };
            let _ = config::save(&cfg);
        }
    }

    fn ui_guard(&mut self, ui: &mut egui::Ui) {
        #[cfg(target_os = "macos")]
        {
            if toggle_row(ui, "Disable built-in keyboard while connected", &mut self.guard_enabled) {
                let mut cfg = config::load();
                cfg.guard_enabled = self.guard_enabled;
                let _ = config::save(&cfg);
            }
            if let Some(g) = &self.guard {
                ui.colored_label(pal::GREEN, format!("🔒 disabled: {}", g.describe()));
            }
            if let Some(e) = &self.guard_error {
                ui.colored_label(pal::RED, e);
            }
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                ui.weak("Keys are remapped to no-ops with hidutil (no special permission); restored on toggle-off, disconnect, quit - and by any reboot. If keyjitsu is force-killed first, restore by hand:");
                let mut cmd = crate::macos_kb::restore_command();
                ui.add(
                    egui::TextEdit::singleline(&mut cmd)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
            });
        }
        #[cfg(not(target_os = "macos"))]
        ui.weak("(macOS only)");
    }

    fn ui_autolayer(&mut self, ui: &mut egui::Ui) {
        #[cfg(target_os = "macos")]
        {
            if toggle_row(ui, "Enable autolayer", &mut self.autolayer_enabled) {
                let mut cfg = config::load();
                cfg.autolayer_enabled = self.autolayer_enabled;
                let _ = config::save(&cfg);
            }
            // Live feedback: what the frontmost app is and whether a rule hits.
            if self.autolayer_enabled {
                if let Some(front) = crate::cmd_autolayer::frontmost_bundle_id() {
                    let hit = self.rules.iter().find(|r| front.contains(r.bundle.as_str()));
                    ui.horizontal(|ui| {
                        ui.weak("frontmost:");
                        ui.label(RichText::new(&front).size(11.5).monospace().color(pal::TEXT_MUTED));
                        match hit {
                            Some(r) => ui.colored_label(pal::GREEN, format!("→ {}", self.layer_name(r.layer))),
                            None => ui.weak("→ base"),
                        };
                    });
                }
            }
            ui.add_space(4.0);

            let names: Vec<String> = (0..self.layer_count()).map(|n| self.layer_name(n)).collect();
            let mut remove: Option<usize> = None;
            egui::Grid::new("rules").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.strong("app (bundle id contains)");
                ui.strong("switch to layer");
                ui.strong("");
                ui.end_row();
                for (i, rule) in self.rules.iter_mut().enumerate() {
                    if ui.add(egui::TextEdit::singleline(&mut rule.bundle).desired_width(240.0)).changed() {
                        self.rules_dirty = true;
                    }
                    egui::ComboBox::from_id_salt(("rule_layer", i))
                        .selected_text(names.get(rule.layer as usize).cloned().unwrap_or_else(|| format!("Layer {}", rule.layer)))
                        .show_ui(ui, |ui| {
                            for (n, nm) in names.iter().enumerate() {
                                if ui.selectable_value(&mut rule.layer, n as u8, format!("{n} · {nm}")).changed() {
                                    self.rules_dirty = true;
                                }
                            }
                        });
                    if ui.button("✕").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                self.rules.remove(i);
                self.rules_dirty = true;
            }
            ui.add_space(6.0);

            // Add a rule from a running app (no need to know bundle ids).
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("add_running")
                    .selected_text("＋ Add from running app…")
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (name, bundle) in crate::cmd_autolayer::running_apps() {
                            if ui.selectable_label(false, format!("{name}  ·  {bundle}")).clicked() {
                                self.rules.push(AutolayerRule { bundle, layer: 1 });
                                self.rules_dirty = true;
                            }
                        }
                    });
                if ui.button("＋ blank rule").clicked() {
                    self.rules.push(AutolayerRule { bundle: String::new(), layer: 1 });
                    self.rules_dirty = true;
                }
            });

            if self.rules_dirty {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Save & apply").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                        let mut cfg = config::load();
                        cfg.autolayer_rules = self.rules.clone();
                        if config::save(&cfg).is_ok() {
                            self.rules_dirty = false;
                            // Restart the watcher so it picks up the new rules.
                            self.autolayer = None;
                        }
                    }
                    status_pill(ui, "unsaved rules", pal::AMBER);
                });
            }
        }
        #[cfg(not(target_os = "macos"))]
        ui.weak("(macOS only)");
    }

    /// The Peek tab: a clean, vertically-stacked settings page for the layer
    /// minimap. Changes preview live.
    fn ui_peek_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("👁").size(22.0));
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Layer peek").strong().size(20.0).color(pal::TEXT));
                ui.label(
                    RichText::new("A transparent, click-through minimap that flashes when a layer activates.")
                        .size(12.5)
                        .color(pal::TEXT_MUTED),
                );
            });
        });
        ui.add_space(12.0);

        let mut c = self.peek.clone();
        // Preview on top (full width), then options left / appearance+position
        // right - the settings lists stay short and nothing gets cut.
        self.peek_preview_card(ui, &mut c);
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            self.peek_settings_card(&mut cols[0], &mut c);
            self.peek_appearance_card(&mut cols[1], &mut c);
            cols[1].add_space(8.0);
            self.peek_position_card(&mut cols[1], &mut c);
        });

        if c != self.peek {
            self.peek = c.clone();
            if c.enabled {
                self.arm_preview(&c, 2500);
            } else {
                self.peek_until = None;
            }
            let mut cfg = config::load();
            cfg.peek = c;
            let _ = config::save(&cfg);
        }

    }

    fn peek_settings_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Options", |ui| {
            // Shortcut lives with the options (a bound Voyager key shows the
            // minimap while held, independent of the auto-peek toggles).
            group_header(ui, "Shortcut", "");
            self.peek_shortcut_row(ui);

            group_header(ui, "Options", "");
            toggle_row(ui, "Enable layer peek", &mut c.enabled);
            ui.add_enabled_ui(c.enabled, |ui| {
                toggle_row(ui, "Only outside the base layer", &mut c.only_non_base);
                toggle_row(ui, "Show background panel", &mut c.show_background);
                toggle_row(ui, "Black & white (high contrast)", &mut c.monochrome);
                toggle_row(ui, "Show layer name", &mut c.show_layer_name);
                toggle_row(ui, "Show key legends", &mut c.show_legends);
                toggle_row(ui, "Combo: show recent presses (hold, double-tap…)", &mut c.show_combo);
                ui.add_enabled_ui(c.show_combo, |ui| {
                    toggle_row(ui, "   ↳ measure: show timings (ms) on the chips", &mut c.show_combo_ms);
                });
            });
        });
    }

    /// Right column: timing + look of the minimap.
    fn peek_appearance_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Appearance", |ui| {
            let wide = 220.0;
            ui.add_enabled_ui(c.enabled, |ui| {
                labeled(ui, "Show for", |ui| {
                    ui.add_sized([wide, 20.0], egui::Slider::new(&mut c.duration_ms, 300..=5000).suffix(" ms"));
                });
                labeled(ui, "Transparency", |ui| {
                    ui.add_sized([wide, 20.0], egui::Slider::new(&mut c.opacity, 0.08..=1.0).show_value(false));
                });
                labeled(ui, "Size", |ui| {
                    ui.add_sized([wide, 20.0], egui::Slider::new(&mut c.scale, 0.5..=1.6).show_value(false));
                });
                labeled(ui, "Accent color", |ui| {
                    ui.color_edit_button_srgb(&mut c.accent);
                });
            });
        });
    }

    /// Position controls in their own card (right column) so the settings
    /// list stays short.
    fn peek_position_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Position", |ui| {
            let wide = 220.0;
            ui.add_enabled_ui(c.enabled, |ui| {
                labeled(ui, "Monitor", |ui| {
                    self.peek_monitor_combo(ui, &mut c.monitor);
                });
                ui.add_space(4.0);
                labeled(ui, "Anchor", |ui| {
                    position_grid(ui, &mut c.valign, &mut c.halign);
                });
                ui.add_space(4.0);
                labeled(ui, "Nudge X", |ui| {
                    ui.add_sized([wide, 20.0], egui::Slider::new(&mut c.offset[0], -1200.0..=1200.0).suffix(" px"));
                });
                labeled(ui, "Nudge Y", |ui| {
                    ui.add_sized([wide, 20.0], egui::Slider::new(&mut c.offset[1], -1200.0..=1200.0).suffix(" px"));
                });
            });
        });
    }

    /// Name a chord like "⌥ + Spc" from layer-0 legends.
    fn chord_label(&self, chord: &[[u8; 2]]) -> String {
        chord
            .iter()
            .map(|&[r, c]| {
                self.geometry()
                    .key_index(r, c)
                    .and_then(|i| self.layer_def(0).and_then(|l| l.keys.get(i)).map(|k| labels_for(k).tap))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("r{r}c{c}"))
            })
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// One-line bind/rebind/clear row for the minimap shortcut combo.
    fn peek_shortcut_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.binding_overlay {
                if self.binding_draft.is_empty() {
                    ui.colored_label(pal::AMBER, "press the key or combo on the Voyager (release = save)…");
                } else {
                    ui.colored_label(pal::AMBER, format!("combo: {} - release to save", self.chord_label(&self.binding_draft.clone())));
                }
                if ui.button("cancel").clicked() {
                    self.binding_overlay = false;
                    self.binding_draft.clear();
                }
            } else if !self.overlay_chord.is_empty() {
                let label = self.chord_label(&self.overlay_chord.clone());
                ui.label(RichText::new(format!("hold {label} → show minimap")).color(pal::TEXT));
                if ui.button("rebind").clicked() {
                    self.binding_overlay = true;
                    self.binding_draft.clear();
                }
                if ui.button("✕ clear").clicked() {
                    self.overlay_chord.clear();
                    let mut cfg = config::load();
                    cfg.overlay_chord.clear();
                    cfg.overlay_trigger = None;
                    let _ = config::save(&cfg);
                }
            } else {
                ui.weak("no shortcut");
                if ui.button("＋ bind a key or combo").clicked() {
                    self.binding_overlay = true;
                    self.binding_draft.clear();
                }
            }
        });
    }

    /// Right column: a live preview of the peek over a transparency checkerboard.
    fn peek_preview_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Preview", |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 185.0),
                egui::Sense::hover(),
            );
            draw_checkerboard(ui.painter(), rect);
            self.render_peek_into(ui, rect.shrink(14.0), c);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("📌 Keep preview visible").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                    let snap = c.clone();
                    self.arm_preview(&snap, 5000);
                }
                if ui.button("Reset to defaults").clicked() {
                    // Preserve the chosen monitor/offset, reset the rest.
                    let (monitor, offset) = (c.monitor, c.offset);
                    *c = PeekConfig { monitor, offset, ..PeekConfig::default() };
                }
            });
            ui.weak("Shown over a checkerboard to represent your desktop showing through.");
        });
    }

    /// Draw the peek's card + minimap inside `rect` (for the inline preview).
    fn render_peek_into(&self, ui: &mut egui::Ui, rect: egui::Rect, c: &PeekConfig) {
        let layer = self.active_layer.max(if self.layer_count() > 1 { 1 } else { 0 });
        let geo = self.geometry();
        let glow = self.glow_colors(layer);
        let legends = if c.show_legends { self.layer_def(layer) } else { None };
        let title = self
            .layer_def(layer)
            .and_then(|l| l.title.clone())
            .unwrap_or_else(|| format!("Layer {layer}"));
        let a = (c.opacity.clamp(0.08, 1.0) * 255.0) as u8;
        let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);

        let mut child = ui.new_child(
            egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Center)),
        );
        // Fit the minimap into the rect: unit from the HEIGHT budget (the board
        // is PEEK_BOARD_UNITS_TALL units tall incl. the rotated thumbs), width follows.
        let header_h = if c.show_layer_name { 32.0 } else { 0.0 };
        let unit_fit = ((rect.height() - 24.0 - header_h) / PEEK_BOARD_UNITS_TALL)
            .min((rect.width() - 44.0) / PEEK_BOARD_UNITS_WIDE);
        let kb_w = (unit_fit * PEEK_BOARD_UNITS_WIDE + 24.0).max(120.0);
        let card_fill = if c.show_background {
            Color32::from_rgba_unmultiplied(17, 18, 24, a)
        } else {
            Color32::TRANSPARENT
        };
        egui::Frame::new()
            .fill(card_fill)
            .stroke(if c.show_background {
                egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), (a as f32 * 0.6) as u8))
            } else {
                egui::Stroke::NONE
            })
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(10))
            .show(&mut child, |ui| {
                if c.show_layer_name {
                    ui.horizontal(|ui| {
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), a))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(7, 3))
                            .show(ui, |ui| {
                                ui.label(RichText::new(format!("L{layer}")).strong().color(Color32::from_rgba_unmultiplied(255, 255, 255, a)));
                            });
                        ui.label(RichText::new(&title).color(Color32::from_rgba_unmultiplied(235, 236, 242, a)));
                    });
                    ui.add_space(4.0);
                }
                let press = if c.show_combo { self.pressed.clone() } else { vec![false; geo.len()] };
                ui.set_max_width(kb_w);
                draw_keyboard(ui, geo, legends, &glow, &press, None, c.opacity.clamp(0.08, 1.0), c.monochrome);
                if c.show_combo {
                    ui.add_space(6.0);
                    let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
                    // In the settings preview the log may be empty - show a hint.
                    let entries = self.combo_recent();
                    combo_strip(ui, &entries, c.opacity.clamp(0.08, 1.0), accent, c.show_combo_ms);
                }
            });
    }

    /// Monitor selector: lists monitors by their real name + resolution
    /// (from the cached list).
    fn peek_monitor_combo(&self, ui: &mut egui::Ui, monitor: &mut usize) {
        let mons = &self.monitors_cache;
        if mons.len() <= 1 {
            ui.weak("only one monitor");
            *monitor = 0;
            return;
        }
        if *monitor >= mons.len() {
            *monitor = 0;
        }
        egui::ComboBox::from_id_salt("peek_monitor")
            .width(240.0)
            .selected_text(mons[*monitor].label(*monitor))
            .show_ui(ui, |ui| {
                for (i, m) in mons.iter().enumerate() {
                    ui.selectable_value(monitor, i, m.label(i));
                }
            });
    }

    /// Show a sample layer's peek for `ms`, so settings changes are visible.
    fn arm_preview(&mut self, c: &PeekConfig, ms: u64) {
        let sample = if self.active_layer > 0 {
            self.active_layer
        } else {
            self.layer_count().saturating_sub(1).max(1)
        };
        self.peek_layer = sample;
        self.peek_until = Some(Instant::now() + Duration::from_millis(ms.max(c.duration_ms)));
    }

    fn ui_localbuild(&mut self, ui: &mut egui::Ui) {
        // Uses the cached env - recompute only on demand (each check spawns
        // `which` processes, so never do it per frame).
        ui.horizontal(|ui| {
            status_dot(ui, self.env.qmk_cli);
            ui.label(RichText::new("qmk CLI").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.env.firmware_dir.is_some());
            ui.label(RichText::new("qmk_firmware tree").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.env.arm_gcc);
            ui.label(RichText::new("arm-gcc").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.connected.is_some());
            ui.label(RichText::new("zsa/voyager").color(pal::TEXT_MUTED));
        });
        if let Some(d) = &self.env.firmware_dir {
            ui.label(RichText::new(format!("tree: {}", d.display())).size(11.5).monospace().color(pal::TEXT_DIM));
        }

        if !self.env.is_ready() {
            egui::CollapsingHeader::new(RichText::new("Setup guide").color(pal::AMBER)).default_open(true).show(ui, |ui| {
                if !self.env.qmk_cli {
                    ui.label("1. Install the QMK CLI:");
                    ui.code("pip3 install qmk   # or: brew install qmk/qmk/qmk");
                }
                if self.env.firmware_dir.is_none() {
                    ui.label("2. Fetch ZSA's firmware tree (one-time, ~1 GB):");
                    ui.code("qmk setup zsa/qmk_firmware -b firmware25");
                }
                ui.horizontal(|ui| {
                    if ui.button("↻ Recheck setup").clicked() {
                        self.env = localbuild::detect_env();
                    }
                });
            });
            return;
        }

        // Ready - power-user actions. Everything here runs on this Mac; the
        // only network is an anonymous read of the generated QMK source.
        ui.add_space(4.0);
        let edits = self.key_edits.len();
        if edits > 0 {
            ui.colored_label(pal::AMBER, format!("● {edits} staged key change{}", if edits == 1 { "" } else { "s" }));
        } else {
            ui.weak("Stage key remaps in Live (select a key → Assign key). Build compiles the current layout + your staged changes.");
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let can = self.connected.is_some() && !self.build_busy;
            if ui.add_enabled(can, egui::Button::new(RichText::new("⚙ Build firmware").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                self.start_local_build(false);
            }
            if ui.add_enabled(can, egui::Button::new(RichText::new("⚡ Build & flash").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                self.start_local_build(true);
            }
            if ui.button("🔦 Flash a file / Oryx URL…").clicked() {
                self.show_flash = true;
            }
            if ui.button("📂 Open firmware folder").clicked() {
                if let Some(d) = &self.env.firmware_dir {
                    reveal_in_finder(&d.join("keyboards/zsa/voyager/keymaps/keyjitsu"));
                }
            }
            if ui.button("↻ Recheck").clicked() {
                self.env = localbuild::detect_env();
            }
            if self.build_busy {
                ui.spinner();
                if ui.button("✕ cancel").clicked() {
                    self.build_cancel.store(true, Ordering::SeqCst);
                }
            }
        });

        if let Some(bin) = &self.last_build_bin {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(pal::GREEN, "✓ built");
                if ui.link(RichText::new(bin.display().to_string()).size(11.5).monospace()).clicked() {
                    reveal_in_finder(bin);
                }
            });
        }
        if !self.build_log.is_empty() {
            egui::CollapsingHeader::new("Build log").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    ui.label(RichText::new(&self.build_log).monospace().size(11.0).color(pal::TEXT_MUTED));
                });
            });
        }
        ui.add_space(2.0);
        ui.weak("To flash: keyjitsu waits for the bootloader - press the Voyager's reset button when prompted, and don't unplug it while it writes.");
    }

    fn flash_controls(&mut self, ui: &mut egui::Ui) {
        ui.weak("Firmware - separate from the glow above. Changing a key's *function* needs Oryx; this flashes a full layout.");
        if let Some((_, serial)) = &self.connected {
            ui.label(format!("current firmware/layout: {serial}"));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Oryx URL or .bin path:");
            ui.text_edit_singleline(&mut self.flash_input);
        });

        let busy = matches!(
            self.flash_state,
            Some(FlashState::Downloading)
                | Some(FlashState::WaitingForBootloader)
                | Some(FlashState::Working { .. })
        );
        ui.horizontal(|ui| {
            let can_latest = self.connected.is_some() && !busy;
            if ui
                .add_enabled(can_latest, egui::Button::new("⚡ flash latest from Oryx"))
                .on_hover_text("updates to the newest revision of the layout already on the keyboard")
                .clicked()
            {
                self.flash_state = None;
                self.flash_cancel = Arc::new(AtomicBool::new(false));
                self.flash_rx =
                    Some(worker::spawn_flash(None, true, self.flash_cancel.clone(), ui.ctx().clone()));
            }
            let can_input = !self.flash_input.trim().is_empty() && !busy;
            if ui.add_enabled(can_input, egui::Button::new("flash from URL/file")).clicked() {
                self.flash_state = None;
                self.flash_cancel = Arc::new(AtomicBool::new(false));
                self.flash_rx = Some(worker::spawn_flash(
                    Some(self.flash_input.trim().to_string()),
                    false,
                    self.flash_cancel.clone(),
                    ui.ctx().clone(),
                ));
            }
            if busy && ui.button("✕ cancel").clicked() {
                self.flash_cancel.store(true, Ordering::SeqCst);
            }
        });
        ui.add_space(8.0);

        match &self.flash_state {
            None => {
                ui.weak("After starting, press the keyboard's RESET button (Voyager: tiny button on the left half).");
            }
            Some(FlashState::Downloading) => {
                ui.label("Downloading firmware…");
                ui.add(ProgressBar::new(0.0).animate(true));
            }
            Some(FlashState::WaitingForBootloader) => {
                ui.label(RichText::new("Press the RESET button on the keyboard now").strong());
                ui.add(ProgressBar::new(0.0).animate(true));
            }
            Some(FlashState::Working { phase, fraction }) => {
                ui.label(*phase);
                ui.add(ProgressBar::new(*fraction).show_percentage());
            }
            Some(FlashState::Done) => {
                ui.colored_label(
                    pal::GREEN,
                    "✓ Flash complete - the keyboard reconnects automatically.",
                );
            }
            Some(FlashState::Failed(e)) => {
                ui.colored_label(pal::RED, e);
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(h) = &mut self.heat {
            let _ = h.save();
        }
        // Hand the LEDs back to the firmware on exit (in case an effect or glow
        // sync had taken them over).
        let _ = self.cmd_tx.send(KbCmd::RgbRelease);
    }
}

#[cfg(test)]
mod slot_tests {
    use super::hold_wrap;

    #[test]
    fn hold_wraps_layers_and_mods() {
        assert_eq!(hold_wrap("MO(2)", "KC_A").as_deref(), Some("LT(2,KC_A)"));
        assert_eq!(hold_wrap("KC_LSFT", "KC_A").as_deref(), Some("LSFT_T(KC_A)"));
        assert_eq!(hold_wrap("KC_RGUI", "KC_SPC").as_deref(), Some("RGUI_T(KC_SPC)"));
        assert_eq!(hold_wrap("KC_MEH", "KC_1").as_deref(), Some("MEH_T(KC_1)"));
        // Not expressible as MT/LT → None (caller warns + falls back to tap).
        assert_eq!(hold_wrap("KC_B", "KC_A"), None);
        assert_eq!(hold_wrap("OSL(1)", "KC_A"), None);
    }
}

#[cfg(test)]
mod update_check_tests {
    /// Exercises the real request + parsing used by the button. Needs network,
    /// so it is ignored by default: `cargo test update_check_live -- --ignored`.
    #[test]
    #[ignore]
    fn update_check_live() {
        let rx = super::spawn_update_check();
        let r = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("checker replied");
        match r {
            super::UpdateCheck::Error(e) => panic!("update check failed: {e}"),
            other => eprintln!("LIVE RESULT: {other:?}"),
        }
    }

    #[test]
    fn version_compare() {
        use super::version_newer;
        assert!(version_newer("v0.9.2", "0.9.1"));
        assert!(version_newer("1.0.0", "0.9.9"));
        assert!(!version_newer("v0.9.1", "0.9.1"));
        assert!(!version_newer("0.9.0", "0.9.1"));
        assert!(!version_newer("garbage", "0.9.1")); // unparseable never reports an update
        assert!(version_newer("v0.10.0-beta", "0.9.1")); // pre-release suffix ignored
    }
}

#[cfg(test)]
mod layer_ref_tests {
    use super::renumber_layer_ref;

    #[test]
    fn shifts_refs_above_deleted_layer() {
        // Deleting layer 2: refs to 3+ shift down, 2 and below unchanged.
        assert_eq!(renumber_layer_ref("MO(3)", 2), "MO(3)".replace('3', "2")); // MO(3)→MO(2)
        assert_eq!(renumber_layer_ref("MO(3)", 2), "MO(2)");
        assert_eq!(renumber_layer_ref("MO(1)", 2), "MO(1)");
        assert_eq!(renumber_layer_ref("MO(2)", 2), "MO(2)"); // the deleted one: left as-is
        assert_eq!(renumber_layer_ref("LT(4,KC_A)", 2), "LT(3,KC_A)");
        assert_eq!(renumber_layer_ref("OSL(5)", 2), "OSL(4)");
        assert_eq!(renumber_layer_ref("DF(3)", 2), "DF(2)");
        // Non-layer codes untouched.
        assert_eq!(renumber_layer_ref("KC_A", 2), "KC_A");
        assert_eq!(renumber_layer_ref("LSFT_T(KC_A)", 2), "LSFT_T(KC_A)");
    }
}

#[cfg(test)]
mod combo_tests {
    use super::combo_merges;

    #[test]
    fn double_tap_merges_only_same_key_in_window() {
        // Same key, single, quick press-to-press → merges (becomes ×2).
        assert!(combo_merges(Some((5, 1, 220)), 5));
        assert!(combo_merges(Some((5, 1, 480)), 5)); // still inside 500ms
        // Too slow → new entry.
        assert!(!combo_merges(Some((5, 1, 700)), 5));
        // Different key → new entry.
        assert!(!combo_merges(Some((5, 1, 120)), 6));
        // Previous already a double → new entry (no triple merge).
        assert!(!combo_merges(Some((5, 2, 120)), 5));
        // No history → new entry.
        assert!(!combo_merges(None, 5));
    }
}
