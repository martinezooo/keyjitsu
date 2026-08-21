#[cfg(target_os = "macos")]
mod cmd_autolayer;
mod cmd_flash;
#[cfg(target_os = "macos")]
mod cmd_guard;
mod cmd_live;
#[cfg(target_os = "macos")]
mod cmd_overlay;
mod config;
mod device;
mod gui;
mod keycodes;
mod keymap;
mod localbuild;
mod perf;
mod shortcuts;
#[cfg(target_os = "macos")]
mod macos_display;
#[cfg(target_os = "macos")]
mod macos_kb;
#[cfg(target_os = "macos")]
mod macos_lockwatch;
#[cfg(target_os = "macos")]
mod macos_overlay;
mod geometry;
mod heatmap;
mod legend;
mod oryx_api;
mod protocol;
mod ui;

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use device::Keyboard;
use heatmap::{normalize, HeatmapStore};
use oryx_api::{fetch_layout, Layout, LayoutId};
use protocol::{Command, Event, PROTOCOL_VERSION};
use ui::KeyboardWidget;

#[derive(Parser)]
#[command(
    name = "keyjitsu",
    version,
    about = "Serverless CLI for managing the ZSA Voyager keyboard",
    long_about = "Talks directly to the keyboard over raw HID - no Keymapp, no daemon.\n\
                  Note: quit Keymapp first; the HID channel is exclusive."
)]
struct Cli {
    /// Pick a keyboard by (part of) its USB serial number
    #[arg(long, global = true)]
    serial: Option<String>,

    /// With no subcommand, the GUI opens.
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the windowed app (default when no subcommand is given)
    Gui,
    /// List connected ZSA keyboards
    List,
    /// Show connection, firmware and layer status
    Status,
    /// Stream key/layer events to the terminal (Ctrl+C to stop)
    Watch {
        /// Emit one JSON object per line instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// Full-screen live view: layers, key presses, heatmap recording
    Live,
    /// Print the keyboard's layout with legends fetched from Oryx
    Layout {
        #[command(flatten)]
        source: LayoutSource,
        /// Show only this layer
        #[arg(long)]
        layer: Option<u8>,
        /// Bypass the on-disk layout cache
        #[arg(long)]
        refresh: bool,
        /// Dump the layout as JSON instead of drawing it
        #[arg(long)]
        json: bool,
    },
    /// Show or reset collected key-press statistics
    Heatmap {
        #[command(subcommand)]
        action: HeatmapAction,
    },
    /// Activate or release a layer
    Layer {
        #[command(subcommand)]
        action: LayerAction,
    },
    /// Control per-key RGB (takes over the LEDs until released)
    Rgb {
        #[command(subcommand)]
        action: RgbAction,
    },
    /// Control the status LEDs (0-5)
    StatusLed {
        #[command(subcommand)]
        action: StatusLedAction,
    },
    /// Adjust LED brightness
    Brightness {
        direction: Direction,
        /// Repeat the step this many times
        #[arg(long, default_value_t = 1)]
        steps: u8,
    },
    /// Build firmware locally from your layout + key remaps (headless QMK)
    BuildLocal {
        /// Oryx revision id (default: read from the connected keyboard's serial)
        #[arg(long)]
        rev: Option<String>,
        /// Remap, repeatable: "LAYER,POSITION=KEYCODE" (position = LAYOUT index)
        #[arg(long = "set")]
        sets: Vec<String>,
        /// Tap dance, repeatable: "LAYER,POSITION=TAP,HOLD,DOUBLE,TAPHOLD"
        /// (use '-' for an empty slot, e.g. "0,1=KC_1,MO(2),KC_F1,-")
        #[arg(long = "dance")]
        dance: Vec<String>,
        /// Append a new layer, repeatable: "INDEX" (empty) or
        /// "INDEX:POS=CODE,POS=CODE" (e.g. "3:19=KC_7,20=KC_8")
        #[arg(long = "new-layer")]
        new_layer: Vec<String>,
    },
    /// Flash firmware (file, Oryx URL, or --latest for the current layout)
    Flash {
        /// Firmware file (.bin/.hex) or Oryx layout URL
        target: Option<String>,
        /// Update to the newest Oryx revision of the layout on the keyboard
        #[arg(long, conflicts_with = "target")]
        latest: bool,
        /// Seconds to wait for the bootloader after you press reset
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Disable the Mac's built-in keyboard while a ZSA keyboard is connected
    #[cfg(target_os = "macos")]
    Guard,
    /// Transparent on-screen live keyboard HUD, summoned by a key you choose
    #[cfg(target_os = "macos")]
    Overlay {
        /// Trigger key as matrix "ROW,COL" (values shown by `keyjitsu watch`)
        #[arg(long, value_parser = cmd_overlay::parse_matrix_key)]
        key: Option<(u8, u8)>,
        /// Choose the trigger by pressing it on the keyboard (saved for later runs)
        #[arg(long, conflicts_with = "key")]
        pick: bool,
        /// Tap the trigger to toggle instead of hold-to-show
        #[arg(long)]
        toggle: bool,
        /// Also stay visible while any of these layers is active (e.g. 1,2)
        #[arg(long, value_delimiter = ',')]
        show_on_layers: Vec<u8>,
        /// Keycap background opacity, 0.0-1.0
        #[arg(long, default_value_t = 0.72)]
        opacity: f64,
        /// Size multiplier for the on-screen keyboard
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Screen position
        #[arg(long, value_enum, default_value_t = macos_overlay::OverlayPosition::Bottom)]
        position: macos_overlay::OverlayPosition,
    },
    /// Smart layers: switch layers based on the frontmost app
    #[cfg(target_os = "macos")]
    Autolayer {
        /// Mapping like com.apple.Terminal=2 (substring match; repeatable)
        #[arg(long = "rule", required = true, value_parser = cmd_autolayer::parse_rule)]
        rules: Vec<(String, u8)>,
        /// How often to check the frontmost app, in milliseconds
        #[arg(long, default_value_t = 400)]
        poll_ms: u64,
    },
}

/// Where to take the layout definition from. Defaults to the id embedded in
/// the connected keyboard's firmware.
#[derive(Args)]
struct LayoutSource {
    /// Oryx layout URL (https://configure.zsa.io/voyager/layouts/…)
    #[arg(long)]
    url: Option<String>,
    /// Oryx layout hash id
    #[arg(long, conflicts_with = "url")]
    hash: Option<String>,
    /// Revision id, used with --hash ("latest" = newest edit)
    #[arg(long, default_value = "latest")]
    rev: String,
}

impl LayoutSource {
    fn resolve(&self, serial: Option<&str>) -> Result<LayoutId> {
        if let Some(url) = &self.url {
            return LayoutId::from_url(url);
        }
        if let Some(hash) = &self.hash {
            return Ok(LayoutId { hash: hash.clone(), revision: self.rev.clone() });
        }
        let kb = Keyboard::open(serial)?;
        kb.pair()?;
        let fw = kb.fw_version()?;
        kb.disconnect();
        LayoutId::from_serial(&fw)
    }
}

#[derive(Subcommand)]
enum HeatmapAction {
    /// Render the heatmap over the keyboard layout
    Show {
        #[command(flatten)]
        source: LayoutSource,
        /// Only this layer (default: every layer that has data)
        #[arg(long)]
        layer: Option<u8>,
    },
    /// Delete recorded statistics for a layout
    Reset {
        #[command(flatten)]
        source: LayoutSource,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum LayerAction {
    /// Switch to a layer (firmware `layer_move`)
    Set { layer: u8 },
    /// Turn a layer off
    Unset { layer: u8 },
}

#[derive(Subcommand)]
enum RgbAction {
    /// Set one LED: keyjitsu rgb set <LED> <R> <G> <B>
    Set { led: u8, r: u8, g: u8, b: u8 },
    /// Set every LED to one color: keyjitsu rgb all <R> <G> <B>
    All { r: u8, g: u8, b: u8 },
    /// Give LED control back to the firmware
    Release,
}

#[derive(Subcommand)]
enum StatusLedAction {
    /// Set a status LED: keyjitsu status-led set <LED> <on|off>
    Set { led: u8, state: OnOff },
    /// Give status-LED control back to the firmware
    Release,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum Direction {
    Up,
    Down,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let serial = cli.serial.as_deref();
    let command = cli.command.unwrap_or(Cmd::Gui);
    match command {
        Cmd::Gui => gui::run(serial.map(str::to_owned)),
        Cmd::List => cmd_list(),
        Cmd::Status => cmd_status(serial),
        Cmd::Watch { json } => cmd_watch(serial, json),
        Cmd::Live => cmd_live::run(serial),
        Cmd::Layout { source, layer, refresh, json } => {
            cmd_layout(serial, &source, layer, refresh, json)
        }
        Cmd::Heatmap { action } => match action {
            HeatmapAction::Show { source, layer } => cmd_heatmap_show(serial, &source, layer),
            HeatmapAction::Reset { source, yes } => cmd_heatmap_reset(serial, &source, yes),
        },
        Cmd::Layer { action } => with_paired(serial, |kb| {
            let cmd = match action {
                LayerAction::Set { layer } => Command::SetLayer { on: true, layer },
                LayerAction::Unset { layer } => Command::SetLayer { on: false, layer },
            };
            kb.send(cmd)
        }),
        Cmd::Rgb { action } => with_paired(serial, |kb| match action {
            RgbAction::Set { led, r, g, b } => kb.send(Command::SetRgbLed { led, r, g, b }),
            RgbAction::All { r, g, b } => kb.send(Command::SetRgbLedAll { r, g, b }),
            RgbAction::Release => kb.send(Command::RgbControl(false)),
        }),
        Cmd::StatusLed { action } => with_paired(serial, |kb| match action {
            StatusLedAction::Set { led, state } => kb.send(Command::SetStatusLed {
                led,
                on: matches!(state, OnOff::On),
            }),
            StatusLedAction::Release => kb.send(Command::StatusLedControl(false)),
        }),
        Cmd::BuildLocal { rev, sets, dance, new_layer } => cmd_build_local(serial, rev, &sets, &dance, &new_layer),
        Cmd::Flash { target, latest, timeout } => {
            cmd_flash::run(target.as_deref(), latest, timeout)
        }
        #[cfg(target_os = "macos")]
        Cmd::Guard => cmd_guard::run(serial),
        #[cfg(target_os = "macos")]
        Cmd::Overlay { key, pick, toggle, show_on_layers, opacity, scale, position } => {
            cmd_overlay::run(
                serial,
                cmd_overlay::Opts { key, pick, toggle, show_on_layers, opacity, scale, position },
            )
        }
        #[cfg(target_os = "macos")]
        Cmd::Autolayer { rules, poll_ms } => cmd_autolayer::run(serial, &rules, poll_ms),
        Cmd::Brightness { direction, steps } => with_paired(serial, |kb| {
            for _ in 0..steps.max(1) {
                kb.send(Command::UpdateBrightness {
                    up: matches!(direction, Direction::Up),
                })?;
            }
            Ok(())
        }),
    }
}

/// Open + pair, run `f`, then leave a short grace so the last write lands.
fn with_paired(serial: Option<&str>, f: impl FnOnce(&Keyboard) -> Result<()>) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    kb.pair()?;
    f(&kb)?;
    // One-shot commands have no ack; give the OS a beat to flush the report.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn cmd_build_local(serial: Option<&str>, rev: Option<String>, sets: &[String], dance: &[String], new_layer: &[String]) -> Result<()> {
    use anyhow::Context as _;

    let revision = match rev {
        Some(r) => r,
        None => {
            let kb = Keyboard::open(serial)?;
            kb.pair()?;
            let fw = kb.fw_version()?;
            kb.disconnect();
            oryx_api::LayoutId::from_serial(&fw)?.revision
        }
    };

    let edits = sets
        .iter()
        .map(|s| {
            let (lhs, keycode) = s.split_once('=').context("expected LAYER,POSITION=KEYCODE")?;
            let (layer, position) = lhs.split_once(',').context("expected LAYER,POSITION=KEYCODE")?;
            Ok::<_, anyhow::Error>(localbuild::KeyEdit {
                layer: layer.trim().parse().context("bad layer")?,
                position: position.trim().parse().context("bad position")?,
                keycode: keycode.trim().to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let dances = dance
        .iter()
        .map(|s| {
            let (lhs, spec) = s.split_once('=').context("expected LAYER,POSITION=TAP,HOLD,DOUBLE,TAPHOLD")?;
            let (layer, position) = lhs.split_once(',').context("expected LAYER,POSITION=…")?;
            let slot = |i: usize| -> Option<String> {
                spec.split(',')
                    .nth(i)
                    .map(str::trim)
                    .filter(|v| !v.is_empty() && *v != "-")
                    .map(str::to_string)
            };
            Ok::<_, anyhow::Error>(keymap::DanceSpec {
                layer: layer.trim().parse().context("bad layer")?,
                position: position.trim().parse().context("bad position")?,
                tap: slot(0),
                hold: slot(1),
                double_tap: slot(2),
                tap_hold: slot(3),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let new_layers = new_layer
        .iter()
        .map(|s| {
            let (idx, rest) = s.split_once(':').unwrap_or((s.as_str(), ""));
            let keys = rest
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .map(|p| {
                    let (pos, code) = p.split_once('=').context("expected POS=CODE")?;
                    Ok::<_, anyhow::Error>((pos.trim().parse::<usize>().context("bad position")?, code.trim().to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok::<_, anyhow::Error>(localbuild::NewLayer { position: idx.trim().parse().context("bad layer index")?, keys })
        })
        .collect::<Result<Vec<_>>>()?;

    let cancel = Arc::new(AtomicBool::new(false));
    let bin = localbuild::build(&revision, &edits, &dances, &new_layers, &cancel, &|line| println!("{line}"))?;
    println!("\n✓ built: {}", bin.display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let api = device::hid_api()?;
    let found = device::enumerate(&api);
    if found.is_empty() {
        println!("No ZSA keyboards found.");
        return Ok(());
    }
    for (i, k) in found.iter().enumerate() {
        println!(
            "{i}: {} (pid 0x{:04x})  serial: {}  product: {}",
            k.model(),
            k.pid,
            k.serial.as_deref().unwrap_or("-"),
            k.product.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

fn cmd_status(serial: Option<&str>) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    println!("Keyboard : {} (pid 0x{:04x})", kb.info.model(), kb.info.pid);
    if let Some(s) = &kb.info.serial {
        println!("Serial   : {s}");
    }
    let proto = kb.protocol_version()?;
    print!("Protocol : v{proto}");
    if proto != PROTOCOL_VERSION {
        print!("  (keyjitsu targets v{PROTOCOL_VERSION} - consider re-flashing recent firmware)");
    }
    println!();
    let layer = kb.pair()?;
    println!("Firmware : {}", kb.fw_version()?);
    match layer {
        Some(n) => println!("Layer    : {n}"),
        None => println!("Layer    : (not reported)"),
    }
    kb.disconnect();
    Ok(())
}

fn cmd_watch(serial: Option<&str>, json: bool) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    kb.pair()?;
    if !json {
        eprintln!(
            "Watching {} - press keys on the keyboard. Ctrl+C to stop.",
            kb.info.model()
        );
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;

    while running.load(Ordering::SeqCst) {
        let Some(ev) = kb.read_event(Duration::from_millis(200))? else {
            continue;
        };
        if json {
            println!("{}", event_json(&ev));
        } else {
            print_event(&ev);
        }
    }
    kb.disconnect();
    Ok(())
}

fn cmd_layout(
    serial: Option<&str>,
    source: &LayoutSource,
    only_layer: Option<u8>,
    refresh: bool,
    json: bool,
) -> Result<()> {
    let id = source.resolve(serial)?;
    let layout = fetch_layout(&id, "voyager", refresh)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&layout)?);
        return Ok(());
    }
    print_layout_header(&layout);
    let geo = geometry::voyager();
    let (w, h) = KeyboardWidget::size(geo);
    for layer in &layout.revision.layers {
        if only_layer.is_some_and(|n| n != layer.position) {
            continue;
        }
        println!(
            "\nLayer {} · {}",
            layer.position,
            layer.title.as_deref().unwrap_or("(unnamed)")
        );
        let widget = KeyboardWidget::new(geo, Some(layer));
        ui::print_widget(&widget, w, h);
    }
    Ok(())
}

fn print_layout_header(layout: &Layout) {
    println!(
        "{} · {}/{} · geometry: {}",
        layout.title, layout.hash_id, layout.revision.hash_id, layout.geometry
    );
    if layout.geometry != "voyager" {
        println!("warning: keyjitsu renders the Voyager geometry; this layout is for {:?}", layout.geometry);
    }
}

fn cmd_heatmap_show(serial: Option<&str>, source: &LayoutSource, only_layer: Option<u8>) -> Result<()> {
    let id = source.resolve(serial)?;
    let geo = geometry::voyager();
    let store = HeatmapStore::load(&id.hash, geo.len())?;
    if store.total_presses() == 0 {
        println!(
            "No key-press data recorded for layout {} yet.\n\
             Run `keyjitsu live` and type for a while - presses are collected there.",
            id.hash
        );
        return Ok(());
    }
    // Legends are best-effort; the heat colors work without them.
    let layout = fetch_layout(&id, "voyager", false).ok();
    if let Some(l) = &layout {
        print_layout_header(l);
    }
    println!("Total recorded presses: {}", store.total_presses());

    let (w, h) = KeyboardWidget::size(geo);
    let layers: Vec<u8> = match only_layer {
        Some(n) => vec![n],
        None => store.layers.keys().copied().collect(),
    };
    for n in layers {
        let counts = store.counts(Some(n), geo.len());
        let total: u64 = counts.iter().sum();
        let layer_def = layout
            .as_ref()
            .and_then(|l| l.revision.layers.iter().find(|la| la.position == n));
        let title = layer_def
            .and_then(|l| l.title.as_deref())
            .unwrap_or("(unnamed)");
        println!("\nLayer {n} · {title} · {total} presses");
        let mut widget = KeyboardWidget::new(geo, layer_def);
        widget.heat = Some(normalize(&counts));
        ui::print_widget(&widget, w, h);

        // Top keys list.
        let mut ranked: Vec<(usize, u64)> = counts
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, c)| *c > 0)
            .collect();
        ranked.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (rank, (idx, count)) in ranked.iter().take(8).enumerate() {
            let label = layer_def
                .and_then(|l| l.keys.get(*idx))
                .map(|k| legend::labels_for(k).tap)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    let k = geo.keys[*idx];
                    format!("r{},c{}", k.m[0], k.m[1])
                });
            println!(
                "  {:>2}. {:<6} {:>7}  {:>5.1}%",
                rank + 1,
                label,
                count,
                *count as f64 / total as f64 * 100.0
            );
        }
    }
    Ok(())
}

fn cmd_heatmap_reset(serial: Option<&str>, source: &LayoutSource, yes: bool) -> Result<()> {
    let id = source.resolve(serial)?;
    if !yes {
        print!("Delete recorded key statistics for layout {}? [y/N] ", id.hash);
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    if HeatmapStore::reset(&id.hash)? {
        println!("Heatmap data for {} deleted.", id.hash);
    } else {
        println!("No heatmap data for {} to delete.", id.hash);
    }
    Ok(())
}

fn print_event(ev: &Event) {
    // Left-pad the verb to a fixed column so the details line up.
    match ev {
        Event::Layer(n) => println!("{:<10} {n}", "layer"),
        Event::KeyDown { col, row } => println!("{:<10} row {row}, col {col}", "keydown"),
        Event::KeyUp { col, row } => println!("{:<10} row {row}, col {col}", "keyup"),
        Event::ToggleSmartLayer(n) => println!("{:<10} toggle {n}", "smart"),
        Event::TriggerSmartLayer(n) => println!("{:<10} trigger {n}", "smart"),
        Event::Error(p) => println!("{:<10} {p:?}", "fw-error"),
        other => println!("{other:?}"),
    }
}

fn event_json(ev: &Event) -> String {
    use serde_json::json;
    let v = match ev {
        Event::Layer(n) => json!({"event": "layer", "layer": n}),
        Event::KeyDown { col, row } => json!({"event": "keydown", "row": row, "col": col}),
        Event::KeyUp { col, row } => json!({"event": "keyup", "row": row, "col": col}),
        Event::ToggleSmartLayer(n) => json!({"event": "smart_layer_toggle", "layer": n}),
        Event::TriggerSmartLayer(n) => json!({"event": "smart_layer_trigger", "layer": n}),
        Event::Error(p) => json!({"event": "error", "params": p}),
        other => json!({"event": "other", "debug": format!("{other:?}")}),
    };
    v.to_string()
}
