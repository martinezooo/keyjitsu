//! `keyjitsu gui` (also plain `keyjitsu`) - the windowed app: Keymapp feature
//! parity (live view, layers, heatmap, flashing) plus keyjitsu's extras
//! (per-key RGB, built-in-keyboard guard, autolayer rules).

mod firmware;
mod fx;
mod heatmap_page;
mod live;
mod peek;
mod profiles;
mod rgb_anim;
mod runtime;
mod settings;
mod sidebar;
mod state;
mod update;
mod widget;
mod worker;

pub use rgb_anim::{CustomFx, Effect, FxStep, FxTrigger, PressEffect};

use std::collections::HashMap;

type FirmwareEdits = HashMap<(u8, usize), String>;
type FirmwareDances = HashMap<(u8, usize), [Option<String>; 4]>;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use profiles::{
    create_profile, list_profiles, load_profile, next_profile_copy_name, profile_file_name,
    profile_path, save_profile,
};
use rgb_anim::{Anim, FxEvent};
use state::DeviceStateKind;
use update::{spawn_update_check, UpdateCheck};

#[cfg(target_os = "macos")]
use anyhow::Context as _;
use anyhow::{anyhow, Result};
use eframe::egui::{self, Color32, ProgressBar, RichText};

use std::time::Instant;

use crate::config::{
    self, AutolayerRule, GlowOverride, HAlign, PeekConfig, StagedDance, StagedEdit, VAlign,
};
use crate::firmware_state::{self, FirmwareDance, FirmwareEdit, FirmwareGlow, FirmwareState};
use crate::geometry::{self, Geometry};
use crate::heatmap::{normalize, HeatmapStore};
use crate::key_action::{hold_wrap, synth_key, synth_slots, unknown_device_key};
use crate::keycodes;
use crate::legend::{self, labels_for};
use crate::localbuild::{self, BuildMsg, KeyEdit};
use crate::oryx_api::{Layer, Layout, LayoutId, OryxKey};
use crate::perf;
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
        ctrlc::set_handler(|| {
            crate::macos_kb::force_restore_if_active();
            std::process::exit(0);
        })
        .context("installing built-in keyboard safety handler")?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirmwareConfirmation {
    Confirmed,
    Mismatch,
}

fn confirm_firmware_state(expected: &str, reported: Option<&str>) -> FirmwareConfirmation {
    if reported == Some(expected) {
        FirmwareConfirmation::Confirmed
    } else {
        FirmwareConfirmation::Mismatch
    }
}

fn is_post_flash_generation(start: Option<u64>, current: u64) -> bool {
    start.is_none_or(|start| current > start)
}

fn layout_event_is_current(current: Option<u64>, event: u64) -> bool {
    current == Some(event)
}

fn disconnect_event_is_current(current: Option<u64>, event: u64) -> bool {
    current.is_none_or(|generation| generation == event)
}

fn flash_can_cancel(state: Option<&FlashState>) -> bool {
    matches!(
        state,
        None | Some(FlashState::Downloading) | Some(FlashState::WaitingForBootloader)
    )
}

fn flash_is_writing(state: Option<&FlashState>) -> bool {
    matches!(state, Some(FlashState::Working { .. }))
}

fn flash_job_terminal(state: Option<&FlashState>) -> bool {
    matches!(state, Some(FlashState::Done) | Some(FlashState::Failed(_)))
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
struct App {
    egui_ctx: egui::Context,
    _device_handle: worker::DeviceWorkerHandle,
    erx: Receiver<DevEvent>,
    cmd_tx: Sender<KbCmd>,

    connected: Option<(String, String)>, // (model, serial/layout-id)
    connection_generation: Option<u64>,
    layout: Option<Layout>,
    /// Last layout-fetch/parse error for the current connection generation.
    layout_error: Option<String>,
    /// Best known state of the connected Keyjitsu-built firmware. Marker-bearing
    /// firmware is device-verified; a pre-marker build may use an explicitly
    /// recovered legacy state for the exact same Oryx serial/revision.
    firmware_state: Option<FirmwareState>,
    heat: Option<HeatmapStore>,
    heat_error: Option<String>,

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
    selected_key: Option<usize>,              // shown in the bottom config panel
    edit_color: [u8; 3],
    sync_glow: bool,  // mirror the glow onto the physical keyboard
    needs_push: bool, // re-push colors on next frame
    show_flash: bool, // Flash section expanded in Live

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
    build_state_id: Option<String>,
    expected_firmware_state: Option<String>,
    expected_firmware_generation: Option<u64>,
    flash_write_completed: bool,
    last_build_bin: Option<std::path::PathBuf>,
    last_build_state_id: Option<String>,
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
    /// True after the user tried to close the app during a non-cancelable
    /// firmware write; cleared automatically once the write is over.
    flash_close_blocked: bool,
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
    /// "Build a combo" tab: [ctrl, shift, alt, gui] toggles, the chosen base
    /// key (code, label), and a small local search for it.
    picker_combo_mods: [bool; 4],
    picker_combo_base: Option<(&'static str, &'static str)>,
    picker_combo_search: String,
    /// A slot's stored code can hold more than one step ("KC_A\nKC_B" -
    /// "then press another key"), tapped in order when the gesture fires.
    /// `Some(i)` while the picker is open to REPLACE step `i` of the
    /// current slot (instead of the whole slot); `picker_append` while open
    /// to ADD a new step at the end.
    picker_step_index: Option<usize>,
    picker_append: bool,

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
    /// Ground truth from hidutil (not just "did the command error"),
    /// refreshed every few seconds while the guard should be on.
    #[cfg(target_os = "macos")]
    guard_hidutil_ok: bool,
    #[cfg(target_os = "macos")]
    guard_checked: Instant,
    /// The manual "press a key on the MacBook" self-test: in flight or done.
    #[cfg(target_os = "macos")]
    guard_test_rx: Option<std::sync::mpsc::Receiver<crate::macos_guard_test::GuardTestOutcome>>,
    #[cfg(target_os = "macos")]
    guard_test_result: Option<crate::macos_guard_test::GuardTestOutcome>,
    #[cfg(target_os = "macos")]
    guard_test_started: Option<Instant>,
    #[cfg(target_os = "macos")]
    guard_enabled: bool,
    #[cfg(target_os = "macos")]
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
    /// Active firmware profile name. None means the connected device state is
    /// the desired baseline; selecting a profile creates a target that must be flashed.
    active_profile: Option<String>,
    /// Full desired firmware target loaded from the active profile.
    profile_state: Option<FirmwareState>,
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
    profile_error: Option<String>,
    persist_error: Option<String>,
    /// Last failure while opening/revealing a build-related path.
    file_action_error: Option<String>,
    /// Last autostart toggle error (shown in the App card).
    #[cfg(target_os = "macos")]
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
        egui::FontData::from_static(include_bytes!(
            "../../resources/NotoSansSymbols2-Regular.ttf"
        ))
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
        (
            "arial-unicode",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push(name.to_owned());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn replace_staged_config(
    cfg: &mut config::Config,
    hash: &str,
    edits: Vec<StagedEdit>,
    dances: Vec<StagedDance>,
) {
    cfg.staged_edits.retain(|entry| entry.layout != hash);
    cfg.staged_edits.extend(edits);
    cfg.staged_dances.retain(|entry| entry.layout != hash);
    cfg.staged_dances.extend(dances);
}

fn reconcile_confirmed_layout_config(
    cfg: &mut config::Config,
    hash: &str,
    confirmed_layers: &[config::CustomLayer],
    edits: Vec<StagedEdit>,
    dances: Vec<StagedDance>,
) {
    replace_staged_config(cfg, hash, edits, dances);
    cfg.custom_layer_sets
        .retain(|set| !(set.layout == hash && set.layers == confirmed_layers));
}

struct LayoutScopedState {
    custom_layers: Vec<config::CustomLayer>,
    glow_overrides: Vec<GlowOverride>,
    key_fx: Vec<config::KeyFx>,
    staged_edits: Vec<StagedEdit>,
    staged_dances: Vec<StagedDance>,
}

fn replace_layout_scoped_state(cfg: &mut config::Config, hash: &str, state: LayoutScopedState) {
    cfg.custom_layers.retain(|layer| layer.layout != hash);
    cfg.custom_layer_sets.retain(|set| set.layout != hash);
    cfg.custom_layer_sets.push(config::CustomLayerSet {
        layout: hash.to_string(),
        layers: state.custom_layers,
    });

    cfg.glow_overrides.retain(|entry| entry.layout != hash);
    cfg.glow_overrides.extend(state.glow_overrides);

    cfg.key_fx.retain(|entry| entry.layout != hash);
    cfg.key_fx.extend(state.key_fx);

    replace_staged_config(cfg, hash, state.staged_edits, state.staged_dances);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, serial: Option<String>) -> App {
        setup_style(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let (erx, cmd_tx, device_handle) = worker::spawn_device_worker(serial, cc.egui_ctx.clone());
        let (cfg, config_load_error) = match config::load_checked() {
            Ok(cfg) => (cfg, None),
            Err(e) => (
                config::Config::default(),
                Some(format!("loading config: {e:#}")),
            ),
        };
        let (profile_state, profile_load_error) = match cfg.active_profile.as_deref() {
            Some(name) => match load_profile(name) {
                Ok(state) => (Some(state), None),
                Err(e) => (
                    None,
                    Some(format!("could not load firmware profile {name}: {e:#}")),
                ),
            },
            None => (None, None),
        };
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
            _device_handle: device_handle,
            erx,
            cmd_tx,
            connected: None,
            connection_generation: None,
            layout: None,
            layout_error: None,
            firmware_state: None,
            heat: None,
            heat_error: None,
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
            fx_sel: match std::env::var("KEYJITSU_FX").ok().and_then(|v| {
                v.strip_prefix("custom")
                    .and_then(|n| n.parse::<usize>().ok())
            }) {
                Some(i) if i < cfg.custom_fx.len() => FxSel::Custom(i),
                _ => FxSel::Press(PressEffect::Ripple),
            },
            fx_lib: if std::env::var("KEYJITSU_FX").is_ok() {
                FxLib::Custom
            } else {
                FxLib::Press
            },
            overlay_chord: if cfg.overlay_chord.is_empty() {
                cfg.overlay_trigger.map(|t| vec![t]).unwrap_or_default()
            } else {
                cfg.overlay_chord.clone()
            },
            binding_overlay: false,
            binding_draft: Vec::new(),
            hidden_shortcuts: cfg.hidden_shortcuts.clone(),
            active_profile: cfg.active_profile.clone(),
            profile_state,
            prof_new_open: false,
            keys_cat: None,
            keys_search: String::new(),
            custom_shortcuts: cfg.custom_shortcuts.clone(),
            keys_adding: false,
            profile_draft: String::new(),
            profile_error: profile_load_error,
            persist_error: config_load_error,
            file_action_error: None,
            #[cfg(target_os = "macos")]
            autostart_error: None,
            update_rx: None,
            update_state: None,
            draft_sc: config::CustomShortcut {
                category: String::new(),
                keys: String::new(),
                desc: String::new(),
                high: true,
            },
            // KEYJITSU_SEL=<key index> preselects a key (QA screenshots).
            selected_key: std::env::var("KEYJITSU_SEL")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|&i| i < key_count),
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
            picker_combo_mods: [false; 4],
            picker_combo_base: None,
            picker_combo_search: String::new(),
            picker_step_index: None,
            picker_append: false,
            edit_synced: None,
            key_dances: HashMap::new(),
            build_rx: None,
            build_log: String::new(),
            build_busy: false,
            build_flash_after: true,
            build_state_id: None,
            expected_firmware_state: None,
            expected_firmware_generation: None,
            flash_write_completed: false,
            build_open: false,
            build_phase: String::new(),
            build_progress: 0.0,
            build_compiles: 0,
            build_result: None,
            last_build_bin: None,
            last_build_state_id: None,
            build_cancel: Arc::new(AtomicBool::new(false)),
            flash_cancel: Arc::new(AtomicBool::new(false)),
            flash_close_blocked: false,
            env: localbuild::detect_env(),
            monitors_cache: fetch_monitors(&cc.egui_ctx),
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
            #[cfg(target_os = "macos")]
            guard_hidutil_ok: false,
            #[cfg(target_os = "macos")]
            guard_checked: Instant::now(),
            #[cfg(target_os = "macos")]
            guard_test_rx: None,
            #[cfg(target_os = "macos")]
            guard_test_result: None,
            #[cfg(target_os = "macos")]
            guard_test_started: None,
            #[cfg(target_os = "macos")]
            guard_enabled: cfg.guard_enabled,
            #[cfg(target_os = "macos")]
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
            PerfPhase {
                label: "idle".into(),
                secs: 12,
                anim: Effect::Off,
                peek: false,
            },
            PerfPhase {
                label: "layout RGB".into(),
                secs: 12,
                anim: Effect::Layout,
                peek: false,
            },
            PerfPhase {
                label: "rainbow".into(),
                secs: 12,
                anim: Effect::Rainbow,
                peek: false,
            },
            PerfPhase {
                label: "rainbow+peek".into(),
                secs: 12,
                anim: Effect::Rainbow,
                peek: true,
            },
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
        if !perf::CPU_SUPPORTED {
            return;
        }
        // Live CPU read (cheap, sub-Hz).
        if self.perf_tick.elapsed() >= Duration::from_millis(600) {
            self.perf_live = self.perf_sampler.sample();
            self.perf_tick = Instant::now();
        }

        let now = Instant::now();
        let Some(run) = self.perf_run.as_ref() else {
            return;
        };

        // Decide phase transitions without holding a borrow across self calls.
        let (ended, advance_to) = if run.phases.is_empty() {
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
        };

        if ended {
            self.finish_perf();
            return;
        }

        if let Some(next) = advance_to {
            let Some(ph) = self
                .perf_run
                .as_ref()
                .and_then(|run| run.phases.get(next))
                .cloned()
            else {
                self.finish_perf();
                return;
            };
            self.set_anim_effect(ph.anim);
            if ph.peek {
                self.peek_layer = self.active_layer.max(1);
                self.peek_until = Some(now + Duration::from_secs(ph.secs + 1));
            } else {
                self.peek_until = None;
            }
            let Some(run) = self.perf_run.as_mut() else {
                return;
            };
            run.phase_i = next;
            run.phase_until = now + Duration::from_secs(ph.secs);
        }

        // Take a sample once per second.
        if self
            .perf_run
            .as_ref()
            .is_none_or(|run| run.next_sample > now)
        {
            return;
        }

        let cpu = match self.perf_run.as_mut() {
            Some(run) => run.sampler.sample(),
            None => return,
        };
        let label = match self.perf_run.as_ref() {
            Some(run) if run.phases.is_empty() => self.perf_state().label(),
            Some(run) => run
                .phases
                .get(run.phase_i)
                .map(|phase| phase.label.clone())
                .unwrap_or_else(|| "unknown".into()),
            None => return,
        };
        if let Some(run) = self.perf_run.as_mut() {
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
    fn unsaved_glow_count(&self) -> usize {
        let mut keys: std::collections::HashSet<(u8, usize)> =
            self.glow_work.keys().copied().collect();
        keys.extend(self.glow_saved.keys().copied());
        keys.into_iter()
            .filter(|k| self.glow_work.get(k) != self.glow_saved.get(k))
            .count()
    }

    fn custom_layers_pending(&self) -> bool {
        match &self.firmware_state {
            Some(state) => self.custom_layers != state.custom_layers,
            None => !self.custom_layers.is_empty(),
        }
    }

    fn pending_glow_count(&self) -> usize {
        let device = self.device_glow_map();
        let mut keys: std::collections::HashSet<(u8, usize)> =
            self.glow_work.keys().copied().collect();
        keys.extend(device.keys().copied());
        keys.into_iter()
            .filter(|k| self.glow_work.get(k) != device.get(k))
            .count()
    }

    fn pending_key_change_count(&self) -> usize {
        let (actual_edits, actual_dances) = self.actual_firmware_maps();
        let (desired_edits, desired_dances) = self.desired_firmware_maps();
        firmware_map_diff_count(
            &actual_edits,
            &actual_dances,
            &desired_edits,
            &desired_dances,
        )
    }

    fn pending_firmware_count(&self) -> usize {
        self.pending_key_change_count()
            + self.pending_glow_count()
            + usize::from(self.custom_layers_pending())
            + usize::from(
                self.firmware_state
                    .as_ref()
                    .is_some_and(FirmwareState::needs_action_normalization),
            )
    }

    fn save_glow(&mut self) -> bool {
        if self.active_profile.is_some() {
            return self.save_active_profile_target();
        }
        let Some(hash) = self.layout_hash.clone() else {
            return false;
        };
        let entries: Vec<_> = self
            .glow_work
            .iter()
            .map(|(&(layer, key), &rgb)| GlowOverride {
                layout: hash.clone(),
                layer,
                key: key as u16,
                rgb,
            })
            .collect();
        let saved = self.persist_config("saving glow profile draft", move |cfg| {
            cfg.glow_overrides.retain(|o| o.layout != hash);
            cfg.glow_overrides.extend(entries);
            if !cfg.glow_draft_layouts.iter().any(|layout| layout == &hash) {
                cfg.glow_draft_layouts.push(hash);
            }
        });
        if saved {
            self.glow_saved = self.glow_work.clone();
        }
        saved
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
    fn push_glow(&self) -> bool {
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
        if self
            .cmd_tx
            .send(KbCmd::SetRgbLedAll {
                r: dominant[0],
                g: dominant[1],
                b: dominant[2],
            })
            .is_err()
        {
            return false;
        }
        for (i, c) in colors.iter().enumerate() {
            if *c != dominant
                && self
                    .cmd_tx
                    .send(KbCmd::SetRgbLed {
                        led: i as u8,
                        r: c[0],
                        g: c[1],
                        b: c[2],
                    })
                    .is_err()
            {
                return false;
            }
        }
        true
    }

    pub(super) fn update_frame(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.reconcile_background_jobs(ctx);

        let writing_firmware = flash_is_writing(self.flash_state.as_ref());
        if ctx.input(|i| i.viewport().close_requested()) && writing_firmware {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.flash_close_blocked = true;
        } else if !writing_firmware {
            self.flash_close_blocked = false;
        }
        self.tick_perf();
        if let Some(rx) = &self.update_rx {
            if let Ok(r) = rx.try_recv() {
                self.update_state = Some(r);
                self.update_rx = None;
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(rx) = &self.guard_test_rx {
            if let Ok(r) = rx.try_recv() {
                self.guard_test_result = Some(r);
                self.guard_test_rx = None;
                self.guard_test_started = None;
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
            self.monitors_cache = fetch_monitors(ctx);
            self.monitors_checked = Instant::now();
        }

        if self.flash_close_blocked {
            egui::Window::new("Firmware write in progress")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.colored_label(
                        pal::AMBER,
                        "Keyjitsu must stay open until the firmware write finishes.",
                    );
                    ui.label("Do not unplug the keyboard. The app can be closed after flashing completes.");
                });
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
                    for (tab, name, icon, available) in [
                        (Tab::Live, "Live", "⌨", true),
                        (Tab::Layers, "Layers", "▤", true),
                        (Tab::Heatmap, "Heatmap", "🔥", true),
                        (Tab::Peek, "Peek", "👁", true),
                        (Tab::Fx, "FX Studio (exp)", "✨", true),
                        (Tab::Perf, "Performance (exp)", "📈", perf::CPU_SUPPORTED),
                        (Tab::Auto, "Autolayer", "⇆", cfg!(target_os = "macos")),
                        (Tab::Tools, "Settings", "⚙", true),
                    ] {
                        if !available {
                            continue;
                        }
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
                    if let Some(e) = &self.persist_error {
                        egui::Frame::new()
                            .show(ui, |ui| status_pill(ui, "⚠ save failed", pal::RED))
                            .response
                            .on_hover_text(e);
                    }
                    ui.horizontal_wrapped(|ui| {
                        #[cfg(target_os = "macos")]
                        {
                            let (label, color, hover) = self.guard_indicator();
                            let action = if self.guard_enabled {
                                "Click to turn the guard OFF and restore the built-in keyboard immediately."
                            } else {
                                "Click to turn the guard ON. It engages while the Voyager is connected."
                            };
                            let resp = egui::Frame::new()
                                .show(ui, |ui| status_pill(ui, &label, color))
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_text(format!("{hover}
{action}"));
                            if resp.clicked() {
                                self.set_guard_enabled(!self.guard_enabled);
                            }
                        }
                        if cfg!(target_os = "macos") && self.autolayer_enabled {
                            egui::Frame::new()
                                .show(ui, |ui| status_pill(ui, "⇆ autolayer", pal::GREEN))
                                .response
                                .on_hover_text("Layers follow the frontmost app.");
                        }
                    });
                    if perf::CPU_SUPPORTED && self.show_cpu_header {
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
            // Size the inspector from its visible slot rows, with a compact empty state.
            let cap = (ctx.screen_rect().height() * 0.44).clamp(170.0, 300.0);
            let h = if self.selected_key.is_some() {
                let rows = 1
                    + (1..4)
                        .filter(|&sl| self.edit_slots[sl].is_some() || self.slot_added[sl])
                        .count();
                let warn = if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
                    18.0
                } else {
                    0.0
                };
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

/// Render keycode choices and shortcut rows for the Assign picker.
fn shortcut_pick_row(ui: &mut egui::Ui, keys: &str, desc: &str, pick: &mut Option<String>) {
    match crate::shortcuts::to_qmk_code(keys) {
        Some(code) => {
            if ui.button(format!("{keys} - {desc}")).clicked() {
                *pick = Some(code);
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new(format!("{keys} - {desc}")))
                .on_disabled_hover_text(
                    "Not a single key press (a sequence, combo, or tap/hold description) - can't be assigned directly.",
                );
        }
    }
}

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
                k.label
                    .replace("L{n}", layer_name)
                    .replace("{n}", layer_name)
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
            None => format!(
                "Monitor {} · {}×{}",
                index + 1,
                self.w as i32,
                self.h as i32
            ),
        }
    }
}

fn fetch_monitors(_ctx: &egui::Context) -> Vec<MonitorInfo> {
    #[cfg(target_os = "macos")]
    {
        crate::macos_display::monitors()
            .into_iter()
            .map(|m| MonitorInfo {
                x: m.x,
                y: m.y,
                w: m.w,
                h: m.h,
                name: m.name,
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let rect = _ctx.input(|i| i.viewport().outer_rect);
        let size = _ctx.input(|i| i.viewport().monitor_size);
        match (rect, size) {
            (Some(rect), _) => vec![MonitorInfo {
                x: rect.min.x,
                y: rect.min.y,
                w: rect.width(),
                h: rect.height(),
                name: Some("Current monitor".into()),
            }],
            (None, Some(size)) => vec![MonitorInfo {
                x: 0.0,
                y: 0.0,
                w: size.x,
                h: size.y,
                name: Some("Current monitor".into()),
            }],
            _ => Vec::new(),
        }
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

fn live_board_size(avail_w: f32, avail_h: f32, cols: f32, rows: f32) -> (f32, f32) {
    let chrome_h = 88.0;
    let card_w = 48.0;
    let card_h = 32.0;
    let by_height = ((avail_h - chrome_h - card_h).max(120.0)) / rows;
    let by_width = ((avail_w - card_w).max(120.0)) / cols;
    let unit = by_height.min(by_width).clamp(34.0, 62.0);
    (unit * cols + card_w, unit * rows + card_h)
}

fn centered_page(ui: &mut egui::Ui, max_width: f32, body: impl FnOnce(&mut egui::Ui)) {
    let full = ui.available_width();
    let width = full.min(max_width);
    let pad = ((full - width) / 2.0).max(12.0);
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(width - 24.0);
            body(ui);
        });
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
    let label = if dot {
        format!("● {label}")
    } else {
        label.to_string()
    };
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
    let (full, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
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
        let galley = p.layout_no_wrap(
            b.to_string(),
            egui::FontId::proportional(9.5),
            pal::TEXT_DIM,
        );
        let pad = egui::vec2(5.0, 2.0);
        let bsize = galley.size() + pad * 2.0;
        let brect = egui::Rect::from_min_size(
            egui::pos2(
                rect.right() - 8.0 - bsize.x,
                rect.center().y - bsize.y / 2.0,
            ),
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
    if nav_row(
        ui,
        30.0,
        0.0,
        8.0,
        fill,
        text,
        13.5,
        &format!("{icon}  {name}"),
        badge,
    )
    .clicked()
    {
        *current = tab;
    }
    ui.add_space(1.0);
}

/// Standard page header: a title and one short subtitle line. Every page
/// that has a header uses this, so type and spacing stay identical.
fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong().size(21.0).color(pal::TEXT));
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).size(12.5).color(pal::TEXT_DIM));
    }
    ui.add_space(12.0);
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
            ui.label(
                RichText::new(title)
                    .strong()
                    .size(14.0)
                    .color(pal::VIOLET_HI),
            );
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
            egui::Label::new(egui::RichText::new(label).color(pal::TEXT_MUTED))
                .halign(egui::Align::LEFT),
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
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(radius as u8), bg);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(radius as u8),
        egui::Stroke::new(1.0, if *on { pal::VIOLET } else { pal::BORDER }),
        egui::StrokeKind::Inside,
    );
    let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter().circle_filled(
        egui::pos2(cx, rect.center().y),
        radius * 0.72,
        egui::Color32::WHITE,
    );
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
    let (a, b) = (
        egui::Color32::from_rgb(40, 42, 50),
        egui::Color32::from_rgb(28, 30, 37),
    );
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
        (
            VAlign::Top,
            [
                ("↖", HAlign::Left),
                ("↑", HAlign::Center),
                ("↗", HAlign::Right),
            ],
        ),
        (
            VAlign::Middle,
            [
                ("←", HAlign::Left),
                ("•", HAlign::Center),
                ("→", HAlign::Right),
            ],
        ),
        (
            VAlign::Bottom,
            [
                ("↙", HAlign::Left),
                ("↓", HAlign::Center),
                ("↘", HAlign::Right),
            ],
        ),
    ];
    egui::Frame::new()
        .fill(pal::INPUT)
        .stroke(egui::Stroke::new(1.0, pal::BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            egui::Grid::new("posgrid")
                .spacing([5.0, 5.0])
                .show(ui, |ui| {
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
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(3), pal::INPUT);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter()
        .rect_filled(fill, egui::CornerRadius::same(3), color);
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
                (
                    Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        (alpha as f32 * 0.35) as u8,
                    ),
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha),
                )
            } else if c.held {
                (
                    Color32::from_rgba_unmultiplied(30, 32, 42, alpha),
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(30, 32, 42, alpha),
                    Color32::from_rgba_unmultiplied(90, 94, 112, alpha),
                )
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
                            ui.label(
                                RichText::new(format!("Δ{}ms", c.gap_ms))
                                    .size(10.0)
                                    .color(dim),
                            );
                        }
                    }
                    if c.held {
                        ui.label(
                            RichText::new(if c.live { "hold" } else { "⇩" })
                                .size(11.0)
                                .strong()
                                .color(accent),
                        );
                    }
                    // Measurement readout: the press/hold duration in ms.
                    if show_ms {
                        ui.label(
                            RichText::new(format!("{}ms", c.ms))
                                .size(10.0)
                                .color(if c.held { accent } else { dim }),
                        );
                    }
                });
        }
    });
}

/// Rewrite the target layer in a layer-switch keycode after layer `del` was
/// removed: a reference to a layer ABOVE `del` shifts down by one; references
/// to `del` or below (and non-layer codes) are unchanged.
fn renumber_layer_ref(code: &str, del: u8) -> String {
    if code.contains('\n') {
        return code
            .split('\n')
            .map(|step| renumber_layer_ref(step, del))
            .collect::<Vec<_>>()
            .join("\n");
    }

    let target = |n: &str| n.trim().parse::<u8>().ok();
    for fam in ["MO", "TO", "TG", "TT", "OSL", "DF"] {
        if let Some(rest) = code
            .strip_prefix(fam)
            .and_then(|r| r.strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
        {
            return match target(rest) {
                Some(v) if v == del => "KC_NO".to_string(),
                Some(v) if v > del => format!("{fam}({})", v - 1),
                _ => code.to_string(),
            };
        }
    }
    if let Some(rest) = code.strip_prefix("LT(").and_then(|r| r.strip_suffix(')')) {
        if let Some((n, tap)) = rest.split_once(',') {
            return match target(n) {
                Some(v) if v == del => tap.trim().to_string(),
                Some(v) if v > del => format!("LT({},{})", v - 1, tap.trim()),
                _ => code.to_string(),
            };
        }
    }
    code.to_string()
}

/// Turn a QMK keycode string into an `OryxKey` for display: layer-switch
/// families render as `CODE → layer` (via the layer field), everything else
/// as its plain legend. A dual-role `LT(n,tap)` shows the tap with a hold hint.
fn staged_edit_is_pending(state: &FirmwareState, layer: u8, key: usize, code: &str) -> bool {
    !state
        .edits
        .iter()
        .any(|edit| edit.layer == layer && edit.key as usize == key && edit.code.as_str() == code)
}

fn staged_dance_is_pending(
    state: &FirmwareState,
    layer: u8,
    key: usize,
    slots: &[Option<String>; 4],
) -> bool {
    !state.dances.iter().any(|dance| {
        dance.layer == layer
            && dance.key as usize == key
            && dance.slots.as_slice() == slots.as_slice()
    })
}

fn merge_firmware_maps(
    state: Option<&FirmwareState>,
    staged_edits: &FirmwareEdits,
    staged_dances: &FirmwareDances,
) -> (FirmwareEdits, FirmwareDances) {
    let mut edits: HashMap<(u8, usize), String> = state
        .map(|state| {
            state
                .edits
                .iter()
                .map(|edit| ((edit.layer, edit.key as usize), edit.code.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut dances: HashMap<(u8, usize), [Option<String>; 4]> = state
        .map(|state| {
            state
                .dances
                .iter()
                .map(|dance| ((dance.layer, dance.key as usize), dance.slots.clone()))
                .collect()
        })
        .unwrap_or_default();

    for (&pos, code) in staged_edits {
        dances.remove(&pos);
        edits.insert(pos, code.clone());
    }
    for (&pos, slots) in staged_dances {
        edits.remove(&pos);
        dances.insert(pos, slots.clone());
    }
    (edits, dances)
}

fn firmware_map_diff_count(
    actual_edits: &FirmwareEdits,
    actual_dances: &FirmwareDances,
    desired_edits: &FirmwareEdits,
    desired_dances: &FirmwareDances,
) -> usize {
    let keys: std::collections::HashSet<(u8, usize)> = actual_edits
        .keys()
        .chain(actual_dances.keys())
        .chain(desired_edits.keys())
        .chain(desired_dances.keys())
        .copied()
        .collect();
    keys.into_iter()
        .filter(|pos| {
            actual_edits.get(pos) != desired_edits.get(pos)
                || actual_dances.get(pos) != desired_dances.get(pos)
        })
        .count()
}

fn status_dot(ui: &mut egui::Ui, ok: bool) {
    let (c, s) = if ok {
        (pal::GREEN, "●")
    } else {
        (Color32::from_rgb(230, 150, 90), "○")
    };
    ui.colored_label(c, s);
}

impl Drop for App {
    fn drop(&mut self) {
        // Build/download/bootloader-wait phases are safe to cancel. A close
        // request during the actual firmware write is intercepted in update(),
        // so Drop must never be the mechanism that interrupts erase/write.
        self.build_cancel.store(true, Ordering::SeqCst);
        self.flash_cancel.store(true, Ordering::SeqCst);

        if let Some(h) = &mut self.heat {
            if let Err(e) = h.save() {
                eprintln!("keyjitsu: could not save heatmap on exit: {e:#}");
            }
        }

        // Stop the autolayer watcher while the device worker is still alive so
        // its final SetLayer(false) can be delivered. DeviceWorkerHandle::drop
        // then drains release-only commands before disconnecting.
        self.autolayer.take();

        // Hand the LEDs back to the firmware on exit (in case an effect or glow
        // sync had taken them over).
        let _ = self.cmd_tx.send(KbCmd::RgbRelease);
    }
}

#[cfg(test)]
mod firmware_confirmation_tests {
    use super::{
        confirm_firmware_state, disconnect_event_is_current, flash_job_terminal,
        is_post_flash_generation, layout_event_is_current, FirmwareConfirmation, FlashState,
    };

    #[test]
    fn only_the_expected_reported_state_confirms_a_flash() {
        assert_eq!(
            confirm_firmware_state("0123456789", Some("0123456789")),
            FirmwareConfirmation::Confirmed
        );
        assert_eq!(
            confirm_firmware_state("0123456789", Some("aaaaaaaaaa")),
            FirmwareConfirmation::Mismatch
        );
        assert_eq!(
            confirm_firmware_state("0123456789", None),
            FirmwareConfirmation::Mismatch
        );
    }

    #[test]
    fn reconnect_must_be_newer_than_the_connection_that_started_the_flash() {
        assert!(!is_post_flash_generation(Some(7), 7));
        assert!(!is_post_flash_generation(Some(7), 6));
        assert!(is_post_flash_generation(Some(7), 8));
        assert!(is_post_flash_generation(None, 1));
    }

    #[test]
    fn terminal_flash_state_is_consumed_instead_of_reprocessed() {
        assert!(!flash_job_terminal(None));
        assert!(!flash_job_terminal(Some(&FlashState::Downloading)));
        assert!(!flash_job_terminal(Some(&FlashState::WaitingForBootloader)));
        assert!(!flash_job_terminal(Some(&FlashState::Working {
            phase: "writing",
            fraction: 0.5,
        })));
        assert!(flash_job_terminal(Some(&FlashState::Done)));
        assert!(flash_job_terminal(Some(&FlashState::Failed("nope".into()))));
    }

    #[test]
    fn stale_events_from_a_rapid_reconnect_cannot_replace_newer_state() {
        assert!(layout_event_is_current(Some(8), 8));
        assert!(!layout_event_is_current(Some(8), 7));
        assert!(!layout_event_is_current(None, 7));

        assert!(disconnect_event_is_current(Some(8), 8));
        assert!(!disconnect_event_is_current(Some(8), 7));
        assert!(disconnect_event_is_current(None, 7));
    }

    #[test]
    fn flash_cancel_is_only_available_before_device_writes_begin() {
        use super::FlashState;

        assert!(super::flash_can_cancel(None));
        assert!(super::flash_can_cancel(Some(&FlashState::Downloading)));
        assert!(super::flash_can_cancel(Some(
            &FlashState::WaitingForBootloader
        )));
        assert!(!super::flash_can_cancel(Some(&FlashState::Working {
            phase: "Writing",
            fraction: 0.5,
        })));
        assert!(!super::flash_can_cancel(Some(&FlashState::Done)));
        assert!(!super::flash_can_cancel(Some(&FlashState::Failed(
            "failed".into()
        ))));
        assert!(super::flash_is_writing(Some(&FlashState::Working {
            phase: "Writing",
            fraction: 0.5,
        })));
        assert!(!super::flash_is_writing(Some(
            &FlashState::WaitingForBootloader
        )));
        assert!(super::flash_is_writing(Some(&FlashState::Working {
            phase: "Restarting keyboard",
            fraction: 1.0,
        })));
    }
}

#[cfg(test)]
mod state_composition_tests {
    use std::collections::HashMap;

    use super::{
        firmware_map_diff_count, merge_firmware_maps, reconcile_confirmed_layout_config,
        replace_layout_scoped_state, staged_dance_is_pending, staged_edit_is_pending, synth_key,
        FxTrigger, LayoutScopedState, PressEffect,
    };
    use crate::config::{self, GlowOverride, StagedDance, StagedEdit};
    use crate::firmware_state::{FirmwareDance, FirmwareEdit, FirmwareState};

    #[test]
    fn layer_scoped_state_replacement_updates_all_categories_together() {
        let mut cfg = config::Config::default();
        cfg.custom_layer_sets.push(config::CustomLayerSet {
            layout: "target".into(),
            layers: vec![],
        });
        cfg.glow_overrides.push(GlowOverride {
            layout: "target".into(),
            layer: 4,
            key: 1,
            rgb: [1, 2, 3],
        });
        cfg.key_fx.push(config::KeyFx {
            layout: "target".into(),
            layer: 4,
            key: 2,
            trigger: FxTrigger::Press,
            effect: PressEffect::Flash,
            color: [4, 5, 6],
            custom: None,
        });
        cfg.staged_edits.push(StagedEdit {
            layout: "target".into(),
            layer: 4,
            key: 3,
            code: "KC_A".into(),
        });
        cfg.staged_dances.push(StagedDance {
            layout: "other".into(),
            layer: 1,
            key: 4,
            slots: [Some("KC_B".into()), None, None, None],
        });

        replace_layout_scoped_state(
            &mut cfg,
            "target",
            LayoutScopedState {
                custom_layers: vec![config::CustomLayer {
                    layout: "target".into(),
                    name: "Renumbered".into(),
                    keys: vec![],
                }],
                glow_overrides: vec![],
                key_fx: vec![],
                staged_edits: vec![StagedEdit {
                    layout: "target".into(),
                    layer: 3,
                    key: 3,
                    code: "KC_A".into(),
                }],
                staged_dances: vec![],
            },
        );

        let set = cfg
            .custom_layer_sets
            .iter()
            .find(|set| set.layout == "target")
            .unwrap();
        assert_eq!(set.layers[0].name, "Renumbered");
        assert!(!cfg
            .glow_overrides
            .iter()
            .any(|entry| entry.layout == "target"));
        assert!(!cfg.key_fx.iter().any(|entry| entry.layout == "target"));
        assert_eq!(
            cfg.staged_edits
                .iter()
                .find(|entry| entry.layout == "target")
                .map(|entry| entry.layer),
            Some(3)
        );
        assert!(cfg
            .staged_dances
            .iter()
            .any(|entry| entry.layout == "other"));
    }

    #[test]
    fn confirmed_firmware_reconciliation_is_one_config_mutation() {
        let confirmed_layers = vec![config::CustomLayer {
            layout: "target".into(),
            name: "Applied".into(),
            keys: vec![],
        }];
        let mut cfg = config::Config::default();
        cfg.custom_layer_sets.push(config::CustomLayerSet {
            layout: "target".into(),
            layers: confirmed_layers.clone(),
        });
        cfg.staged_edits.push(StagedEdit {
            layout: "target".into(),
            layer: 0,
            key: 1,
            code: "KC_OLD".into(),
        });
        cfg.staged_edits.push(StagedEdit {
            layout: "other".into(),
            layer: 0,
            key: 2,
            code: "KC_KEEP".into(),
        });

        reconcile_confirmed_layout_config(
            &mut cfg,
            "target",
            &confirmed_layers,
            vec![StagedEdit {
                layout: "target".into(),
                layer: 0,
                key: 3,
                code: "KC_NEW".into(),
            }],
            vec![],
        );

        assert!(!cfg
            .custom_layer_sets
            .iter()
            .any(|set| set.layout == "target"));
        assert_eq!(
            cfg.staged_edits
                .iter()
                .find(|entry| entry.layout == "target")
                .map(|entry| entry.code.as_str()),
            Some("KC_NEW")
        );
        assert!(cfg
            .staged_edits
            .iter()
            .any(|entry| entry.layout == "other" && entry.code == "KC_KEEP"));
    }

    #[test]
    fn full_firmware_target_diff_detects_removal_and_action_type_changes() {
        let actual_edits = HashMap::from([((1, 6), "LALT(KC_TAB)".to_string())]);
        let empty_dances = HashMap::new();
        assert_eq!(
            firmware_map_diff_count(
                &actual_edits,
                &empty_dances,
                &HashMap::new(),
                &HashMap::new(),
            ),
            1,
            "a profile that omits a device override must remove it"
        );
        assert_eq!(
            firmware_map_diff_count(&actual_edits, &empty_dances, &actual_edits, &empty_dances,),
            0
        );
        let desired_dances = HashMap::from([(
            (1, 6),
            [
                Some("KC_TAB".into()),
                Some("KC_LEFT_ALT".into()),
                None,
                None,
            ],
        )]);
        assert_eq!(
            firmware_map_diff_count(
                &actual_edits,
                &empty_dances,
                &HashMap::new(),
                &desired_dances,
            ),
            1,
            "edit-vs-dance at one physical key is one pending firmware change"
        );
    }

    #[test]
    fn pending_changes_override_applied_firmware_state() {
        let state = FirmwareState::new(
            "layout".into(),
            "revision".into(),
            vec![
                FirmwareEdit {
                    layer: 0,
                    key: 1,
                    code: "KC_A".into(),
                },
                FirmwareEdit {
                    layer: 0,
                    key: 2,
                    code: "KC_B".into(),
                },
            ],
            vec![FirmwareDance {
                layer: 0,
                key: 3,
                slots: [Some("KC_C".into()), None, Some("KC_D".into()), None],
            }],
            vec![],
            vec![],
        );
        let staged_edits = HashMap::from([((0, 3), "KC_NO".to_string())]);
        let staged_dances = HashMap::from([(
            (0, 2),
            [Some("KC_X".into()), Some("KC_LSFT".into()), None, None],
        )]);

        let (edits, dances) = merge_firmware_maps(Some(&state), &staged_edits, &staged_dances);

        assert_eq!(edits.get(&(0, 1)).map(String::as_str), Some("KC_A"));
        assert_eq!(edits.get(&(0, 3)).map(String::as_str), Some("KC_NO"));
        assert!(!edits.contains_key(&(0, 2)));
        assert!(!dances.contains_key(&(0, 3)));
        assert_eq!(
            dances.get(&(0, 2)).and_then(|slots| slots[0].as_deref()),
            Some("KC_X")
        );
    }

    #[test]
    fn flashing_an_older_build_keeps_newer_pending_changes() {
        let state = FirmwareState::new(
            "layout".into(),
            "revision".into(),
            vec![FirmwareEdit {
                layer: 0,
                key: 1,
                code: "KC_A".into(),
            }],
            vec![FirmwareDance {
                layer: 0,
                key: 2,
                slots: [Some("KC_B".into()), None, None, None],
            }],
            vec![],
            vec![],
        );

        assert!(!staged_edit_is_pending(&state, 0, 1, "KC_A"));
        assert!(staged_edit_is_pending(&state, 0, 1, "KC_Z"));
        assert!(staged_edit_is_pending(&state, 0, 3, "KC_A"));

        let applied_dance = [Some("KC_B".into()), None, None, None];
        let newer_dance = [Some("KC_C".into()), None, None, None];
        assert!(!staged_dance_is_pending(&state, 0, 2, &applied_dance));
        assert!(staged_dance_is_pending(&state, 0, 2, &newer_dance));
    }

    #[test]
    fn unknown_device_placeholder_is_explicit_not_blank() {
        let key = super::unknown_device_key();
        assert_eq!(key.custom_label.as_deref(), Some("?"));
    }

    #[test]
    fn picker_modified_tab_survives_staging_and_display_synthesis() {
        let key = synth_key("LALT(KC_TAB)");
        let action = key.tap.as_ref().expect("tap action");
        assert_eq!(action.qmk_code().as_deref(), Some("LALT(KC_TAB)"));
        assert_eq!(crate::legend::action_label(action), "⌥Tab");
    }

    #[test]
    fn synthesized_keys_preserve_disabled_transparent_and_dual_role_codes() {
        let disabled = synth_key("KC_NO");
        assert_eq!(
            disabled.tap.as_ref().and_then(|a| a.code.as_deref()),
            Some("KC_NO")
        );

        let transparent = synth_key("KC_TRNS");
        assert_eq!(
            transparent.tap.as_ref().and_then(|a| a.code.as_deref()),
            Some("KC_TRNS")
        );

        let layer_tap = synth_key("LT(2,KC_A)");
        assert_eq!(
            layer_tap.tap.as_ref().and_then(|a| a.code.as_deref()),
            Some("KC_A")
        );
        assert_eq!(layer_tap.hold.as_ref().and_then(|a| a.layer), Some(2));

        let mod_tap = synth_key("LGUI_T(KC_SPC)");
        assert_eq!(
            mod_tap.tap.as_ref().and_then(|a| a.code.as_deref()),
            Some("KC_SPC")
        );
        assert_eq!(
            mod_tap.hold.as_ref().and_then(|a| a.code.as_deref()),
            Some("KC_LGUI")
        );
    }
}

#[cfg(test)]
mod slot_tests {
    use super::hold_wrap;

    #[test]
    fn hold_wraps_layers_and_mods() {
        assert_eq!(hold_wrap("MO(2)", "KC_A").as_deref(), Some("LT(2,KC_A)"));
        assert_eq!(
            hold_wrap("KC_LSFT", "KC_A").as_deref(),
            Some("LSFT_T(KC_A)")
        );
        assert_eq!(
            hold_wrap("KC_RGUI", "KC_SPC").as_deref(),
            Some("RGUI_T(KC_SPC)")
        );
        assert_eq!(hold_wrap("KC_MEH", "KC_1").as_deref(), Some("MEH_T(KC_1)"));
        // Not expressible as MT/LT → None (caller warns + falls back to tap).
        assert_eq!(hold_wrap("KC_B", "KC_A"), None);
        assert_eq!(hold_wrap("OSL(1)", "KC_A"), None);
    }
}

#[cfg(test)]
mod layer_ref_tests {
    use super::renumber_layer_ref;

    #[test]
    fn rewrites_refs_after_deleted_layer() {
        assert_eq!(renumber_layer_ref("MO(3)", 2), "MO(2)");
        assert_eq!(renumber_layer_ref("MO(1)", 2), "MO(1)");
        assert_eq!(renumber_layer_ref("MO(2)", 2), "KC_NO");
        assert_eq!(renumber_layer_ref("LT(2,KC_A)", 2), "KC_A");
        assert_eq!(renumber_layer_ref("LT(4,KC_A)", 2), "LT(3,KC_A)");
        assert_eq!(renumber_layer_ref("OSL(5)", 2), "OSL(4)");
        assert_eq!(renumber_layer_ref("DF(3)", 2), "DF(2)");
        assert_eq!(renumber_layer_ref("KC_A", 2), "KC_A");
        assert_eq!(renumber_layer_ref("LSFT_T(KC_A)", 2), "LSFT_T(KC_A)");
        assert_eq!(
            renumber_layer_ref("KC_A\nMO(2)\nMO(4)", 2),
            "KC_A\nKC_NO\nMO(3)"
        );
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

#[cfg(test)]
mod live_layout_tests {
    use super::live_board_size;

    #[test]
    fn live_board_respects_width_and_height_budgets() {
        let (w, h) = live_board_size(900.0, 600.0, 14.0, 5.0);
        assert!(w <= 900.0);
        assert!(h <= 600.0 - 88.0);

        let (narrow_w, narrow_h) = live_board_size(620.0, 900.0, 14.0, 5.0);
        assert!(narrow_w <= 620.0);
        assert!(narrow_h < h);
    }
}
