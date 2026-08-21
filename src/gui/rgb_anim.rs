//! Host-driven RGB for the physical keyboard.
//!
//! Model: a **base** (either a constant animation, or the layout's own per-key
//! colors - "your normal RGB") plus **fx events** fired per key press. The app
//! resolves which effect a press triggers (per-key assignment from the editor,
//! falling back to the global one) and pushes an event; this thread renders
//! ~16 fps (a 60 ms frame) over HID. When nothing needs drawing the LEDs are
//! handed back to the firmware, so effects play "on top of" normal RGB and
//! disappear.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::worker::KbCmd;
use crate::geometry;

/// One rendered LED frame every 60 ms (~16 fps) - smooth enough for the
/// effects, easy on the HID bus.
const FRAME_INTERVAL: Duration = Duration::from_millis(60);
/// Keep the RGB takeover for this long after the last activity, so typing with
/// pauses doesn't flap the LEDs on/off (each re-takeover costs a repaint burst).
const IDLE_GRACE: Duration = Duration::from_millis(2000);

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Effect {
    /// No constant effect. Fx events still play (over the layout colors), and
    /// between them the firmware controls the LEDs.
    Off,
    /// Constantly reproduce the layout's per-key colors (your normal RGB).
    Layout,
    Reactive,
    Rainbow,
    Breathing,
    Heartbeat,
    Matrix,
    /// A user-built step sequence (see [`FxStep`]); the program itself lives
    /// in [`Anim::custom`] and loops forever.
    Custom,
}

impl Effect {
    pub const ALL: [(Effect, &'static str); 7] = [
        (Effect::Off, "Off (firmware controls LEDs)"),
        (Effect::Layout, "Layout colors (your normal RGB)"),
        (Effect::Reactive, "Reactive - dim wash, keys flare"),
        (Effect::Rainbow, "Rainbow wave"),
        (Effect::Breathing, "Breathing"),
        (Effect::Heartbeat, "Heartbeat"),
        (Effect::Matrix, "Matrix rain"),
    ];
    pub fn label(self) -> &'static str {
        if self == Effect::Custom {
            return "Custom sequence";
        }
        Self::ALL.iter().find(|(e, _)| *e == self).map(|(_, l)| *l).unwrap_or("Off")
    }
    pub fn uses_color(self) -> bool {
        matches!(self, Effect::Reactive | Effect::Breathing | Effect::Heartbeat)
    }
}

/// One frame of a user-built sequence: these keys, in this color, for this
/// long. A custom effect is a looped list of steps painted in FX Studio.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct FxStep {
    pub keys: Vec<u16>,
    pub color: [u8; 3],
    pub ms: u64,
}

/// A named, persisted custom effect.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct CustomFx {
    pub name: String,
    pub steps: Vec<FxStep>,
}

/// When a per-key effect fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum FxTrigger {
    Press,
    DoublePress,
}

impl FxTrigger {
    pub const ALL: [(FxTrigger, &'static str); 2] =
        [(FxTrigger::Press, "every press"), (FxTrigger::DoublePress, "double press (2×)")];
    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(t, _)| *t == self).map(|(_, l)| *l).unwrap_or("every press")
    }
}

/// A per-press effect. "This key" variants react locally; "Whole board"
/// variants light up the entire keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PressEffect {
    None,
    // this key / localized
    Flash,
    Splash,
    Ripple,
    Column,
    Row,
    Cross,
    Comet,
    Sparkle,
    // whole board
    BoardFlash,
    BoardRainbow,
    BoardWave,
    BoardPulse,
    BoardSparkle,
    BoardMatrix,
}

impl PressEffect {
    pub const ALL: [(PressEffect, &'static str); 15] = [
        (PressEffect::None, "nothing"),
        (PressEffect::Flash, "This key - flash"),
        (PressEffect::Splash, "This key - splash (+ neighbours)"),
        (PressEffect::Ripple, "This key - ripple out"),
        (PressEffect::Column, "This key - light its column"),
        (PressEffect::Row, "This key - light its row"),
        (PressEffect::Cross, "This key - paint a cross (row+column)"),
        (PressEffect::Comet, "This key - comets fly outward"),
        (PressEffect::Sparkle, "This key - sparkles around it"),
        (PressEffect::BoardFlash, "Whole board - flash"),
        (PressEffect::BoardRainbow, "Whole board - rainbow blink"),
        (PressEffect::BoardWave, "Whole board - wave sweep"),
        (PressEffect::BoardPulse, "Whole board - double pulse"),
        (PressEffect::BoardSparkle, "Whole board - sparkle storm"),
        (PressEffect::BoardMatrix, "Whole board - matrix burst"),
    ];
    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(e, _)| *e == self).map(|(_, l)| *l).unwrap_or("nothing")
    }
    pub fn uses_color(self) -> bool {
        !matches!(self, PressEffect::None | PressEffect::BoardRainbow)
    }
    fn duration(self) -> f32 {
        match self {
            PressEffect::Ripple => 0.65,
            PressEffect::Column | PressEffect::Row | PressEffect::BoardRainbow => 0.5,
            PressEffect::Cross => 0.6,
            PressEffect::Comet => 0.7,
            PressEffect::Sparkle => 0.8,
            PressEffect::BoardFlash => 0.45,
            PressEffect::BoardWave => 0.65,
            PressEffect::BoardPulse => 0.7,
            PressEffect::BoardSparkle => 0.9,
            PressEffect::BoardMatrix => 1.2,
            _ => 0.4,
        }
    }
}

/// One resolved "play this effect from this key now".
#[derive(Clone)]
pub struct FxEvent {
    pub key: usize,
    pub effect: PressEffect,
    pub color: [u8; 3],
    pub at: Instant,
    /// Per-event randomness seed (sparkles, matrix bursts).
    pub seed: u64,
    /// A user-built sequence to play ONCE instead of `effect` (per-key
    /// "on press → ★ custom" assignments from the editor).
    pub seq: Option<Arc<Vec<FxStep>>>,
}

impl FxEvent {
    fn duration(&self) -> f32 {
        match &self.seq {
            Some(steps) => (steps.iter().map(|st| st.ms.max(30)).sum::<u64>() as f32 / 1000.0).max(0.1),
            None => self.effect.duration(),
        }
    }
}

/// Cheap deterministic pseudo-random in 0..1 from two integers.
fn prand(a: u64, b: u64) -> f32 {
    let mut x = a
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(b.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 29;
    (x >> 11) as f32 / (1u64 << 53) as f32
}

/// Shared, live-editable animation state.
pub struct Anim {
    pub effect: Effect,
    pub color: [u8; 3],
    pub speed: f32,
    pub brightness: f32,
    /// Global fallback press effect (keys without their own assignment).
    pub press_effect: PressEffect,
    pub press_color: [u8; 3],
    /// The layout's per-key colors (base for `Effect::Layout` and for fx
    /// events when the constant effect is Off). Fed by the app.
    pub base: Vec<[u8; 3]>,
    /// Resolved press events queued by the app.
    pub events: Vec<FxEvent>,
    /// The program for `Effect::Custom` + its display name.
    pub custom: Vec<FxStep>,
    pub custom_name: String,
    /// The latest rendered frame (what the LEDs show right now); empty when
    /// the engine is idle. The Live view mirrors this.
    pub frame: Vec<[u8; 3]>,
}

impl Default for Anim {
    fn default() -> Self {
        Anim {
            effect: Effect::Off,
            color: [80, 170, 255],
            speed: 1.0,
            brightness: 0.85,
            press_effect: PressEffect::None,
            press_color: [255, 255, 255],
            base: Vec::new(),
            events: Vec::new(),
            custom: Vec::new(),
            custom_name: String::new(),
            frame: Vec::new(),
        }
    }
}

/// Stops the animation thread and releases the LEDs when dropped.
pub struct AnimHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for AnimHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub fn spawn(
    shared: Arc<Mutex<Anim>>,
    cmd_tx: Sender<KbCmd>,
    ctx: eframe::egui::Context,
) -> AnimHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    std::thread::spawn(move || run(shared, cmd_tx, ctx, stop_t));
    AnimHandle { stop }
}

fn run(
    shared: Arc<Mutex<Anim>>,
    cmd_tx: Sender<KbCmd>,
    ctx: eframe::egui::Context,
    stop: Arc<AtomicBool>,
) {
    let geo = geometry::voyager();
    let start = Instant::now();
    // Whether we've handed the anim engine's frames to the device (so we know
    // to send a final RgbRelease). Per-key diffing/priming lives in the device
    // loop now (frames are coalesced there).
    let mut took_over = false;
    let mut idle_since: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) {
        // Don't `.unwrap()` the lock: if the GUI thread ever panics while
        // holding this mutex it becomes poisoned, and unwrap here would kill
        // the anim thread WITHOUT running the RgbRelease cleanup below - the
        // LEDs would stay stuck in host-takeover. Recover the guard instead.
        let snapshot = match shared.lock() {
            Ok(guard) => Some(guard),
            Err(poisoned) => Some(poisoned.into_inner()),
        };
        let (effect, color, speed, brightness, base, events, custom) = {
            let Some(mut a) = snapshot else { break };
            let now = Instant::now();
            a.events.retain(|e| now.duration_since(e.at).as_secs_f32() < e.duration());
            (
                a.effect,
                a.color,
                a.speed.max(0.05),
                a.brightness.clamp(0.0, 1.0),
                a.base.clone(),
                a.events.clone(),
                a.custom.clone(),
            )
        };

        let busy = effect != Effect::Off || !events.is_empty();
        if !busy {
            let idle = *idle_since.get_or_insert_with(Instant::now);
            if took_over && idle.elapsed() > IDLE_GRACE {
                let _ = cmd_tx.send(KbCmd::RgbRelease);
                took_over = false;
                if let Ok(mut a) = shared.lock() {
                    a.frame.clear(); // UI falls back to the static layout view
                }
                ctx.request_repaint();
            }
            std::thread::sleep(FRAME_INTERVAL);
            continue;
        }
        idle_since = None;
        took_over = true;

        let t = start.elapsed().as_secs_f32();
        let frame = compute(effect, color, speed, brightness, t, &base, &events, &custom, geo);

        // Send the whole frame as ONE coalescing command; the device loop keeps
        // only the newest, primes the takeover, and sends the per-key diff.
        let _ = cmd_tx.send(KbCmd::SetFrame(Arc::new(frame.clone())));
        // Mirror the frame for the on-screen Live view and repaint it.
        if let Ok(mut a) = shared.lock() {
            a.frame = frame;
        }
        ctx.request_repaint();

        std::thread::sleep(FRAME_INTERVAL);
    }

    if took_over {
        let _ = cmd_tx.send(KbCmd::RgbRelease);
    }
}

/// Pure frame renderer - also used by FX Studio's on-screen preview.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute(
    effect: Effect,
    color: [u8; 3],
    speed: f32,
    bright: f32,
    t: f32,
    base: &[[u8; 3]],
    events: &[FxEvent],
    custom: &[FxStep],
    geo: &geometry::Geometry,
) -> Vec<[u8; 3]> {
    let n = geo.len();
    let mut f = vec![[0u8, 0, 0]; n];

    // --- base layer -------------------------------------------------------
    let layout_base = |f: &mut Vec<[u8; 3]>| {
        for (i, c) in f.iter_mut().enumerate() {
            *c = scale(base.get(i).copied().unwrap_or([0, 0, 0]), bright);
        }
    };
    match effect {
        // Off: fx events play over the layout colors so the board still looks
        // like "your RGB" for the fraction of a second we hold the LEDs.
        Effect::Off | Effect::Layout => layout_base(&mut f),
        Effect::Reactive => {
            let basec = scale(color, 0.30 * bright);
            f.iter_mut().for_each(|c| *c = basec);
        }
        Effect::Rainbow => {
            for (i, k) in geo.keys.iter().enumerate() {
                let hue = (t * 0.12 * speed + k.x / 12.0).rem_euclid(1.0);
                f[i] = hsv(hue, 1.0, bright);
            }
        }
        Effect::Breathing => {
            let phase = (t * speed * 1.6).sin() * 0.5 + 0.5;
            let v = (0.25 + 0.75 * phase) * bright;
            let c = scale(color, v);
            f.iter_mut().for_each(|x| *x = c);
        }
        Effect::Heartbeat => {
            let v = heartbeat(t * speed) * bright;
            let c = scale(color, v);
            f.iter_mut().for_each(|x| *x = c);
        }
        Effect::Matrix => {
            let rows = 5.5;
            for (i, k) in geo.keys.iter().enumerate() {
                let col = k.x.round();
                let seed = (col * 12.9898).sin().abs() * 6.0;
                let head = (t * speed * 3.0 + seed).rem_euclid(rows + 3.0);
                let d = head - k.y;
                let b = if (0.0..3.0).contains(&d) { 1.0 - d / 3.0 } else { 0.0 };
                let g = (b * bright * 255.0) as u8;
                f[i] = [0, g, g / 6];
            }
        }
        Effect::Custom => {
            // Looping step sequencer: dark board, each step paints its keys in
            // its color for its duration. `speed` scales the whole loop.
            let total: u64 = custom.iter().map(|s| s.ms.max(30)).sum::<u64>().max(1);
            let tm = ((t * speed * 1000.0) as u64) % total;
            let mut acc = 0u64;
            for s in custom {
                acc += s.ms.max(30);
                if tm < acc {
                    for &k in &s.keys {
                        if (k as usize) < n {
                            f[k as usize] = scale(s.color, bright);
                        }
                    }
                    break;
                }
            }
        }
    }

    // --- fx events on top ---------------------------------------------------
    for ev in events {
        if ev.key >= n {
            continue;
        }
        let e = ev.at.elapsed().as_secs_f32();
        // A custom sequence event plays its steps once, as authored.
        if let Some(steps) = &ev.seq {
            let tm = (e * 1000.0) as u64;
            let mut acc = 0u64;
            for st in steps.iter() {
                acc += st.ms.max(30);
                if tm < acc {
                    for &k in st.keys.iter() {
                        if (k as usize) < n {
                            f[k as usize] = scale(st.color, bright);
                        }
                    }
                    break;
                }
            }
            continue;
        }
        let dur = ev.effect.duration();
        if e >= dur {
            continue;
        }
        let fade_k = 1.0 - e / dur;
        let (px, py) = (geo.keys[ev.key].x, geo.keys[ev.key].y);
        for (j, k) in geo.keys.iter().enumerate() {
            let d = ((k.x - px).powi(2) + (k.y - py).powi(2)).sqrt();
            let intensity = match ev.effect {
                PressEffect::None => 0.0,
                PressEffect::Flash => {
                    if j == ev.key {
                        fade_k
                    } else {
                        0.0
                    }
                }
                PressEffect::Splash => fade_k * (1.0 - d / 2.2).max(0.0),
                PressEffect::Ripple => {
                    let radius = (e / dur) * 7.0;
                    (-((d - radius) / 0.8).powi(2)).exp() * fade_k
                }
                PressEffect::Column => {
                    if (k.x - px).abs() < 0.5 {
                        fade_k
                    } else {
                        0.0
                    }
                }
                PressEffect::Row => {
                    if (k.y - py).abs() < 0.6 {
                        fade_k
                    } else {
                        0.0
                    }
                }
                PressEffect::Cross => {
                    // The cross "paints itself": arms grow outward from the key
                    // along its row and column, then the whole shape fades.
                    let reach = (e / (dur * 0.6)).min(1.0) * 16.0;
                    let on_row = (k.y - py).abs() < 0.6 && (k.x - px).abs() <= reach;
                    let on_col = (k.x - px).abs() < 0.5 && (k.y - py).abs() <= reach;
                    if on_row || on_col {
                        fade_k
                    } else {
                        0.0
                    }
                }
                PressEffect::Comet => {
                    // Two comet heads fly outward along the row, with trails.
                    if (k.y - py).abs() < 0.6 {
                        let head = (e / dur) * 14.0;
                        let dx = (k.x - px).abs();
                        let behind = head - dx;
                        if (0.0..3.0).contains(&behind) {
                            (1.0 - behind / 3.0) * fade_k
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    }
                }
                PressEffect::Sparkle => {
                    // Random twinkles around the pressed key.
                    if d < 3.5 {
                        let r = prand(ev.seed, j as u64);
                        let peak = r * dur * 0.75;
                        let tw = (-((e - peak) / 0.12).powi(2)).exp();
                        tw * (1.0 - d / 4.0) * fade_k
                    } else {
                        0.0
                    }
                }
                PressEffect::BoardFlash => fade_k,
                PressEffect::BoardRainbow => {
                    // ignore ev.color below by writing directly
                    let hue = (k.x / 12.0 + e * 2.0).rem_euclid(1.0);
                    let c = hsv(hue, 1.0, fade_k);
                    f[j] = lerp(f[j], c, fade_k);
                    0.0
                }
                PressEffect::BoardWave => {
                    // A vertical light bar sweeps left → right across the board.
                    let head = (e / dur) * 18.0 - 1.5;
                    (-((k.x - head) / 1.3).powi(2)).exp() * fade_k.sqrt()
                }
                PressEffect::BoardPulse => {
                    // Two quick whole-board pulses (lub-dub).
                    let p = e / dur;
                    let pulse = |c: f32, w: f32| (-((p - c) / w).powi(2)).exp();
                    (pulse(0.15, 0.08) + 0.75 * pulse(0.5, 0.08)).min(1.0)
                }
                PressEffect::BoardSparkle => {
                    // Twinkles across the whole keyboard.
                    let r = prand(ev.seed, j as u64);
                    let peak = r * dur * 0.8;
                    let tw = (-((e - peak) / 0.1).powi(2)).exp();
                    tw * fade_k
                }
                PressEffect::BoardMatrix => {
                    // A burst of matrix rain: per-column drops with trails, in
                    // the event color (pick green for the classic look).
                    let col = k.x.round();
                    let r = prand(ev.seed, col as u64);
                    let head = (e * (8.0 + r * 8.0) + r * 6.0) % 9.0 - 1.5;
                    let behind = head - k.y;
                    if (0.0..2.5).contains(&behind) {
                        (1.0 - behind / 2.5) * fade_k
                    } else {
                        0.0
                    }
                }
            };
            if intensity > 0.02 {
                f[j] = lerp(f[j], ev.color, intensity.min(1.0));
            }
        }
    }
    f
}

/// The most frequent color in a frame (used to prime a takeover in 1 packet).
pub(crate) fn dominant(frame: &[[u8; 3]]) -> [u8; 3] {
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    let mut best = [0u8, 0, 0];
    let mut best_n = 0;
    for &c in frame {
        let n = counts.entry(c).or_insert(0);
        *n += 1;
        if *n > best_n {
            best_n = *n;
            best = c;
        }
    }
    best
}

fn scale(c: [u8; 3], v: f32) -> [u8; 3] {
    let v = v.clamp(0.0, 1.0);
    [(c[0] as f32 * v) as u8, (c[1] as f32 * v) as u8, (c[2] as f32 * v) as u8]
}

fn lerp(a: [u8; 3], b: [u8; 3], k: f32) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * k) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * k) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * k) as u8,
    ]
}

/// Double-pulse "lub-dub" waveform in 0..1, roughly one beat per second.
fn heartbeat(t: f32) -> f32 {
    let p = t.rem_euclid(1.0);
    let pulse = |center: f32, w: f32| (-((p - center) / w).powi(2)).exp();
    (pulse(0.0, 0.06) + 0.7 * pulse(0.2, 0.05)).min(1.0) * 0.84 + 0.16
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor() as i32;
    let fr = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * fr);
    let tt = v * (1.0 - s * (1.0 - fr));
    let (r, g, b) = match i % 6 {
        0 => (v, tt, p),
        1 => (q, v, p),
        2 => (p, v, tt),
        3 => (p, q, v),
        4 => (tt, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(key: usize, effect: PressEffect) -> FxEvent {
        FxEvent { key, effect, color: [255, 255, 255], at: Instant::now(), seed: 7, seq: None }
    }

    #[test]
    fn cross_paints_row_and_column() {
        let geo = geometry::voyager();
        // Late in the paint phase the arms reach the whole row/column.
        let mut e = ev(8, PressEffect::Cross);
        e.at = Instant::now() - Duration::from_millis(350);
        let f = compute(Effect::Off, [0, 0, 0], 1.0, 1.0, 0.0, &[], &[e], &[], geo);
        let (px, py) = (geo.keys[8].x, geo.keys[8].y);
        let lit_row = geo
            .keys
            .iter()
            .enumerate()
            .filter(|(j, k)| (k.y - py).abs() < 0.6 && f[*j] != [0, 0, 0])
            .count();
        let lit_col = geo
            .keys
            .iter()
            .enumerate()
            .filter(|(j, k)| (k.x - px).abs() < 0.5 && f[*j] != [0, 0, 0])
            .count();
        assert!(lit_row >= 3, "row arm should be painted ({lit_row})");
        assert!(lit_col >= 2, "column arm should be painted ({lit_col})");
    }

    #[test]
    fn board_sparkle_lights_scattered_keys() {
        let geo = geometry::voyager();
        let mut any = 0;
        for ms in [100u64, 300, 500, 700] {
            let mut e = ev(0, PressEffect::BoardSparkle);
            e.at = Instant::now() - Duration::from_millis(ms);
            let f = compute(Effect::Off, [0, 0, 0], 1.0, 1.0, 0.0, &[], &[e], &[], geo);
            any += f.iter().filter(|c| **c != [0, 0, 0]).count();
        }
        assert!(any > 0, "sparkles should appear at some point");
    }

    #[test]
    fn frame_is_full_length_and_colored() {
        let geo = geometry::voyager();
        let f = compute(Effect::Rainbow, [80, 170, 255], 1.0, 1.0, 0.3, &[], &[], &[], geo);
        assert_eq!(f.len(), geo.len());
        assert!(f.iter().any(|c| *c != [0, 0, 0]), "rainbow should light keys");
    }

    #[test]
    fn layout_base_reproduces_key_colors() {
        let geo = geometry::voyager();
        let mut base = vec![[0u8, 0, 0]; geo.len()];
        base[3] = [10, 200, 30];
        let f = compute(Effect::Layout, [0, 0, 0], 1.0, 1.0, 0.0, &base, &[], &[], geo);
        assert_eq!(f[3], [10, 200, 30]);
        assert_eq!(f[4], [0, 0, 0]);
    }

    #[test]
    fn flash_lights_only_the_pressed_key() {
        let geo = geometry::voyager();
        let f = compute(Effect::Off, [0, 0, 0], 1.0, 1.0, 0.0, &[], &[ev(5, PressEffect::Flash)], &[], geo);
        let sum = |c: [u8; 3]| c[0] as u32 + c[1] as u32 + c[2] as u32;
        assert!(sum(f[5]) > sum(f[6]));
    }

    #[test]
    fn board_flash_lights_everything() {
        let geo = geometry::voyager();
        let f = compute(Effect::Off, [0, 0, 0], 1.0, 1.0, 0.0, &[], &[ev(5, PressEffect::BoardFlash)], &[], geo);
        assert!(f.iter().all(|c| *c != [0, 0, 0]), "whole board should light");
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), [0, 255, 0]);
    }

    #[test]
    fn custom_sequence_steps_and_loops() {
        let geo = geometry::voyager();
        let prog = vec![
            FxStep { keys: vec![0, 1], color: [255, 0, 0], ms: 100 },
            FxStep { keys: vec![2], color: [0, 0, 255], ms: 100 },
        ];
        let at = |t: f32| compute(Effect::Custom, [0, 0, 0], 1.0, 1.0, t, &[], &[], &prog, geo);
        // t=0.05s → step 1: keys 0,1 red, key 2 dark.
        let f = at(0.05);
        assert_eq!(f[0], [255, 0, 0]);
        assert_eq!(f[1], [255, 0, 0]);
        assert_eq!(f[2], [0, 0, 0]);
        // t=0.15s → step 2: key 2 blue, key 0 dark.
        let f = at(0.15);
        assert_eq!(f[2], [0, 0, 255]);
        assert_eq!(f[0], [0, 0, 0]);
        // t=0.25s → looped back to step 1.
        let f = at(0.25);
        assert_eq!(f[0], [255, 0, 0]);
        // speed 2.0 halves the loop: t=0.52s → 1040ms → %200 = 40ms → step 1.
        let f = compute(Effect::Custom, [0, 0, 0], 2.0, 1.0, 0.52, &[], &[], &prog, geo);
        assert_eq!(f[0], [255, 0, 0]);
    }

    #[test]
    fn heartbeat_bounded() {
        for i in 0..100 {
            let v = heartbeat(i as f32 * 0.03);
            assert!((0.0..=1.0).contains(&v));
        }
    }
}
