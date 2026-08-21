//! `keyjitsu overlay` - hold (or toggle with) a key of your choice and a
//! transparent, click-through live picture of the keyboard appears on screen:
//! current layer's legends, keys lighting up as you press them.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::config;
use crate::device::Keyboard;
use crate::geometry;
use crate::legend::{labels_for, KeyLabels};
use crate::macos_overlay::{Overlay, OverlayPosition};
use crate::oryx_api::{fetch_layout, Layout, LayoutId};
use crate::protocol::Event;

pub struct Opts {
    pub key: Option<(u8, u8)>,
    pub pick: bool,
    pub toggle: bool,
    pub show_on_layers: Vec<u8>,
    pub opacity: f64,
    pub scale: f64,
    pub position: OverlayPosition,
}

/// `--key 4,0` → matrix (row, col); see `keyjitsu watch` output for values.
pub fn parse_matrix_key(s: &str) -> Result<(u8, u8), String> {
    let (r, c) = s.split_once(',').ok_or_else(|| format!("expected ROW,COL - got {s:?}"))?;
    let row = r.trim().parse().map_err(|_| format!("bad row {r:?}"))?;
    let col = c.trim().parse().map_err(|_| format!("bad col {c:?}"))?;
    Ok((row, col))
}

pub fn run(serial: Option<&str>, opts: Opts) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    let mut active_layer = kb.pair()?.unwrap_or(0);

    // Trigger key: flag > interactive pick > saved config.
    let mut cfg = config::load();
    let trigger: Option<(u8, u8)> = if let Some(k) = opts.key {
        cfg.overlay_trigger = Some([k.0, k.1]);
        config::save(&cfg).ok();
        Some(k)
    } else if opts.pick {
        println!("Press the key on the keyboard you want as the overlay trigger…");
        let k = wait_for_keydown(&kb)?;
        println!("Trigger set to row {}, col {} (saved).", k.0, k.1);
        cfg.overlay_trigger = Some([k.0, k.1]);
        config::save(&cfg).ok(); // best-effort, like the --key path; the overlay still runs
        Some(k)
    } else {
        cfg.overlay_trigger.map(|[r, c]| (r, c))
    };

    if trigger.is_none() && opts.show_on_layers.is_empty() {
        bail!(
            "no trigger key configured - run with --pick (press a key to choose it),\n\
             or --key ROW,COL, or use --show-on-layers 1,2 for layer-based showing"
        );
    }

    // Legends: best effort, same as `live`.
    let layout = kb
        .fw_version()
        .ok()
        .and_then(|fw| LayoutId::from_serial(&fw).ok())
        .and_then(|id| fetch_layout(&id, "voyager", false).ok());
    if layout.is_none() {
        eprintln!("note: no Oryx layout available - keys will light up without legends");
    }

    let geo = geometry::voyager();
    let mut overlay = Overlay::new(geo, opts.scale, opts.opacity, opts.position);
    apply_layer(&overlay, layout.as_ref(), active_layer);

    match (trigger, opts.toggle) {
        (Some((r, c)), false) => {
            println!("overlay: hold the trigger key (row {r}, col {c}) to show. Ctrl+C quits.")
        }
        (Some((r, c)), true) => {
            println!("overlay: tap the trigger key (row {r}, col {c}) to toggle. Ctrl+C quits.")
        }
        (None, _) => println!(
            "overlay: visible on layer(s) {:?}. Ctrl+C quits.",
            opts.show_on_layers
        ),
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;

    let mut held = false;
    let mut toggled = false;

    while running.load(Ordering::SeqCst) {
        // Serve AppKit (≤30 ms) - this is also the loop's tick.
        overlay.pump();

        // Drain pending HID events without blocking.
        while let Some(ev) = kb.read_event(Duration::ZERO)? {
            match ev {
                Event::Layer(n) => {
                    active_layer = n;
                    apply_layer(&overlay, layout.as_ref(), n);
                }
                Event::KeyDown { col, row } => {
                    if trigger == Some((row, col)) {
                        if opts.toggle {
                            toggled = !toggled;
                        } else {
                            held = true;
                        }
                    }
                    if let Some(idx) = geo.key_index(row, col) {
                        overlay.set_pressed(idx, true);
                    }
                }
                Event::KeyUp { col, row } => {
                    if trigger == Some((row, col)) {
                        held = false;
                    }
                    if let Some(idx) = geo.key_index(row, col) {
                        overlay.set_pressed(idx, false);
                    }
                }
                _ => {}
            }
        }

        let visible = held || toggled || opts.show_on_layers.contains(&active_layer);
        overlay.set_visible(visible);
    }

    overlay.set_visible(false);
    kb.disconnect();
    Ok(())
}

fn wait_for_keydown(kb: &Keyboard) -> Result<(u8, u8)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(Event::KeyDown { col, row }) = kb.read_event(Duration::from_millis(100))? {
            // Swallow the matching keyup so it doesn't confuse the main loop.
            let _ = kb.read_event(Duration::from_millis(200));
            return Ok((row, col));
        }
    }
    bail!("no key was pressed within 30s")
}

fn apply_layer(overlay: &Overlay, layout: Option<&Layout>, layer_no: u8) {
    let geo = geometry::voyager();
    let layer = layout.and_then(|l| l.revision.layers.iter().find(|la| la.position == layer_no));
    let (labels, glow, title) = match layer {
        Some(layer) => (
            layer.keys.iter().map(labels_for).collect::<Vec<_>>(),
            layer.keys.iter().map(|k| k.glow_color.as_deref().and_then(parse_hex)).collect(),
            format!(
                "L{layer_no} · {}",
                layer.title.as_deref().unwrap_or("(unnamed)")
            ),
        ),
        None => (
            (0..geo.len()).map(|_| KeyLabels { tap: String::new(), hold: None }).collect(),
            vec![None; geo.len()],
            format!("L{layer_no}"),
        ),
    };
    overlay.set_labels(&labels, &glow, &title);
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    crate::geometry::parse_hex_rgb(s)
}
