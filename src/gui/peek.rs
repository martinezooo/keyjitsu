//! Peek HUD settings and preview rendering.

use super::*;

impl App {
    pub(super) fn ui_peek_page(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            "Peek",
            "A transparent, click-through minimap that flashes when a layer activates.",
        );

        let mut c = self.peek.clone();
        // Preview stays full width. Use two columns only when there is enough
        // room for both setting groups without squeezing their controls.
        self.peek_preview_card(ui, &mut c);
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

        if c != self.peek {
            self.peek = c.clone();
            if c.enabled {
                self.arm_preview(&c, 2500);
            } else {
                self.peek_until = None;
            }
            self.persist_config("saving peek settings", move |cfg| {
                cfg.peek = c;
            });
        }
    }

    pub(super) fn peek_settings_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Behaviour", |ui| {
            // A bound Voyager key shows the minimap while held, independent of
            // the auto-peek toggles.
            group_header(ui, "Shortcut", "");
            self.peek_shortcut_row(ui);
            ui.add_space(6.0);
            toggle_row(ui, "Enable layer peek", &mut c.enabled);
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
                    self.persist_config("clearing peek shortcut", |cfg| {
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

    /// Right column: a live preview of the peek over a transparency checkerboard.
    pub(super) fn peek_preview_card(&mut self, ui: &mut egui::Ui, c: &mut PeekConfig) {
        card(ui, "Preview", |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 185.0),
                egui::Sense::hover(),
            );
            draw_checkerboard(ui.painter(), rect);
            self.render_peek_into(ui, rect.shrink(14.0), c);
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

    /// Draw the peek's card + minimap inside `rect` (for the inline preview).
    pub(super) fn render_peek_into(&self, ui: &mut egui::Ui, rect: egui::Rect, c: &PeekConfig) {
        let layer = self
            .active_layer
            .max(if self.layer_count() > 1 { 1 } else { 0 });
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
        // Fit the minimap into the rect: unit from the HEIGHT budget (the board
        // is PEEK_BOARD_UNITS_TALL units tall incl. the rotated thumbs), width follows.
        let header_h = if c.show_layer_name { 32.0 } else { 0.0 };
        let unit_fit = ((rect.height() - 24.0 - header_h) / PEEK_BOARD_UNITS_TALL)
            .min((rect.width() - 44.0) / PEEK_BOARD_UNITS_WIDE);
        let kb_w = (unit_fit * PEEK_BOARD_UNITS_WIDE + 24.0).max(120.0);
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
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(10))
            .show(&mut child, |ui| {
                if c.show_layer_name {
                    ui.horizontal(|ui| {
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(
                                accent.r(),
                                accent.g(),
                                accent.b(),
                                a,
                            ))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(7, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("L{layer}"))
                                        .strong()
                                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, a)),
                                );
                            });
                        ui.label(
                            RichText::new(&title)
                                .color(Color32::from_rgba_unmultiplied(235, 236, 242, a)),
                        );
                    });
                    ui.add_space(4.0);
                }
                let press = if c.show_combo {
                    self.pressed.clone()
                } else {
                    vec![false; geo.len()]
                };
                let combo_keys = self.combo_member_mask(layer);
                ui.set_max_width(kb_w);
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
                    ui.add_space(6.0);
                    let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
                    // In the settings preview the log may be empty - show a hint.
                    let entries = self.combo_recent();
                    combo_strip(
                        ui,
                        &entries,
                        c.opacity.clamp(0.08, 1.0),
                        accent,
                        c.show_combo_ms,
                    );
                }
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
        let sample = if self.active_layer > 0 {
            self.active_layer
        } else {
            self.layer_count().saturating_sub(1).max(1)
        };
        self.peek_layer = sample;
        self.peek_until = Some(Instant::now() + Duration::from_millis(ms.max(c.duration_ms)));
    }
}
