//! `keyjitsu live` - full-screen TUI: your Voyager on screen, live.
//!
//! Key presses and layer changes stream in over HID; presses are also
//! recorded into the heatmap store (same data `keyjitsu heatmap show` reads).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout as RtLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::device::Keyboard;
use crate::geometry;
use crate::heatmap::{normalize, HeatmapStore};
use crate::oryx_api::{fetch_layout, Layout, LayoutId};
use crate::protocol::Event;
use crate::ui::KeyboardWidget;

struct App {
    layout: Option<Layout>,
    active_layer: u8,
    view_layer: u8,
    follow: bool,
    show_heat: bool,
    pressed: Vec<bool>,
    heat: HeatmapStore,
    key_count: usize,
    status: String,
}

pub fn run(serial: Option<&str>) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    let active_layer = kb.pair()?.unwrap_or(0);
    let fw = kb.fw_version().unwrap_or_default();

    // Legends are best-effort: no Oryx id / no network still gives live keys.
    let (layout, layout_hash) = match LayoutId::from_serial(&fw) {
        Ok(id) => {
            let hash = id.hash.clone();
            (fetch_layout(&id, "voyager", false).ok(), hash)
        }
        Err(_) => (None, format!("unknown-{fw}")),
    };

    let geo = geometry::voyager();
    let key_count = geo.len();
    let heat = HeatmapStore::load(&layout_hash, key_count)?;
    let mut app = App {
        status: match &layout {
            Some(l) => format!("{} ({})", l.title, l.hash_id),
            None => "no Oryx layout (keys shown without legends)".into(),
        },
        layout,
        active_layer,
        view_layer: active_layer,
        follow: true,
        show_heat: false,
        pressed: vec![false; key_count],
        heat,
        key_count,
    };

    // HID reader thread → channel.
    let (tx, rx) = mpsc::channel::<Event>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = stop.clone();
    let reader = std::thread::spawn(move || {
        while !stop_reader.load(Ordering::SeqCst) {
            match kb.read_event(Duration::from_millis(150)) {
                Ok(Some(ev)) => {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break, // unplugged; UI will show stale state until quit
            }
        }
        kb.disconnect();
    });

    let mut terminal = ratatui::init();
    let res = event_loop(&mut terminal, &mut app, &rx);
    ratatui::restore();

    stop.store(true, Ordering::SeqCst);
    let _ = reader.join();
    app.heat.save()?;
    res
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &mpsc::Receiver<Event>,
) -> Result<()> {
    loop {
        // Drain pending keyboard events.
        while let Ok(ev) = rx.try_recv() {
            handle_hid_event(app, ev);
        }
        app.heat.autosave()?;

        terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            if let TermEvent::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Left => {
                        app.view_layer = app.view_layer.saturating_sub(1);
                        app.follow = false;
                    }
                    KeyCode::Right => {
                        let max = app.max_layer();
                        app.view_layer = (app.view_layer + 1).min(max);
                        app.follow = false;
                    }
                    KeyCode::Char('f') => {
                        app.follow = true;
                        app.view_layer = app.active_layer;
                    }
                    KeyCode::Char('h') => app.show_heat = !app.show_heat,
                    _ => {}
                }
            }
        }
    }
}

impl App {
    fn max_layer(&self) -> u8 {
        self.layout
            .as_ref()
            .map(|l| l.revision.layers.len().saturating_sub(1) as u8)
            .unwrap_or(15)
    }

    fn layer(&self, n: u8) -> Option<&crate::oryx_api::Layer> {
        self.layout
            .as_ref()
            .and_then(|l| l.revision.layers.iter().find(|la| la.position == n))
    }
}

fn handle_hid_event(app: &mut App, ev: Event) {
    let geo = geometry::voyager();
    match ev {
        Event::Layer(n) => {
            app.active_layer = n;
            if app.follow {
                app.view_layer = n;
            }
        }
        Event::KeyDown { col, row } => {
            if let Some(idx) = geo.key_index(row, col) {
                app.pressed[idx] = true;
                app.heat.record(app.active_layer, idx, app.key_count);
            }
        }
        Event::KeyUp { col, row } => {
            if let Some(idx) = geo.key_index(row, col) {
                app.pressed[idx] = false;
            }
        }
        _ => {}
    }
}

fn draw(f: &mut Frame, app: &App) {
    let geo = geometry::voyager();
    let (kb_w, kb_h) = KeyboardWidget::size(geo);
    let chunks = RtLayout::vertical([
        Constraint::Length(2),
        Constraint::Length(kb_h),
        Constraint::Length(1),
    ])
    .split(f.area());

    // Header: layout name + layer tabs.
    let mut spans: Vec<Span> = vec![
        Span::styled(" keyjitsu live ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&app.status),
        Span::raw("   "),
    ];
    let layer_count = app.max_layer() + 1;
    for n in 0..layer_count {
        let title = app
            .layer(n)
            .and_then(|l| l.title.clone())
            .unwrap_or_else(|| format!("L{n}"));
        let mut style = Style::default().add_modifier(Modifier::DIM);
        if n == app.view_layer {
            style = Style::default().add_modifier(Modifier::BOLD);
        }
        if n == app.active_layer {
            style = style.fg(Color::Green).remove_modifier(Modifier::DIM);
        }
        spans.push(Span::styled(format!(" {n}:{title} "), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

    // Keyboard.
    let mut widget = KeyboardWidget::new(geo, app.layer(app.view_layer));
    widget.pressed = app.pressed.clone();
    if app.show_heat {
        widget.heat = Some(normalize(&app.heat.counts(Some(app.view_layer), app.key_count)));
    }
    let kb_area = Rect {
        x: chunks[1].x,
        y: chunks[1].y,
        width: chunks[1].width.min(kb_w),
        height: chunks[1].height.min(kb_h),
    };
    f.render_widget(&widget, kb_area);

    // Footer.
    let heat_state = if app.show_heat { "on" } else { "off" };
    let follow = if app.follow { "following" } else { "pinned" };
    let footer = format!(
        " q quit · ←/→ browse layers ({follow}) · f follow active · h heatmap [{heat_state}] · {} presses recorded",
        app.heat.total_presses()
    );
    f.render_widget(
        Paragraph::new(footer).style(Style::default().add_modifier(Modifier::DIM)),
        chunks[2],
    );
}
