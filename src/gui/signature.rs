//! Tiny KEYJITSU RGB easter egg.
//!
//! Kept separate from the general FX renderer so the feature cannot change
//! normal effect semantics. The RGB thread only asks this module for frames
//! while a signature run is active.

use std::time::{Duration, Instant};

use crate::geometry;

pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(9);
const INITIAL_S: f32 = 0.12;
const TRANSITION_S: f32 = 0.234;
const BEAT_S: f32 = 0.075;
const FINAL_HOLD_S: f32 = 0.36;
const FADE_S: f32 = 0.21;

#[derive(Clone, Copy)]
pub(super) struct KeyjitsuSignature {
    started: Instant,
    keys: [usize; 8],
}

impl KeyjitsuSignature {
    pub(super) fn new(keys: [usize; 8]) -> Self {
        Self {
            started: Instant::now(),
            keys,
        }
    }

    pub(super) fn finished(self, now: Instant) -> bool {
        self.elapsed(now) >= total_s()
    }

    pub(super) fn frame(self, now: Instant, geo: &geometry::Geometry) -> Vec<[u8; 3]> {
        let mut frame = vec![[0, 0, 0]; geo.len()];
        let mut elapsed = self.elapsed(now);
        if elapsed < INITIAL_S {
            if let Some(rgb) = frame.get_mut(self.keys[0]) {
                *rgb = [255, 0, 0];
            }
            return frame;
        }
        elapsed -= INITIAL_S;

        for pair in 1..self.keys.len() {
            if elapsed < TRANSITION_S {
                paint_transition(
                    &mut frame,
                    geo,
                    self.keys[pair - 1],
                    self.keys[pair],
                    elapsed / TRANSITION_S,
                );
                return frame;
            }
            elapsed -= TRANSITION_S;
            if elapsed < BEAT_S {
                if let Some(rgb) = frame.get_mut(self.keys[pair]) {
                    *rgb = [255, 0, 0];
                }
                return frame;
            }
            elapsed -= BEAT_S;
        }

        if elapsed < FINAL_HOLD_S {
            paint_final(&mut frame, geo, &self.keys, 1.0);
            return frame;
        }
        elapsed -= FINAL_HOLD_S;
        let intensity = 1.0 - (elapsed / FADE_S).clamp(0.0, 1.0);
        paint_final(&mut frame, geo, &self.keys, intensity);
        frame
    }

    fn elapsed(self, now: Instant) -> f32 {
        now.saturating_duration_since(self.started).as_secs_f32()
    }
}

fn total_s() -> f32 {
    INITIAL_S + 7.0 * (TRANSITION_S + BEAT_S) + FINAL_HOLD_S + FADE_S
}

fn smooth01(v: f32) -> f32 {
    let t = v.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn key_distance(geo: &geometry::Geometry, key: usize, x: f32, y: f32) -> f32 {
    let Some(key) = geo.keys.get(key) else {
        return f32::INFINITY;
    };
    let dx = key.x - x;
    let dy = key.y - y;
    (dx * dx + dy * dy).sqrt()
}

fn paint_transition(
    frame: &mut [[u8; 3]],
    geo: &geometry::Geometry,
    src: usize,
    dst: usize,
    t: f32,
) {
    let (Some(src_geo), Some(dst_geo)) = (geo.keys.get(src), geo.keys.get(dst)) else {
        return;
    };
    let s = smooth01(t);
    let hx = src_geo.x + (dst_geo.x - src_geo.x) * s;
    let hy = src_geo.y + (dst_geo.y - src_geo.y) * s;
    let trail_s = (s - 0.10).clamp(0.0, 1.0);
    let tx = src_geo.x + (dst_geo.x - src_geo.x) * trail_s;
    let ty = src_geo.y + (dst_geo.y - src_geo.y) * trail_s;

    for (key, rgb) in frame.iter_mut().enumerate() {
        let d1 = key_distance(geo, key, hx, hy);
        let d2 = key_distance(geo, key, tx, ty);
        let head = (-(d1 * d1) / 0.55).exp();
        let trail = 0.46 * (-(d2 * d2) / 1.30).exp();
        let gold = head.max(trail);
        if gold >= 0.045 {
            *rgb = [
                (255.0 * gold).min(255.0) as u8,
                (172.0 * gold).min(255.0) as u8,
                (18.0 * gold).min(255.0) as u8,
            ];
        }
    }

    let next_red = smooth01((t - 0.72) / 0.28);
    let prev_red = 1.0 - smooth01((t - 0.90) / 0.10);
    if let Some(rgb) = frame.get_mut(src) {
        *rgb = [(255.0 * prev_red) as u8, 0, 0];
    }
    if next_red > 0.0 {
        if let Some(rgb) = frame.get_mut(dst) {
            *rgb = [(255.0 * next_red) as u8, 0, 0];
        }
    }
}

fn paint_final(frame: &mut [[u8; 3]], geo: &geometry::Geometry, keys: &[usize; 8], intensity: f32) {
    for (key, rgb) in frame.iter_mut().enumerate() {
        if keys.contains(&key) {
            *rgb = [(255.0 * intensity) as u8, 0, 0];
            continue;
        }
        let mut gold: f32 = 0.0;
        for &letter in keys {
            let Some(letter_geo) = geo.keys.get(letter) else {
                continue;
            };
            let d = key_distance(geo, key, letter_geo.x, letter_geo.y);
            gold = gold.max(0.46 * (-(d * d) / 0.72).exp());
        }
        if gold >= 0.045 {
            *rgb = [
                (255.0 * gold * intensity).min(255.0) as u8,
                (172.0 * gold * intensity).min(255.0) as u8,
                (18.0 * gold * intensity).min(255.0) as u8,
            ];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [usize; 8] = [40, 9, 32, 39, 34, 11, 14, 33];

    fn at(seconds: f32) -> KeyjitsuSignature {
        KeyjitsuSignature {
            started: Instant::now() - Duration::from_secs_f32(seconds),
            keys: KEYS,
        }
    }

    #[test]
    fn starts_on_k_and_finishes_with_all_letters() {
        let geo = geometry::voyager();
        let now = Instant::now();
        let first = KeyjitsuSignature {
            started: now,
            keys: KEYS,
        }
        .frame(now, geo);
        assert_eq!(first[KEYS[0]], [255, 0, 0]);
        assert_eq!(first[KEYS[1]], [0, 0, 0]);

        let finale_at = INITIAL_S + 7.0 * (TRANSITION_S + BEAT_S) + 0.05;
        let finale = at(finale_at).frame(Instant::now(), geo);
        for key in KEYS {
            assert_eq!(finale[key], [255, 0, 0]);
        }
    }

    #[test]
    fn transition_has_gold_between_letters() {
        let geo = geometry::voyager();
        let frame = at(INITIAL_S + TRANSITION_S * 0.5).frame(Instant::now(), geo);
        assert!(frame
            .iter()
            .any(|rgb| rgb[0] > rgb[1] && rgb[1] > rgb[2] && rgb[1] > 0));
    }
}
