//! Minimap HUD settings and preview rendering.

use super::*;

pub(super) fn shift_minimap_layers_after_delete(
    layers: &mut Vec<config::MinimapLayerConfig>,
    deleted: u8,
) {
    layers.retain(|c| c.layer != deleted);
    for c in layers.iter_mut() {
        if c.layer > deleted {
            c.layer -= 1;
        }
    }
    layers.sort_by_key(|c| c.layer);
}

fn hint_placement_label(p: config::MinimapHintPlacement) -> &'static str {
    match p {
        config::MinimapHintPlacement::Left => "Left",
        config::MinimapHintPlacement::Right => "Right",
        config::MinimapHintPlacement::Top => "Top",
        config::MinimapHintPlacement::Bottom => "Bottom",
        config::MinimapHintPlacement::Inline => "Inside",
    }
}

fn minimap_has_hints(layer_cfg: &config::MinimapLayerConfig) -> bool {
    layer_cfg.show_instructions
        && layer_cfg
            .instructions
            .iter()
            .any(|r| !r.keys.trim().is_empty() || !r.desc.trim().is_empty())
}

fn minimap_hint_scale(board_scale: f32, layer_cfg: &config::MinimapLayerConfig) -> f32 {
    (board_scale * layer_cfg.instruction_scale.clamp(0.5, 1.8)).clamp(0.30, 2.40)
}

fn minimap_hint_dimensions(layer_cfg: &config::MinimapLayerConfig, board_scale: f32) -> (f32, f32) {
    let count = layer_cfg
        .instructions
        .iter()
        .filter(|r| !r.keys.trim().is_empty() || !r.desc.trim().is_empty())
        .count();
    if !minimap_has_hints(layer_cfg) || count == 0 {
        return (0.0, 0.0);
    }
    let s = minimap_hint_scale(board_scale, layer_cfg);
    match layer_cfg.instruction_flow {
        config::MinimapHintFlow::Column => (220.0 * s, (18.0 + count as f32 * 28.0) * s),
        config::MinimapHintFlow::Row => {
            let cols = count.min(4).max(1);
            let rows = (count + cols - 1) / cols;
            (
                cols as f32 * 150.0 * s + cols.saturating_sub(1) as f32 * 6.0 * s,
                (18.0 + rows as f32 * 32.0) * s,
            )
        }
    }
}

fn minimap_content_size(
    c: &PeekConfig,
    layer_cfg: &config::MinimapLayerConfig,
    scale: f32,
) -> (f32, f32) {
    let kb_w = 620.0 * scale;
    let keyboard_h = 620.0 / PEEK_BOARD_UNITS_WIDE * PEEK_BOARD_UNITS_TALL * scale
        + if c.show_combo { 42.0 * scale } else { 0.0 };
    let header_h = if c.show_layer_name { 40.0 * scale } else { 0.0 };
    if !minimap_has_hints(layer_cfg) {
        return (kb_w, header_h + keyboard_h);
    }
    let (hint_w, hint_h) = minimap_hint_dimensions(layer_cfg, scale);
    let gap = 12.0 * scale;
    match layer_cfg.instruction_placement {
        config::MinimapHintPlacement::Left | config::MinimapHintPlacement::Right => {
            (kb_w + gap + hint_w, header_h + keyboard_h.max(hint_h))
        }
        config::MinimapHintPlacement::Top
        | config::MinimapHintPlacement::Bottom
        | config::MinimapHintPlacement::Inline => {
            (kb_w.max(hint_w), header_h + keyboard_h + gap + hint_h)
        }
    }
}

fn scaled_i8(value: f32, scale: f32) -> i8 {
    (value * scale).round().clamp(1.0, i8::MAX as f32) as i8
}

fn scaled_u8(value: f32, scale: f32) -> u8 {
    (value * scale).round().clamp(1.0, u8::MAX as f32) as u8
}

fn minimap_layer_header(
    ui: &mut egui::Ui,
    layer: u8,
    title: &str,
    accent: Color32,
    alpha: u8,
    scale: f32,
) {
    let s = scale.clamp(0.30, 2.40);
    ui.horizontal(|ui| {
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                alpha,
            ))
            .corner_radius(egui::CornerRadius::same(scaled_u8(7.0, s)))
            .inner_margin(egui::Margin::symmetric(
                scaled_i8(8.0, s),
                scaled_i8(4.0, s),
            ))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("L{layer}"))
                        .strong()
                        .size((13.0 * s).clamp(7.0, 28.0))
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, alpha)),
                );
            });
        ui.add_space(6.0 * s);
        ui.label(
            RichText::new(title)
                .size((16.0 * s).clamp(8.0, 32.0))
                .color(Color32::from_rgba_unmultiplied(235, 236, 242, alpha)),
        );
    });
    ui.add_space(7.0 * s);
}

fn minimap_content_layout(
    ui: &mut egui::Ui,
    placement: config::MinimapHintPlacement,
    show_hints: bool,
    gap: f32,
    mut header: impl FnMut(&mut egui::Ui),
    mut board: impl FnMut(&mut egui::Ui),
    mut hints: impl FnMut(&mut egui::Ui),
) {
    if !show_hints {
        header(ui);
        board(ui);
        return;
    }
    match placement {
        config::MinimapHintPlacement::Top => {
            hints(ui);
            ui.add_space(gap);
            header(ui);
            board(ui);
        }
        config::MinimapHintPlacement::Inline => {
            header(ui);
            hints(ui);
            ui.add_space(gap);
            board(ui);
        }
        config::MinimapHintPlacement::Bottom => {
            header(ui);
            board(ui);
            ui.add_space(gap);
            hints(ui);
        }
        config::MinimapHintPlacement::Left => {
            header(ui);
            ui.horizontal(|ui| {
                hints(ui);
                ui.add_space(gap);
                board(ui);
            });
        }
        config::MinimapHintPlacement::Right => {
            header(ui);
            ui.horizontal(|ui| {
                board(ui);
                ui.add_space(gap);
                hints(ui);
            });
        }
    }
}

impl App {
    pub(super) fn minimap_layer_config(&self, layer: u8) -> config::MinimapLayerConfig {
        self.minimap_layers
            .iter()
            .find(|c| c.layer == layer)
            .cloned()
            .unwrap_or_else(|| config::MinimapLayerConfig {
                layer,
                settings: self.peek.clone(),
                ..config::MinimapLayerConfig::default()
            })
    }

    pub(super) fn minimap_settings(&self, layer: u8) -> PeekConfig {
        self.minimap_layer_config(layer).settings
    }

    fn store_minimap_layer(&mut self, layer_cfg: config::MinimapLayerConfig) {
        if let Some(existing) = self
            .minimap_layers
            .iter_mut()
            .find(|c| c.layer == layer_cfg.layer)
        {
            *existing = layer_cfg;
        } else {
            self.minimap_layers.push(layer_cfg);
            self.minimap_layers.sort_by_key(|c| c.layer);
        }
        let saved = self.minimap_layers.clone();
        self.persist_config("saving minimap layer", move |cfg| {
            cfg.minimap_layers = saved;
        });
    }

    pub(super) fn ui_peek_page(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            "Minimap",
            "A transparent, click-through map for each layer, with optional shortcut hints.",
        );

        let mut layer_cfg = self.minimap_layer_config(self.peek_layer);
        let original_layer_cfg = layer_cfg.clone();
        let mut c = layer_cfg.settings.clone();
        // Preview stays full width. Use two columns only when there is enough
        // room for both setting groups without squeezing their controls.
        self.peek_preview_card(ui, &mut c, &layer_cfg);
        ui.add_space(8.0);
        self.minimap_instructions_card(ui, &mut layer_cfg);
        ui.add_space(8.0);
        if ui.available_width() >= 760.0 {
            ui.columns(2, |cols| {
                self.peek_settings_card(&mut cols[0], &mut c);
                self.peek_appearance_card(&mut cols[1], &mut c);
                cols[1].add_space(8.0);
                self.peek_position_card(&mut cols[1], &mut c);
            });
        } else {
            self.peek_settings_card(ui, &mut c);
            ui.add_space(8.0);
            self.peek_appearance_card(ui, &mut c);
            ui.add_space(8.0);
            self.peek_position_card(ui, &mut c);
        }

        layer_cfg.settings = c.clone();
        if layer_cfg != original_layer_cfg {
            self.store_minimap_layer(layer_cfg);
            if c.enabled {
                self.arm_preview(&c, 2500);
            } else {
                self.peek_until = None;
            }
        }
    }

    pub(super) fn peek_settings_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Behaviour", |ui| {
            // A bound Voyager key shows the minimap while held, independent of
            // the auto-peek toggles.
            group_header(ui, "Shortcut", "");
            self.peek_shortcut_row(ui);
            ui.add_space(6.0);
            toggle_row(ui, "Enable minimap for this layer", &mut c.enabled);
            ui.add_enabled_ui(c.enabled, |ui| {
                toggle_row(ui, "Only outside the base layer", &mut c.only_non_base);
                toggle_row(ui, "Show background panel", &mut c.show_background);
                toggle_row(ui, "Black & white (high contrast)", &mut c.monochrome);
                toggle_row(ui, "Show layer name", &mut c.show_layer_name);
                toggle_row(ui, "Show key legends", &mut c.show_legends);
                toggle_row(ui, "Show recent key combos", &mut c.show_combo);
                ui.add_enabled_ui(c.show_combo, |ui| {
                    toggle_row(ui, "Show combo timings (ms)", &mut c.show_combo_ms);
                });
            });
        });
    }

    pub(super) fn minimap_instructions_card(
        &mut self,
        ui: &mut egui::Ui,
        c: &mut config::MinimapLayerConfig,
    ) {
        card(ui, "Layer hints", |ui| {
            toggle_row(ui, "Show layer hints", &mut c.show_instructions);
            if !c.show_instructions {
                return;
            }
            ui.add_space(4.0);
            labeled(ui, "Position", |ui| {
                egui::ComboBox::from_id_salt(("minimap_hint_position", c.layer))
                    .width(150.0)
                    .selected_text(hint_placement_label(c.instruction_placement))
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (config::MinimapHintPlacement::Left, "Left"),
                            (config::MinimapHintPlacement::Right, "Right"),
                            (config::MinimapHintPlacement::Top, "Top"),
                            (config::MinimapHintPlacement::Bottom, "Bottom"),
                            (config::MinimapHintPlacement::Inline, "Inside"),
                        ] {
                            ui.selectable_value(&mut c.instruction_placement, value, label);
                        }
                    });
            });
            labeled(ui, "Layout", |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut c.instruction_flow,
                        config::MinimapHintFlow::Column,
                        "Column",
                    );
                    ui.selectable_value(
                        &mut c.instruction_flow,
                        config::MinimapHintFlow::Row,
                        "Row",
                    );
                });
            });
            labeled(ui, "Hint size", |ui| {
                ui.add_sized(
                    [180.0, 20.0],
                    egui::Slider::new(&mut c.instruction_scale, 0.5..=1.8).suffix("×"),
                );
            });
            ui.add_space(4.0);
            let mut remove = None;
            for (i, row) in c.instructions.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut row.keys)
                            .hint_text("e.g. H / J / K / L")
                            .desired_width(150.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut row.desc)
                            .hint_text("what it does")
                            .desired_width(300.0),
                    );
                    if ui.small_button("✕").on_hover_text("remove hint").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                c.instructions.remove(i);
            }
            if ui.button("＋ add hint").clicked() {
                c.instructions.push(config::MinimapInstruction {
                    keys: String::new(),
                    desc: String::new(),
                });
            }
        });
    }

    /// Right column: timing + look of the minimap.
    pub(super) fn peek_appearance_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Appearance", |ui| {
            let wide = 220.0;
            ui.add_enabled_ui(c.enabled, |ui| {
                labeled(ui, "Show for", |ui| {
                    ui.add_sized(
                        [wide, 20.0],
                        egui::Slider::new(&mut c.duration_ms, 300..=5000).suffix(" ms"),
                    );
                });
                labeled(ui, "Transparency", |ui| {
                    ui.add_sized(
                        [wide, 20.0],
                        egui::Slider::new(&mut c.opacity, 0.08..=1.0).show_value(false),
                    );
                });
                labeled(ui, "Size", |ui| {
                    ui.add_sized(
                        [wide, 20.0],
                        egui::Slider::new(&mut c.scale, 0.5..=1.6).show_value(false),
                    );
                });
                labeled(ui, "Accent color", |ui| {
                    ui.color_edit_button_srgb(&mut c.accent);
                });
            });
        });
    }

    /// Position controls in their own card (right column) so the settings
    /// list stays short.
    pub(super) fn peek_position_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Position", |ui| {
            let wide = 220.0;
            ui.add_enabled_ui(c.enabled, |ui| {
                labeled(ui, "Monitor", |ui| {
                    self.peek_monitor_combo(ui, &mut c.monitor);
                });
                ui.add_space(4.0);
                labeled(ui, "Anchor", |ui| {
                    position_grid(ui, &mut c.valign, &mut c.halign);
                });
                ui.add_space(4.0);
                labeled(ui, "Nudge X", |ui| {
                    ui.add_sized(
                        [wide, 20.0],
                        egui::Slider::new(&mut c.offset[0], -1200.0..=1200.0).suffix(" px"),
                    );
                });
                labeled(ui, "Nudge Y", |ui| {
                    ui.add_sized(
                        [wide, 20.0],
                        egui::Slider::new(&mut c.offset[1], -1200.0..=1200.0).suffix(" px"),
                    );
                });
            });
        });
    }

    /// Name a chord like "⌥ + Spc" from layer-0 legends.
    pub(super) fn chord_label(&self, chord: &[[u8; 2]]) -> String {
        chord
            .iter()
            .map(|&[r, c]| {
                self.geometry()
                    .key_index(r, c)
                    .and_then(|i| self.device_key(0, i).map(|k| labels_for(&k).tap))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("r{r}c{c}"))
            })
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// One-line bind/rebind/clear row for the minimap shortcut combo.
    pub(super) fn peek_shortcut_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.binding_overlay {
                if self.binding_draft.is_empty() {
                    ui.colored_label(
                        pal::AMBER,
                        "press the key or combo on the Voyager (release = save)…",
                    );
                } else {
                    ui.colored_label(
                        pal::AMBER,
                        format!(
                            "combo: {} - release to save",
                            self.chord_label(&self.binding_draft.clone())
                        ),
                    );
                }
                if ui.button("cancel").clicked() {
                    self.binding_overlay = false;
                    self.binding_draft.clear();
                }
            } else if !self.overlay_chord.is_empty() {
                let label = self.chord_label(&self.overlay_chord.clone());
                ui.label(RichText::new(format!("hold {label} → show minimap")).color(pal::TEXT));
                if ui.button("rebind").clicked() {
                    self.binding_overlay = true;
                    self.binding_draft.clear();
                }
                if ui.button("✕ clear").clicked() {
                    self.overlay_chord.clear();
                    self.persist_config("clearing minimap shortcut", |cfg| {
                        cfg.overlay_chord.clear();
                        cfg.overlay_trigger = None;
                    });
                }
            } else {
                ui.weak("no shortcut");
                if ui.button("＋ bind a key or combo").clicked() {
                    self.binding_overlay = true;
                    self.binding_draft.clear();
                }
            }
        });
    }

    fn minimap_preview_layout(
        width: f32,
        c: &PeekConfig,
        layer_cfg: &config::MinimapLayerConfig,
    ) -> (f32, f32, f32) {
        let requested = c.scale.clamp(0.5, 1.6);
        let available_w = (width - 44.0).max(180.0);
        let (requested_w, requested_h) = minimap_content_size(c, layer_cfg, requested);
        let fit_w = (available_w / requested_w.max(1.0)).min(1.0);
        let fit_h = (480.0 / requested_h.max(1.0)).min(1.0);
        let scale = (requested * fit_w.min(fit_h)).max(0.24);
        let (_, content_h) = minimap_content_size(c, layer_cfg, scale);
        let keyboard_w = 620.0 * scale;
        ((content_h + 28.0).clamp(140.0, 520.0), keyboard_w, scale)
    }

    /// Live in-app preview of the selected layer minimap.
    pub(super) fn peek_preview_card(
        &mut self,
        ui: &mut egui::Ui,
        c: &mut PeekConfig,
        layer_cfg: &config::MinimapLayerConfig,
    ) {
        card(ui, "Preview", |ui| {
            let preview_w = ui.available_width();
            let (preview_h, _, _) = Self::minimap_preview_layout(preview_w, c, layer_cfg);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(preview_w, preview_h), egui::Sense::hover());
            draw_checkerboard(ui.painter(), rect);
            self.render_peek_into(ui, rect.shrink(14.0), c, layer_cfg);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("📌 Keep preview visible").clicked() {
                    let snap = c.clone();
                    self.arm_preview(&snap, 5000);
                }
                if ui.button("Reset to defaults").clicked() {
                    // Preserve the chosen monitor/offset, reset the rest.
                    let (monitor, offset) = (c.monitor, c.offset);
                    *c = PeekConfig {
                        monitor,
                        offset,
                        ..PeekConfig::default()
                    };
                }
            });
        });
    }

    /// Draw the selected layer minimap inside `rect` (for the inline preview).
    pub(super) fn render_peek_into(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        c: &PeekConfig,
        layer_cfg: &config::MinimapLayerConfig,
    ) {
        let layer = self.peek_layer.min(self.layer_count().saturating_sub(1));
        let geo = self.geometry();
        let glow = self.glow_colors(layer);
        let device_layer = self.device_layer(layer);
        let legends = if c.show_legends {
            device_layer.as_ref()
        } else {
            None
        };
        let title = device_layer
            .as_ref()
            .and_then(|l| l.title.clone())
            .unwrap_or_else(|| format!("Layer {layer}"));
        let a = (c.opacity.clamp(0.08, 1.0) * 255.0) as u8;
        let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
        );
        let (_, kb_w, preview_scale) = Self::minimap_preview_layout(rect.width(), c, layer_cfg);
        let hint_scale = minimap_hint_scale(preview_scale, layer_cfg);
        let show_hints = minimap_has_hints(layer_cfg);
        let card_fill = if c.show_background {
            Color32::from_rgba_unmultiplied(17, 18, 24, a)
        } else {
            Color32::TRANSPARENT
        };
        egui::Frame::new()
            .fill(card_fill)
            .stroke(if c.show_background {
                egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        (a as f32 * 0.6) as u8,
                    ),
                )
            } else {
                egui::Stroke::NONE
            })
            .corner_radius(egui::CornerRadius::same(scaled_u8(12.0, preview_scale)))
            .inner_margin(egui::Margin::same(scaled_i8(10.0, preview_scale)))
            .show(&mut child, |ui| {
                let press = if c.show_combo {
                    self.pressed.clone()
                } else {
                    vec![false; geo.len()]
                };
                let combo_keys = self.combo_member_mask(layer);
                minimap_content_layout(
                    ui,
                    layer_cfg.instruction_placement,
                    show_hints,
                    10.0 * preview_scale,
                    |ui| {
                        if c.show_layer_name {
                            minimap_layer_header(ui, layer, &title, accent, a, preview_scale);
                        }
                    },
                    |ui| {
                        ui.set_width(kb_w);
                        draw_keyboard(
                            ui,
                            geo,
                            legends,
                            &glow,
                            &press,
                            None,
                            Some(&combo_keys),
                            c.opacity.clamp(0.08, 1.0),
                            c.monochrome,
                        );
                        if c.show_combo {
                            ui.add_space(6.0 * preview_scale);
                            let entries = self.combo_recent();
                            combo_strip(
                                ui,
                                &entries,
                                c.opacity.clamp(0.08, 1.0),
                                accent,
                                c.show_combo_ms,
                            );
                        }
                    },
                    |ui| {
                        minimap_instruction_panel(
                            ui,
                            &layer_cfg.instructions,
                            c.opacity.clamp(0.08, 1.0),
                            accent,
                            hint_scale,
                            layer_cfg.instruction_flow,
                        );
                    },
                );
            });
    }

    /// Monitor selector: lists monitors by their real name + resolution
    /// (from the cached list).
    pub(super) fn peek_monitor_combo(&self, ui: &mut egui::Ui, monitor: &mut usize) {
        let mons = &self.monitors_cache;
        if mons.is_empty() {
            ui.weak("current app window (native monitor list unavailable)");
            *monitor = 0;
            return;
        }
        if mons.len() == 1 {
            ui.weak(mons[0].label(0));
            *monitor = 0;
            return;
        }
        if *monitor >= mons.len() {
            *monitor = 0;
        }
        egui::ComboBox::from_id_salt("peek_monitor")
            .width(240.0)
            .selected_text(mons[*monitor].label(*monitor))
            .show_ui(ui, |ui| {
                for (i, m) in mons.iter().enumerate() {
                    ui.selectable_value(monitor, i, m.label(i));
                }
            });
    }

    /// Show a sample layer's peek for `ms`, so settings changes are visible.
    pub(super) fn arm_preview(&mut self, c: &PeekConfig, ms: u64) {
        self.peek_layer = self.peek_layer.min(self.layer_count().saturating_sub(1));
        self.peek_until = Some(Instant::now() + Duration::from_millis(ms.max(c.duration_ms)));
    }

    pub(super) fn maybe_peek(&mut self, n: u8) {
        let c = self.minimap_settings(n);
        if !c.enabled {
            self.peek_until = None;
            return;
        }
        if c.only_non_base && n == 0 {
            // Returning to base: dismiss any active peek immediately.
            self.peek_until = None;
            return;
        }
        self.peek_layer = n;
        // "Only outside the base layer" = the minimap stays up for the whole
        // stay on the layer (dismissed by the return to base above); otherwise
        // it's a timed flash.
        self.peek_until = Some(if c.only_non_base {
            Instant::now() + Duration::from_secs(3600)
        } else {
            Instant::now() + Duration::from_millis(c.duration_ms)
        });
    }

    /// Draw the transparent, click-through layer peek as its own polished,
    /// card-like viewport, positioned on the chosen monitor.
    pub(super) fn show_peek(&self, ctx: &egui::Context) {
        let layer_cfg = self.minimap_layer_config(self.peek_layer);
        let c = &layer_cfg.settings;
        let geo = self.geometry();
        let edge = 48.0;
        let show_hints = minimap_has_hints(&layer_cfg);
        let (mx, my, mw, mh) = self.peek_monitor_rect(ctx, c.monitor);
        let requested_scale = c.scale.clamp(0.5, 1.6);
        let requested_pad = (16.0 * requested_scale).clamp(8.0, 26.0);
        let (requested_w, requested_h) = minimap_content_size(c, &layer_cfg, requested_scale);
        let available_w = (mw - edge * 2.0 - requested_pad * 2.0).max(180.0);
        let available_h = (mh - edge * 2.0 - requested_pad * 2.0 - 24.0).max(160.0);
        let fit = (available_w / requested_w.max(1.0))
            .min(available_h / requested_h.max(1.0))
            .min(1.0);
        let scale = (requested_scale * fit).max(0.24);
        let pad = (16.0 * scale).clamp(6.0, 26.0);
        let (content_w, content_h) = minimap_content_size(c, &layer_cfg, scale);
        let kb_w = 620.0 * scale;
        let width = content_w + pad * 2.0;
        let height = content_h + pad * 2.0;

        let x =
            mx + match c.halign {
                HAlign::Left => edge,
                HAlign::Center => (mw - width) / 2.0,
                HAlign::Right => mw - width - edge,
            } + c.offset[0];
        let y =
            my + match c.valign {
                VAlign::Top => edge,
                VAlign::Middle => (mh - height) / 2.0,
                VAlign::Bottom => mh - height - edge - 24.0,
            } + c.offset[1];

        let builder = egui::ViewportBuilder::default()
            .with_title("keyjitsu minimap")
            .with_inner_size([width, height])
            .with_position([x, y])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_taskbar(false)
            .with_mouse_passthrough(true)
            .with_always_on_top();

        let device_layer = self.device_layer(self.peek_layer);
        let glow = self.glow_colors(self.peek_layer);
        let legends = if c.show_legends {
            device_layer.as_ref()
        } else {
            None
        };
        let title = device_layer
            .as_ref()
            .and_then(|l| l.title.clone())
            .unwrap_or_else(|| format!("Layer {}", self.peek_layer));
        let opacity = c.opacity.clamp(0.08, 1.0);
        let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
        let no_press = if c.show_combo {
            self.pressed.clone()
        } else {
            vec![false; geo.len()]
        };
        let peek_layer = self.peek_layer;
        let combo_keys = self.combo_member_mask(peek_layer);
        let show_name = c.show_layer_name;
        let show_bg = c.show_background;
        let mono = c.monochrome;
        let show_combo = c.show_combo;
        let show_combo_ms = c.show_combo_ms;
        let combo = if show_combo {
            self.combo_recent()
        } else {
            Vec::new()
        };
        let instructions = layer_cfg.instructions.clone();
        let hint_placement = layer_cfg.instruction_placement;
        let hint_flow = layer_cfg.instruction_flow;
        let hint_scale = minimap_hint_scale(scale, &layer_cfg);

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("keyjitsu_peek"),
            builder,
            move |ctx, _class| {
                let a = (opacity * 255.0) as u8;
                let card_fill = if show_bg {
                    Color32::from_rgba_unmultiplied(17, 18, 24, a)
                } else {
                    Color32::TRANSPARENT
                };
                let card_stroke = if show_bg {
                    egui::Stroke::new(
                        1.0,
                        Color32::from_rgba_unmultiplied(
                            accent.r(),
                            accent.g(),
                            accent.b(),
                            (a as f32 * 0.6) as u8,
                        ),
                    )
                } else {
                    egui::Stroke::NONE
                };
                let clear = egui::Frame::new().fill(Color32::TRANSPARENT);
                egui::CentralPanel::default().frame(clear).show(ctx, |ui| {
                    let card = egui::Frame::new()
                        .fill(card_fill)
                        .stroke(card_stroke)
                        .inner_margin(egui::Margin::same(scaled_i8(16.0, scale)))
                        .corner_radius(egui::CornerRadius::same(scaled_u8(16.0, scale)))
                        .shadow(if show_bg {
                            egui::epaint::Shadow {
                                offset: [0, scaled_i8(6.0, scale)],
                                blur: scaled_u8(22.0, scale),
                                spread: 0,
                                color: Color32::from_black_alpha((90.0 * opacity) as u8),
                            }
                        } else {
                            egui::epaint::Shadow::NONE
                        });
                    card.show(ui, |ui| {
                        minimap_content_layout(
                            ui,
                            hint_placement,
                            show_hints,
                            12.0 * scale,
                            |ui| {
                                if show_name {
                                    minimap_layer_header(ui, peek_layer, &title, accent, a, scale);
                                }
                            },
                            |ui| {
                                ui.set_width(kb_w);
                                draw_keyboard(
                                    ui,
                                    geo,
                                    legends,
                                    &glow,
                                    &no_press,
                                    None,
                                    Some(&combo_keys),
                                    opacity,
                                    mono,
                                );
                                if show_combo {
                                    ui.add_space(6.0 * scale);
                                    combo_strip(ui, &combo, opacity, accent, show_combo_ms);
                                }
                            },
                            |ui| {
                                minimap_instruction_panel(
                                    ui,
                                    &instructions,
                                    opacity,
                                    accent,
                                    hint_scale,
                                    hint_flow,
                                );
                            },
                        );
                    });
                });
            },
        );
    }

    /// (x, y, w, h) of the monitor to show the peek on, in egui point space.
    pub(super) fn peek_monitor_rect(
        &self,
        ctx: &egui::Context,
        monitor: usize,
    ) -> (f32, f32, f32, f32) {
        if let Some(m) = self
            .monitors_cache
            .get(monitor)
            .or_else(|| self.monitors_cache.first())
        {
            return (m.x, m.y, m.w, m.h);
        }
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            return (rect.min.x, rect.min.y, rect.width(), rect.height());
        }
        let mon = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(egui::vec2(1440.0, 900.0));
        (0.0, 0.0, mon.x, mon.y)
    }
}

fn minimap_instruction_panel(
    ui: &mut egui::Ui,
    rows: &[config::MinimapInstruction],
    opacity: f32,
    accent: Color32,
    scale: f32,
    flow: config::MinimapHintFlow,
) {
    let s = scale.clamp(0.30, 2.40);
    let a = (opacity.clamp(0.08, 1.0) * 255.0) as u8;
    let font = (11.5 * s).clamp(7.0, 24.0);
    let key_fill = Color32::from_rgba_unmultiplied(36, 38, 49, a);
    let text = Color32::from_rgba_unmultiplied(215, 216, 225, a);
    let key_text = Color32::from_rgba_unmultiplied(245, 245, 250, a);
    let visible = rows
        .iter()
        .filter(|r| !r.keys.trim().is_empty() || !r.desc.trim().is_empty())
        .collect::<Vec<_>>();
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            20,
            21,
            28,
            (a as f32 * 0.82) as u8,
        ))
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                (a as f32 * 0.55) as u8,
            ),
        ))
        .corner_radius(egui::CornerRadius::same(scaled_u8(10.0, s)))
        .inner_margin(egui::Margin::same(scaled_i8(10.0, s)))
        .show(ui, |ui| match flow {
            config::MinimapHintFlow::Column => {
                ui.set_min_width(210.0 * s);
                for row in &visible {
                    ui.horizontal(|ui| {
                        egui::Frame::new()
                            .fill(key_fill)
                            .corner_radius(egui::CornerRadius::same(scaled_u8(5.0, s)))
                            .inner_margin(egui::Margin::symmetric(
                                scaled_i8(6.0, s),
                                scaled_i8(3.0, s),
                            ))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&row.keys)
                                        .monospace()
                                        .size(font)
                                        .color(key_text),
                                );
                            });
                        ui.label(RichText::new(&row.desc).size(font).color(text));
                    });
                }
            }
            config::MinimapHintFlow::Row => {
                ui.horizontal_wrapped(|ui| {
                    for row in &visible {
                        egui::Frame::new()
                            .fill(key_fill)
                            .corner_radius(egui::CornerRadius::same(scaled_u8(6.0, s)))
                            .inner_margin(egui::Margin::symmetric(
                                scaled_i8(7.0, s),
                                scaled_i8(4.0, s),
                            ))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&row.keys)
                                            .monospace()
                                            .strong()
                                            .size(font)
                                            .color(key_text),
                                    );
                                    if !row.desc.trim().is_empty() {
                                        ui.label(RichText::new(&row.desc).size(font).color(text));
                                    }
                                });
                            });
                        ui.add_space(4.0 * s);
                    }
                });
            }
        });
}

#[cfg(test)]
mod minimap_layout_tests {
    use super::*;

    fn hints(n: usize) -> config::MinimapLayerConfig {
        config::MinimapLayerConfig {
            layer: 1,
            show_instructions: true,
            instructions: (0..n)
                .map(|i| config::MinimapInstruction {
                    keys: format!("K{i}"),
                    desc: "Action".into(),
                })
                .collect(),
            ..config::MinimapLayerConfig::default()
        }
    }

    #[test]
    fn inline_preview_tracks_the_requested_minimap_scale() {
        let mut large = PeekConfig::default();
        large.scale = 1.0;
        let mut small = large.clone();
        small.scale = 0.5;
        let layer = config::MinimapLayerConfig::default();
        let (_, large_w, large_scale) = App::minimap_preview_layout(1100.0, &large, &layer);
        let (_, small_w, small_scale) = App::minimap_preview_layout(1100.0, &small, &layer);
        assert!(small_w < large_w);
        assert!(small_scale < large_scale);
    }

    #[test]
    fn layer_delete_drops_deleted_minimap_and_shifts_higher_layers() {
        let mut layers = vec![
            config::MinimapLayerConfig {
                layer: 1,
                ..Default::default()
            },
            config::MinimapLayerConfig {
                layer: 3,
                ..Default::default()
            },
            config::MinimapLayerConfig {
                layer: 4,
                ..Default::default()
            },
        ];
        shift_minimap_layers_after_delete(&mut layers, 3);
        assert_eq!(
            layers.iter().map(|c| c.layer).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn hint_scale_follows_the_minimap_and_keeps_a_user_multiplier() {
        let mut layer = hints(3);
        layer.instruction_scale = 1.25;
        let small = minimap_hint_scale(0.5, &layer);
        let large = minimap_hint_scale(1.0, &layer);
        assert!(small < large);
        assert!((large - 1.25).abs() < 0.001);
    }

    #[test]
    fn top_hints_preserve_more_keyboard_width_than_side_hints() {
        let c = PeekConfig::default();
        let mut side = hints(4);
        side.instruction_placement = config::MinimapHintPlacement::Right;
        let mut top = side.clone();
        top.instruction_placement = config::MinimapHintPlacement::Top;
        let (_, side_w, _) = App::minimap_preview_layout(760.0, &c, &side);
        let (_, top_w, _) = App::minimap_preview_layout(760.0, &c, &top);
        assert!(top_w >= side_w);
    }
}
