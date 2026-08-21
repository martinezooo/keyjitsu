//! Terminal rendering of the keyboard: a ratatui widget shared by
//! `layout` (one-shot print), `live` (full TUI) and `heatmap show`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::geometry::Geometry;
use crate::legend::{labels_for, KeyLabels};
use crate::oryx_api::Layer;

/// Terminal cells per 1u of key width / height.
const CELL_W: u16 = 7;
const CELL_H: u16 = 3;

pub struct KeyboardWidget<'a> {
    geometry: &'a Geometry,
    labels: Vec<KeyLabels>,
    glow: Vec<Option<Color>>,
    pub pressed: Vec<bool>,
    /// Per-key heat in 0.0..=1.0 (already normalized); `None` disables overlay.
    pub heat: Option<Vec<f64>>,
}

impl<'a> KeyboardWidget<'a> {
    pub fn new(geometry: &'a Geometry, layer: Option<&Layer>) -> Self {
        let n = geometry.len();
        let (labels, glow) = match layer {
            Some(layer) => (
                layer.keys.iter().map(labels_for).collect(),
                layer.keys.iter().map(|k| k.glow_color.as_deref().and_then(parse_hex)).collect(),
            ),
            None => (
                (0..n).map(|_| KeyLabels { tap: String::new(), hold: None }).collect(),
                vec![None; n],
            ),
        };
        KeyboardWidget { geometry, labels, glow, pressed: vec![false; n], heat: None }
    }

    /// Size (cols, rows) the widget needs.
    pub fn size(geometry: &Geometry) -> (u16, u16) {
        let mut w = 0u16;
        let mut h = 0u16;
        for k in &geometry.keys {
            w = w.max(key_col(k.x) + CELL_W);
            h = h.max(key_row(k.y) + CELL_H);
        }
        (w, h)
    }
}

fn key_col(x: f32) -> u16 {
    (x * CELL_W as f32).round() as u16
}

/// Floor keeps intra-column spacing exact (y steps are 1.0 within a column).
fn key_row(y: f32) -> u16 {
    (y * CELL_H as f32).floor() as u16
}

fn parse_hex(s: &str) -> Option<Color> {
    crate::geometry::parse_hex_rgb(s).map(|(r, g, b)| Color::Rgb(r, g, b))
}

fn heat_color(t: f64) -> Color {
    // Dark slate → amber → red ramp; sqrt lifts low counts into visibility.
    let t = t.sqrt().clamp(0.0, 1.0);
    let lerp = |a: f64, b: f64| (a + (b - a) * t) as u8;
    Color::Rgb(lerp(45.0, 235.0), lerp(48.0, 90.0), lerp(58.0, 40.0))
}

/// Center `s` (≤5 cells) inside a 5-cell field.
fn center5(s: &str) -> String {
    let len = s.chars().count().min(5);
    let pad = 5 - len;
    let left = pad / 2;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(pad - left))
}

impl Widget for &KeyboardWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for (i, k) in self.geometry.keys.iter().enumerate() {
            let x = area.x + key_col(k.x);
            let y = area.y + key_row(k.y);
            if x + CELL_W > area.right() || y + CELL_H > area.bottom() {
                continue; // terminal too small; clip whole keys
            }

            let pressed = self.pressed.get(i).copied().unwrap_or(false);
            let heat = self.heat.as_ref().and_then(|h| h.get(i).copied()).unwrap_or(0.0);

            let mut style = Style::default();
            if let Some(h) = &self.heat {
                if h.get(i).copied().unwrap_or(0.0) > 0.0 {
                    style = style.bg(heat_color(heat));
                }
            } else if let Some(glow) = self.glow.get(i).copied().flatten() {
                style = style.fg(glow);
            }
            if pressed {
                style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
            }

            let labels = &self.labels[i];
            let top = "╭─────╮".to_string();
            let mid = format!("│{}│", center5(&labels.tap));
            let bot = match &labels.hold {
                Some(h) if !h.is_empty() => {
                    let h: String = h.chars().take(3).collect();
                    format!("╰{}╯", center5(&h).replace(' ', "─"))
                }
                _ => "╰─────╯".to_string(),
            };
            buf.set_string(x, y, &top, style);
            buf.set_string(x, y + 1, &mid, style);
            buf.set_string(x, y + 2, &bot, style.add_modifier(Modifier::DIM));
        }
    }
}

/// Render a widget once to stdout as plain ANSI (no raw mode, no altscreen).
pub fn print_widget(widget: &KeyboardWidget, width: u16, height: u16) {
    use std::fmt::Write as _;

    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);

    let mut out = String::new();
    for y in 0..height {
        let mut line = String::new();
        let mut prev_style: Option<Style> = None;
        for x in 0..width {
            let cell = &buf[(x, y)];
            let style = cell.style();
            if prev_style != Some(style) {
                let _ = write!(line, "\x1b[0m{}", ansi_for(style));
                prev_style = Some(style);
            }
            line.push_str(cell.symbol());
        }
        // Trim trailing spaces per line to keep copy/paste clean.
        out.push_str(line.trim_end());
        out.push_str("\x1b[0m\n");
    }
    print!("{out}");
}

fn ansi_for(style: Style) -> String {
    let mut s = String::new();
    if let Some(Color::Rgb(r, g, b)) = style.fg {
        s.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    }
    if let Some(Color::Rgb(r, g, b)) = style.bg {
        s.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        s.push_str("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        s.push_str("\x1b[2m");
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        s.push_str("\x1b[7m");
    }
    s
}
