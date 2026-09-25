//! Background threads for the GUI: the device loop (auto-reconnect, event
//! streaming, command forwarding), the flash job and the autolayer watcher.
//! Every message that changes UI state triggers `ctx.request_repaint()`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::device::Keyboard;
use crate::oryx_api::{fetch_layout, Layout, LayoutId};
use crate::protocol::{Command, Event};

/// How long to wait between rescans while the keyboard is unplugged.
const RESCAN_INTERVAL: Duration = Duration::from_millis(900);
/// Per-iteration HID read timeout - short so queued LED frames aren't held
/// behind it (see the command-flush comment in `device_loop`).
const READ_TIMEOUT: Duration = Duration::from_millis(25);

pub enum DevEvent {
    Connected { model: String, serial: String, generation: u64 },
    LayoutLoaded { generation: u64, layout: Box<Layout> },
    Hid(Event),
    Disconnected { generation: u64 },
}

#[derive(Debug, Clone)]
pub enum KbCmd {
    SetLayer { on: bool, layer: u8 },
    // SetRgbLed / SetRgbLedAll auto-enable RGB control in the firmware, so no
    // explicit takeover command is needed (or wanted - see the priming logic).
    SetRgbLed { led: u8, r: u8, g: u8, b: u8 },
    SetRgbLedAll { r: u8, g: u8, b: u8 },
    /// A full LED frame from the animation engine. The device loop keeps only
    /// the NEWEST pending frame (older ones are dropped) and sends just the
    /// per-key diff - so a slow HID write can never make the command queue
    /// grow without bound or lag ever further behind.
    SetFrame(Arc<Vec<[u8; 3]>>),
    RgbRelease,
}

impl KbCmd {
    /// The discrete (non-frame) commands map 1:1 to a protocol command.
    fn to_protocol(&self) -> Option<Command> {
        match self {
            KbCmd::SetLayer { on, layer } => Some(Command::SetLayer { on: *on, layer: *layer }),
            KbCmd::SetRgbLed { led, r, g, b } => Some(Command::SetRgbLed { led: *led, r: *r, g: *g, b: *b }),
            KbCmd::SetRgbLedAll { r, g, b } => Some(Command::SetRgbLedAll { r: *r, g: *g, b: *b }),
            KbCmd::RgbRelease => Some(Command::RgbControl(false)),
            KbCmd::SetFrame(_) => None, // handled with coalescing + diffing
        }
    }
}

pub struct DeviceWorkerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DeviceWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Owns the keyboard on a background thread and stops cleanly with the GUI.
pub fn spawn_device_worker(
    serial: Option<String>,
    ctx: egui::Context,
) -> (Receiver<DevEvent>, Sender<KbCmd>, DeviceWorkerHandle) {
    let (etx, erx) = channel::<DevEvent>();
    let (cmd_tx, cmd_rx) = channel::<KbCmd>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let thread = std::thread::spawn(move || device_loop(serial, etx, cmd_rx, ctx, stop_t));
    (
        erx,
        cmd_tx,
        DeviceWorkerHandle {
            stop,
            thread: Some(thread),
        },
    )
}

fn sleep_until_rescan(stop: &AtomicBool) -> bool {
    let mut slept = Duration::ZERO;
    while slept < RESCAN_INTERVAL {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        let step = (RESCAN_INTERVAL - slept).min(Duration::from_millis(50));
        std::thread::sleep(step);
        slept += step;
    }
    false
}

fn device_loop(
    serial: Option<String>,
    etx: Sender<DevEvent>,
    cmd_rx: Receiver<KbCmd>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
) {
    let mut was_connected = true; // force an initial Disconnected if nothing is there
    let mut generation = 0u64;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let kb = match Keyboard::open(serial.as_deref()) {
            Ok(kb) => kb,
            Err(_) => {
                if was_connected {
                    if etx.send(DevEvent::Disconnected { generation }).is_err() {
                        return; // app is gone
                    }
                    ctx.request_repaint();
                    was_connected = false;
                }
                while cmd_rx.try_recv().is_ok() {} // drop stale commands
                if sleep_until_rescan(&stop) {
                    return;
                }
                continue;
            }
        };

        let mut initial_events = Vec::new();
        if kb
            .pair_with_events(|event| initial_events.push(event))
            .is_err()
        {
            kb.disconnect();
            if was_connected && etx.send(DevEvent::Disconnected { generation }).is_err() {
                return;
            }
            ctx.request_repaint();
            was_connected = false;
            if sleep_until_rescan(&stop) {
                return;
            }
            continue;
        }

        let serial = match kb.fw_version_with_events(|event| initial_events.push(event)) {
            Ok(serial) => serial,
            Err(_) => {
                kb.disconnect();
                if was_connected && etx.send(DevEvent::Disconnected { generation }).is_err() {
                    return;
                }
                ctx.request_repaint();
                was_connected = false;
                if sleep_until_rescan(&stop) {
                    return;
                }
                continue;
            }
        };
        generation = generation.wrapping_add(1);
        let connection_generation = generation;
        if etx
            .send(DevEvent::Connected {
                model: kb.info.model().to_string(),
                serial: serial.clone(),
                generation: connection_generation,
            })
            .is_err()
        {
            return;
        }
        was_connected = true;
        for event in initial_events {
            if etx.send(DevEvent::Hid(event)).is_err() {
                kb.disconnect();
                return;
            }
        }
        ctx.request_repaint();

        // Layout loading may hit the network. Keep it off the HID loop so
        // key/layer events continue flowing while Oryx is slow or unavailable.
        if let Ok(id) = LayoutId::from_serial(&serial) {
            let layout_tx = etx.clone();
            let layout_ctx = ctx.clone();
            std::thread::spawn(move || {
                if let Ok(layout) = fetch_layout(&id, "voyager", false) {
                    let _ = layout_tx.send(DevEvent::LayoutLoaded {
                        generation: connection_generation,
                        layout: Box::new(layout),
                    });
                    layout_ctx.request_repaint();
                }
            });
        }

        // Per-connection RGB takeover state (moved here from the anim thread so
        // frames coalesce at the single point that actually writes HID).
        let n_leds = crate::geometry::voyager().len();
        let mut took_over = false;
        let mut last_frame: Vec<[u8; 3]> = vec![[0, 0, 0]; n_leds];

        loop {
            if stop.load(Ordering::SeqCst) {
                // App::drop stops Autolayer and queues its layer release before
                // DeviceWorkerHandle::drop sets this flag. Drain only safety
                // releases here; never apply stale RGB/layer-on work while
                // shutting down.
                let mut rgb_released = false;
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        KbCmd::SetLayer { on: false, layer } => {
                            let _ = kb.send(Command::SetLayer { on: false, layer });
                        }
                        KbCmd::RgbRelease => {
                            let _ = kb.send(Command::RgbControl(false));
                            rgb_released = true;
                        }
                        _ => {}
                    }
                }
                if took_over && !rgb_released {
                    let _ = kb.send(Command::RgbControl(false));
                }
                kb.disconnect();
                return;
            }
            // Flush pending commands FIRST so LED frames aren't held behind the
            // read timeout. Discrete commands run in order; frames coalesce to
            // the newest (older frames dropped) so the queue can't back up.
            let mut latest_frame: Option<Arc<Vec<[u8; 3]>>> = None;
            let mut write_failed = false;
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    KbCmd::SetFrame(f) => latest_frame = Some(f),
                    KbCmd::RgbRelease => {
                        if kb.send(Command::RgbControl(false)).is_err() {
                            write_failed = true;
                            break;
                        }
                        took_over = false;
                        last_frame.iter_mut().for_each(|c| *c = [0, 0, 0]);
                        latest_frame = None;
                    }
                    other => {
                        if let Some(p) = other.to_protocol() {
                            if kb.send(p).is_err() {
                                write_failed = true;
                                break;
                            }
                        }
                    }
                }
            }
            if !write_failed {
                if let Some(frame) = latest_frame {
                    if frame.len() == n_leds {
                        if !took_over {
                            let approx = crate::gui::rgb_anim::dominant(&frame);
                            if kb.send(Command::SetRgbLedAll { r: approx[0], g: approx[1], b: approx[2] }).is_err() {
                                write_failed = true;
                            } else {
                                last_frame = vec![approx; n_leds];
                                took_over = true;
                            }
                        }
                        if !write_failed {
                            for (i, c) in frame.iter().enumerate() {
                                if *c != last_frame[i]
                                    && kb.send(Command::SetRgbLed { led: i as u8, r: c[0], g: c[1], b: c[2] }).is_err()
                                {
                                    write_failed = true;
                                    break;
                                }
                            }
                        }
                        if !write_failed {
                            last_frame.copy_from_slice(&frame);
                        }
                    }
                }
            }
            if write_failed {
                if etx.send(DevEvent::Disconnected { generation }).is_err() {
                    return;
                }
                ctx.request_repaint();
                was_connected = false;
                break;
            }
            match kb.read_event(READ_TIMEOUT) {
                Ok(Some(ev)) => {
                    if etx.send(DevEvent::Hid(ev)).is_err() {
                        kb.disconnect();
                        return;
                    }
                    ctx.request_repaint();
                }
                Ok(None) => {}
                Err(_) => {
                    // Unplugged (or flashing started); go back to scanning.
                    if etx.send(DevEvent::Disconnected { generation }).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                    was_connected = false;
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Flash job

#[derive(Debug, Clone)]
pub enum FlashState {
    Downloading,
    WaitingForBootloader,
    Working { phase: &'static str, fraction: f32 },
    Done,
    Failed(String),
}

pub fn spawn_flash(
    target: Option<String>,
    latest: bool,
    current_serial: Option<String>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) -> Receiver<FlashState> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let send = |s: FlashState| {
            let _ = tx.send(s);
            ctx.request_repaint();
        };
        let canceled = || cancel.load(Ordering::SeqCst);
        send(FlashState::Downloading);
        if canceled() {
            return send(FlashState::Failed("canceled".into()));
        }
        let fw = match crate::cmd_flash::acquire_firmware(
            target.as_deref(),
            latest,
            current_serial.as_deref(),
        ) {
            Ok(fw) => fw,
            Err(e) => return send(FlashState::Failed(format!("{e:#}"))),
        };
        send(FlashState::WaitingForBootloader);

        let dev = match crate::cmd_flash::wait_for_bootloader(
            Duration::from_secs(300),
            Some(&cancel),
            |_| {},
        ) {
            Ok(dev) => dev,
            Err(e) => return send(FlashState::Failed(format!("{e:#}"))),
        };

        let res = zapp_core::flash::flash_device(&dev, &fw, &|p| {
            use zapp_core::flash::FlashProgress as P;
            let state = match p {
                P::Erasing { bytes_erased, total_bytes } => FlashState::Working {
                    phase: "Erasing",
                    fraction: frac(bytes_erased, total_bytes),
                },
                P::Writing { bytes_written, total_bytes } => FlashState::Working {
                    phase: "Writing",
                    fraction: frac(bytes_written, total_bytes),
                },
                P::Resetting => FlashState::Working { phase: "Restarting keyboard", fraction: 1.0 },
                // zapp also returns Ok(()) after this callback. Keep the callback
                // as progress only and emit the terminal Done exactly once below.
                P::Complete => FlashState::Working { phase: "Complete", fraction: 1.0 },
            };
            let _ = tx.send(state);
            ctx.request_repaint();
        });
        match res {
            Ok(()) => send(FlashState::Done),
            Err(e) => send(FlashState::Failed(format!("flashing failed: {e}"))),
        }
    });
    rx
}

fn frac(done: usize, total: usize) -> f32 {
    if total == 0 {
        1.0
    } else {
        done as f32 / total as f32
    }
}

// ---------------------------------------------------------------------------
// Autolayer watcher (macOS only)

pub struct AutolayerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AutolayerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "macos")]
pub fn spawn_autolayer(
    rules: Vec<crate::config::AutolayerRule>,
    cmd_tx: Sender<KbCmd>,
    ctx: egui::Context,
) -> AutolayerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut last_bundle = String::new();
        let mut active_rule_layer: Option<u8> = None;
        while !stop_t.load(Ordering::SeqCst) {
            if let Some(bundle) = crate::cmd_autolayer::frontmost_bundle_id() {
                if bundle != last_bundle {
                    let target = rules
                        .iter()
                        .find(|r| crate::cmd_autolayer::rule_matches(&bundle, &r.bundle))
                        .map(|r| r.layer);
                    let (release, enable) =
                        crate::cmd_autolayer::layer_transition(active_rule_layer, target);
                    if let Some(prev) = release {
                        if cmd_tx.send(KbCmd::SetLayer { on: false, layer: prev }).is_err() {
                            return;
                        }
                    }
                    if let Some(layer) = enable {
                        if cmd_tx.send(KbCmd::SetLayer { on: true, layer }).is_err() {
                            return;
                        }
                    }
                    if release.is_some() || enable.is_some() {
                        active_rule_layer = target;
                        ctx.request_repaint();
                    }
                    last_bundle = bundle;
                }
            }
            for _ in 0..8 {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if let Some(prev) = active_rule_layer {
            if cmd_tx.send(KbCmd::SetLayer { on: false, layer: prev }).is_err() {
                return;
            }
        }
    });
    AutolayerHandle {
        stop,
        thread: Some(thread),
    }
}
