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
    Connected { model: String, serial: String },
    LayoutLoaded(Box<Layout>),
    Hid(Event),
    Disconnected,
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

/// Owns the keyboard on a background thread. Reconnects forever.
pub fn spawn_device_worker(
    serial: Option<String>,
    ctx: egui::Context,
) -> (Receiver<DevEvent>, Sender<KbCmd>) {
    let (etx, erx) = channel::<DevEvent>();
    let (cmd_tx, cmd_rx) = channel::<KbCmd>();
    std::thread::spawn(move || device_loop(serial, etx, cmd_rx, ctx));
    (erx, cmd_tx)
}

fn device_loop(
    serial: Option<String>,
    etx: Sender<DevEvent>,
    cmd_rx: Receiver<KbCmd>,
    ctx: egui::Context,
) {
    let mut was_connected = true; // force an initial Disconnected if nothing is there
    loop {
        let kb = match Keyboard::open(serial.as_deref()) {
            Ok(kb) => kb,
            Err(_) => {
                if was_connected {
                    if etx.send(DevEvent::Disconnected).is_err() {
                        return; // app is gone
                    }
                    ctx.request_repaint();
                    was_connected = false;
                }
                while cmd_rx.try_recv().is_ok() {} // drop stale commands
                std::thread::sleep(RESCAN_INTERVAL);
                continue;
            }
        };

        let initial_layer = kb.pair().ok().flatten();
        // ZSA packs the layout/firmware id into the USB serial; `fw_version()`
        // returns that serial string (later parsed by `LayoutId::from_serial`).
        let serial = kb.fw_version().unwrap_or_default();
        if etx
            .send(DevEvent::Connected { model: kb.info.model().to_string(), serial: serial.clone() })
            .is_err()
        {
            return;
        }
        if let Some(layer) = initial_layer {
            let _ = etx.send(DevEvent::Hid(Event::Layer(layer)));
        }
        ctx.request_repaint();

        // Legends are best-effort and may hit the network; the cache makes
        // reconnects instant.
        if let Ok(id) = LayoutId::from_serial(&serial) {
            if let Ok(layout) = fetch_layout(&id, "voyager", false) {
                if etx.send(DevEvent::LayoutLoaded(Box::new(layout))).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        }

        // Per-connection RGB takeover state (moved here from the anim thread so
        // frames coalesce at the single point that actually writes HID).
        let n_leds = crate::geometry::voyager().len();
        let mut took_over = false;
        let mut last_frame: Vec<[u8; 3]> = vec![[0, 0, 0]; n_leds];

        loop {
            // Flush pending commands FIRST so LED frames aren't held behind the
            // read timeout. Discrete commands run in order; frames coalesce to
            // the newest (older frames dropped) so the queue can't back up.
            let mut latest_frame: Option<Arc<Vec<[u8; 3]>>> = None;
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    KbCmd::SetFrame(f) => latest_frame = Some(f),
                    KbCmd::RgbRelease => {
                        let _ = kb.send(Command::RgbControl(false));
                        took_over = false;
                        last_frame.iter_mut().for_each(|c| *c = [0, 0, 0]);
                        latest_frame = None; // a release supersedes a pending frame
                    }
                    other => {
                        if let Some(p) = other.to_protocol() {
                            let _ = kb.send(p);
                        }
                    }
                }
            }
            if let Some(frame) = latest_frame {
                if frame.len() == n_leds {
                    if !took_over {
                        // Prime with one SetRgbLedAll of the dominant color so the
                        // takeover doesn't flash black while diffs stream in.
                        let approx = crate::gui::rgb_anim::dominant(&frame);
                        let _ = kb.send(Command::SetRgbLedAll { r: approx[0], g: approx[1], b: approx[2] });
                        last_frame = vec![approx; n_leds];
                        took_over = true;
                    }
                    for (i, c) in frame.iter().enumerate() {
                        if *c != last_frame[i] {
                            let _ = kb.send(Command::SetRgbLed { led: i as u8, r: c[0], g: c[1], b: c[2] });
                        }
                    }
                    last_frame.copy_from_slice(&frame);
                }
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
                    if etx.send(DevEvent::Disconnected).is_err() {
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
        let fw = match crate::cmd_flash::acquire_firmware(target.as_deref(), latest) {
            Ok(fw) => fw,
            Err(e) => return send(FlashState::Failed(format!("{e:#}"))),
        };
        send(FlashState::WaitingForBootloader);

        // zapp-core's timeout only fires on USB events; poll our own deadline
        // and the cancel flag so a never-pressed reset can't hang us. We hand
        // the watcher a timeout of its own (longer than our deadline, so our
        // friendlier message wins) purely so a canceled/timed-out attempt's
        // thread self-terminates on the next USB event instead of leaking.
        let (btx, brx) = channel();
        std::thread::spawn(move || {
            let watcher_timeout = Duration::from_secs(360);
            let _ = btx.send(zapp_core::device::wait_for_bootloader(
                Some(watcher_timeout),
                |_| {},
            ));
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        let dev = loop {
            if canceled() {
                return send(FlashState::Failed("canceled".into()));
            }
            match brx.recv_timeout(Duration::from_millis(300)) {
                Ok(Ok(dev)) => break dev,
                Ok(Err(e)) => return send(FlashState::Failed(format!("bootloader detection: {e}"))),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if std::time::Instant::now() >= deadline {
                        return send(FlashState::Failed(
                            "no bootloader within 5 minutes - reset button not pressed?".into(),
                        ));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return send(FlashState::Failed("bootloader watcher stopped".into()))
                }
            }
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
                P::Complete => FlashState::Done,
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
}

impl Drop for AutolayerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
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
    std::thread::spawn(move || {
        let mut last_bundle = String::new();
        let mut active_rule_layer: Option<u8> = None;
        while !stop_t.load(Ordering::SeqCst) {
            if let Some(bundle) = crate::cmd_autolayer::frontmost_bundle_id() {
                if bundle != last_bundle {
                    let target = rules
                        .iter()
                        .find(|r| bundle.contains(r.bundle.as_str()))
                        .map(|r| r.layer);
                    match (target, active_rule_layer) {
                        (Some(layer), current) if current != Some(layer) => {
                            let _ = cmd_tx.send(KbCmd::SetLayer { on: true, layer });
                            active_rule_layer = Some(layer);
                            ctx.request_repaint();
                        }
                        (None, Some(prev)) => {
                            let _ = cmd_tx.send(KbCmd::SetLayer { on: false, layer: prev });
                            active_rule_layer = None;
                            ctx.request_repaint();
                        }
                        _ => {}
                    }
                    last_bundle = bundle;
                }
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        if let Some(prev) = active_rule_layer {
            let _ = cmd_tx.send(KbCmd::SetLayer { on: false, layer: prev });
        }
    });
    AutolayerHandle { stop }
}
