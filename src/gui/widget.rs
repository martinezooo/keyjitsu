//! Painting the keyboard inside egui. Keys look like the physical board:
//! light keycaps carrying the exact per-key color as a crisp edge plus a soft
//! underglow, dark legends on top. The caller supplies one color per key, so
//! the same widget serves Live (layout/edited glow), Heatmap and previews.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::geometry::Geometry;
use crate::legend::{key_kind, labels_for, KeyKind};
use crate::oryx_api::Layer;

pub const SELECTED: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE); // UI "selected" cyan
const CAP: Color32 = Color32::from_rgb(242, 238, 220); // warm ivory
const CAP_TOP: Color32 = Color32::from_rgb(250, 247, 234);
const LEGEND: Color32 = Color32::from_rgb(30, 31, 34);
// Dark enough to actually read on the ivory caps (the old grey made hold
// symbols like ⇧ nearly invisible).
const LEGEND_WEAK: Color32 = Color32::from_rgb(82, 80, 86);
const UNLIT_EDGE: Color32 = Color32::from_rgb(75, 63, 114); // muted violet

pub fn parse_hex(s: &str) -> Option<Color32> {
    crate::geometry::parse_hex_rgb(s).map(|(r, g, b)| Color32::from_rgb(r, g, b))
}

/// Heat ramp tuned to the app palette: neutral → violet → amber → red.
/// (A raw red outline reads as an error; this reads as intensity.)
pub fn heat_color(t: f64) -> Color32 {
    let t = t.sqrt().clamp(0.0, 1.0) as f32;
    const STOPS: [(f32, Color32); 4] = [
        (0.00, Color32::from_rgb(0x4A, 0x4E, 0x62)), // neutral slate
        (0.45, Color32::from_rgb(0x8B, 0x5C, 0xF6)), // violet
        (0.80, Color32::from_rgb(0xF5, 0x9E, 0x0B)), // amber
        (1.00, Color32::from_rgb(0xEF, 0x44, 0x44)), // red (hottest only)
    ];
    for w in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let k = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            return mix(c0, c1, k);
        }
    }
    STOPS[3].1
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Rotate a vector by `a` radians.
fn rot(v: Vec2, a: f32) -> Vec2 {
    let (s, c) = a.sin_cos();
    Vec2::new(c * v.x - s * v.y, s * v.x + c * v.y)
}

/// Rounded-rectangle outline (arc-sampled corners), rotated by `angle`
/// around `center`, optionally grown by `grow` px (for halos/rings).
fn key_poly(center: Pos2, w: f32, h: f32, r: f32, angle: f32, grow: f32) -> Vec<Pos2> {
    let w2 = w / 2.0 + grow;
    let h2 = h / 2.0 + grow;
    let r = (r + grow * 0.5).min(w2).min(h2);
    let quarters: [(Vec2, f32); 4] = [
        (Vec2::new(w2 - r, -(h2 - r)), -90.0),
        (Vec2::new(w2 - r, h2 - r), 0.0),
        (Vec2::new(-(w2 - r), h2 - r), 90.0),
        (Vec2::new(-(w2 - r), -(h2 - r)), 180.0),
    ];
    let mut pts = Vec::with_capacity(20);
    for (c, start) in quarters {
        for step in 0..=4 {
            let t = (start + 90.0 * step as f32 / 4.0).to_radians();
            let p = c + Vec2::new(t.cos(), t.sin()) * r;
            pts.push(center + rot(p, angle));
        }
    }
    pts
}

/// Text centered at `center`, rotated by `angle` (for the thumb keys).
fn text_rot(painter: &Painter, center: Pos2, angle: f32, text: &str, font: FontId, color: Color32) {
    if text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_string(), font, color);
    let off = Vec2::new(galley.size().x / 2.0, galley.size().y / 2.0);
    let pos = center - rot(off, angle);
    let mut ts = eframe::epaint::TextShape::new(pos, galley, color);
    ts.angle = angle;
    painter.add(eframe::egui::Shape::Text(ts));
}

pub struct KbResponse {
    /// Key left-clicked this frame (selects it for the config panel).
    pub clicked: Option<usize>,
    /// Key currently under the cursor (for tooltips).
    pub hovered: Option<usize>,
}

/// Reduce a color's alpha by `alpha` (1.0 = unchanged), keeping its hue.
fn fade(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha).round() as u8)
}

/// `glow[i]` = the color of key `i` (None → unlit). `selected` draws an accent
/// ring. `alpha` (0..1) fades the whole drawing so it can be made see-through.
/// The board is fitted to the available width and centered.
#[allow(clippy::too_many_arguments)]
pub fn draw_keyboard(
    ui: &mut Ui,
    geo: &Geometry,
    legends: Option<&Layer>,
    glow: &[Option<Color32>],
    pressed: &[bool],
    selected: Option<usize>,
    alpha: f32,
    mono: bool,
) -> KbResponse {
    let cols = geo.keys.iter().map(|k| k.x).fold(0.0f32, f32::max) + 1.0;
    let rows = geo.keys.iter().map(|k| k.y).fold(0.0f32, f32::max) + 1.0;

    let margin = 10.0;
    let avail = (ui.available_width() - margin * 2.0).max(120.0);
    // Size by width. Interactive callers enforce their own 34px legibility
    // floor via container width; the low bound here only guards degenerate
    // layouts so the peek minimap can go genuinely small.
    let unit = (avail / cols).clamp(14.0, 62.0);
    let gap = unit * 0.16;
    let board_w = cols * unit;
    // Extra bottom room: the tall, rotated thumb keys swing past the last row.
    let board_h = (rows + 0.55) * unit;

    let (area, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), board_h + gap), Sense::click());
    let painter = ui.painter_at(area);
    // Center horizontally so nothing clips at the window edges.
    let origin = Pos2::new((area.center().x - board_w / 2.0).max(area.left() + margin), area.top() + gap * 0.5);

    let pointer = response.hover_pos();
    let mut hovered = None;
    let key_round = (unit * 0.16) as u8;

    for (i, k) in geo.keys.iter().enumerate() {
        // Thumb-cluster keys render like the physical board (and Oryx): both
        // tilted toward the center, the inner one 1.5u tall.
        let is_thumb = k.y > 3.9;
        let tall = is_thumb && k.y > 4.6;
        let angle = if !is_thumb {
            0.0
        } else if k.x < cols / 2.0 {
            0.30
        } else {
            -0.30
        };
        let kw = unit - gap;
        let kh = if tall { unit * 1.5 - gap } else { unit - gap };
        let center = origin + Vec2::new((k.x + 0.5) * unit, k.y * unit + gap * 0.5 + kh / 2.0);
        let cap = Rect::from_center_size(center, Vec2::new(kw, kh));
        let radius = CornerRadius::same(key_round);
        if let Some(p) = pointer {
            let l = rot(p - center, -angle);
            if l.x.abs() < kw / 2.0 && l.y.abs() < kh / 2.0 {
                hovered = Some(i);
            }
        }
        let is_pressed = pressed.get(i).copied().unwrap_or(false);
        let color = glow.get(i).and_then(|c| *c);

        // Rotated path for the thumb keys (polygon + rotated text).
        if angle != 0.0 {
            let r = unit * 0.16;
            if mono {
                let ink = Color32::WHITE;
                let fill_a = if is_pressed { 0.55 } else { 0.14 };
                painter.add(eframe::egui::Shape::convex_polygon(
                    key_poly(center, kw, kh, r, angle, 0.0),
                    fade(Color32::WHITE, alpha * fill_a),
                    Stroke::new(if is_pressed { 2.6 } else { 1.6 }, fade(ink, alpha)),
                ));
            } else {
                if let Some(c) = color {
                    let boost = if is_pressed { 1.6 } else { 1.0 };
                    for (grow_mul, base_a) in [(0.7f32, 7.0f32), (0.38, 15.0), (0.14, 28.0)] {
                        let a = (base_a * boost).min(52.0);
                        painter.add(eframe::egui::Shape::convex_polygon(
                            key_poly(center, kw, kh, r, angle, gap * grow_mul),
                            fade(Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a as u8), alpha),
                            Stroke::NONE,
                        ));
                    }
                }
                let tint = match color {
                    Some(c) if is_pressed => mix(CAP, c, 0.34),
                    Some(c) => mix(CAP, c, 0.14),
                    None => CAP,
                };
                let edge = color.unwrap_or(UNLIT_EDGE);
                painter.add(eframe::egui::Shape::convex_polygon(
                    key_poly(center, kw, kh, r, angle, 0.0),
                    fade(tint, alpha),
                    Stroke::new(1.4, fade(edge, alpha)),
                ));
                if selected == Some(i) {
                    painter.add(eframe::egui::Shape::convex_polygon(
                        key_poly(center, kw, kh, r, angle, 3.0),
                        Color32::TRANSPARENT,
                        Stroke::new(2.4, fade(SELECTED, alpha)),
                    ));
                }
            }
            // Legends + icon, rotated with the cap.
            let (tap_col, hold_col, icon_col) = if mono {
                (fade(Color32::WHITE, alpha), fade(Color32::WHITE, alpha * 0.7), fade(Color32::WHITE, alpha))
            } else {
                (fade(LEGEND, alpha), fade(LEGEND_WEAK, alpha), fade(LEGEND, alpha * 0.8))
            };
            if let Some(key) = legends.and_then(|l| l.keys.get(i)) {
                let labels = labels_for(key);
                if !labels.tap.is_empty() {
                    let c = center + rot(Vec2::new(0.0, -unit * 0.04), angle);
                    text_rot(&painter, c, angle, &labels.tap, FontId::proportional(unit * 0.31), tap_col);
                }
                if let Some(hold) = &labels.hold {
                    let c = center + rot(Vec2::new(0.0, kh / 2.0 - unit * 0.16), angle);
                    text_rot(&painter, c, angle, hold, FontId::proportional(unit * 0.23), hold_col);
                }
                if let Some(icon) = icon_text(key) {
                    let c = center + rot(Vec2::new(-kw / 2.0 + unit * 0.21, -kh / 2.0 + unit * 0.18), angle);
                    text_rot(&painter, c, angle, &icon, FontId::proportional(unit * 0.26), icon_col);
                }
            }
            continue;
        }

        if mono {
            // High-contrast black & white: white outline + white legends, only
            // a faint fill so the desktop shows through. No colored glow.
            let ink = Color32::WHITE;
            let fill_a = if is_pressed { 0.55 } else { 0.14 };
            painter.rect_filled(cap, radius, fade(Color32::WHITE, alpha * fill_a));
            painter.rect_stroke(
                cap,
                radius,
                Stroke::new(if is_pressed { 2.6 } else { 1.6 }, fade(ink, alpha)),
                StrokeKind::Inside,
            );
            if selected == Some(i) {
                painter.rect_stroke(cap.expand(3.0), CornerRadius::same(key_round + 2), Stroke::new(2.4, fade(SELECTED, alpha)), StrokeKind::Outside);
            }
            draw_legends(&painter, cap, unit, legends, i, fade(ink, alpha), fade(ink, alpha * 0.7));
            draw_icon(&painter, cap, unit, legends, i, fade(ink, alpha));
            continue;
        }

        // Soft underglow: a tight, contained halo that stays within the gap
        // around the key instead of blooming across its neighbours.
        // Underglow: subtle (calm, not arcade). The colored edge below carries
        // the identity; the halo is just a soft hint that the key is lit.
        if let Some(c) = color {
            let boost = if is_pressed { 1.6 } else { 1.0 };
            for (grow_mul, base_a) in [(0.7f32, 7.0f32), (0.38, 15.0), (0.14, 28.0)] {
                let grow = gap * grow_mul;
                let a = (base_a * boost).min(52.0);
                painter.rect_filled(
                    cap.expand(grow),
                    CornerRadius::same(key_round + (grow * 0.5) as u8),
                    fade(Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a as u8), alpha),
                );
            }
        }

        // Keycap: warm ivory, faintly tinted by the key color, top-lit.
        let tint = match color {
            Some(c) if is_pressed => mix(CAP, c, 0.34),
            Some(c) => mix(CAP, c, 0.14),
            None => CAP,
        };
        let top = if is_pressed { mix(tint, Color32::WHITE, 0.22) } else { mix(tint, CAP_TOP, 0.55) };
        painter.rect_filled(cap, radius, fade(tint, alpha));
        painter.rect_filled(Rect::from_min_max(cap.min, cap.center_bottom()), radius, fade(top, alpha));

        // Thin edge in the exact key color - the faithful, calm color reference.
        let edge = color.unwrap_or(UNLIT_EDGE);
        painter.rect_stroke(cap, radius, Stroke::new(1.4, fade(edge, alpha)), StrokeKind::Inside);

        // Selected is a UI state, distinct from the layout colors → cyan.
        if selected == Some(i) {
            painter.rect_stroke(cap.expand(1.5), radius, Stroke::new(1.5, fade(SELECTED.gamma_multiply(0.4), alpha)), StrokeKind::Outside);
            painter.rect_stroke(cap.expand(3.0), CornerRadius::same(key_round + 2), Stroke::new(2.4, fade(SELECTED, alpha)), StrokeKind::Outside);
        }

        draw_legends(&painter, cap, unit, legends, i, fade(LEGEND, alpha), fade(LEGEND_WEAK, alpha));
        draw_icon(&painter, cap, unit, legends, i, fade(LEGEND, alpha * 0.8));
    }

    let clicked = if response.clicked() { hovered } else { None };
    KbResponse { clicked, hovered }
}

/// Draw a key's tap + hold legend text centered on its cap.
fn draw_legends(
    painter: &Painter,
    cap: Rect,
    unit: f32,
    legends: Option<&Layer>,
    i: usize,
    tap_color: Color32,
    hold_color: Color32,
) {
    let Some(key) = legends.and_then(|l| l.keys.get(i)) else { return };
    let labels = labels_for(key);
    if !labels.tap.is_empty() {
        painter.text(
            cap.center() - Vec2::new(0.0, unit * 0.04),
            Align2::CENTER_CENTER,
            &labels.tap,
            FontId::proportional(unit * 0.31),
            tap_color,
        );
    }
    if let Some(hold) = &labels.hold {
        painter.text(
            cap.center_bottom() - Vec2::new(0.0, unit * 0.16),
            Align2::CENTER_CENTER,
            hold,
            FontId::proportional(unit * 0.23),
            hold_color,
        );
    }
}

/// Category badge in the key's top-left corner. Layer keys get a readable
/// "L<n>" (which layer they reach); media/mouse/lighting get their glyph.
fn icon_text(key: &crate::oryx_api::OryxKey) -> Option<String> {
    match key_kind(key) {
        KeyKind::Layer(Some(n)) => Some(format!("L{n}")),
        k => k.icon().map(str::to_string),
    }
}

fn draw_icon(painter: &Painter, cap: Rect, unit: f32, legends: Option<&Layer>, i: usize, tint: Color32) {
    let Some(key) = legends.and_then(|l| l.keys.get(i)) else { return };
    if let Some(icon) = icon_text(key) {
        painter.text(
            cap.left_top() + Vec2::new(unit * 0.21, unit * 0.18),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(unit * 0.26),
            tint,
        );
    }
}
