//! Minimap HUD settings and preview rendering.

use super::*;

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
            toggle_row(
                ui,
                "Show shortcut/instruction panel beside this layer",
                &mut c.show_instructions,
            );
            if !c.show_instructions {
                return;
            }
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
    pub(super) fn peek_preview_card(
        &mut self,
        ui: &mut egui::Ui,
        c: &mut PeekConfig,
        layer_cfg: &config::MinimapLayerConfig,
    ) {
        card(ui, "Preview", |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 185.0),
                egui::Sense::hover(),
            );
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

    /// Draw the peek's card + minimap inside `rect` (for the inline preview).
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
        // Fit the minimap into the rect: unit from the HEIGHT budget (the board
        // is PEEK_BOARD_UNITS_TALL units tall incl. the rotated thumbs), width follows.
        let header_h = if c.show_layer_name { 32.0 } else { 0.0 };
        let hint_w = if layer_cfg.show_instructions && !layer_cfg.instructions.is_empty() {
            240.0
        } else {
            0.0
        };
        let unit_fit = ((rect.height() - 24.0 - header_h) / PEEK_BOARD_UNITS_TALL)
            .min((rect.width() - 44.0 - hint_w) / PEEK_BOARD_UNITS_WIDE);
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
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
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
                    if layer_cfg.show_instructions && !layer_cfg.instructions.is_empty() {
                        ui.add_space(10.0);
                        minimap_instruction_panel(
                            ui,
                            &layer_cfg.instructions,
                            c.opacity.clamp(0.08, 1.0),
                            accent,
                        );
                    }
                });
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
        let scale = c.scale.clamp(0.5, 1.6);
        // Card padding + header add to the raw keyboard size.
        let pad = 16.0;
        let header = if c.show_layer_name { 40.0 } else { 0.0 };
        let kb_w = 620.0 * scale;
        // draw_keyboard sizes by width, so derive the unit from it.
        let unit = kb_w / PEEK_BOARD_UNITS_WIDE;
        let hint_w = if layer_cfg.show_instructions && !layer_cfg.instructions.is_empty() {
            260.0 * scale
        } else {
            0.0
        };
        let width = kb_w + hint_w + pad * 2.0;
        let combo_h = if c.show_combo { 42.0 } else { 0.0 };
        let height = unit * PEEK_BOARD_UNITS_TALL + header + combo_h + pad * 2.0;

        // Position on the selected monitor (falls back to the main display).
        let (mx, my, mw, mh) = self.peek_monitor_rect(ctx, c.monitor);
        let edge = 48.0;
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
        // Overall translucency, applied to the whole overlay so it reads like
        // frosted glass (panel + keys + text fade together) rather than a solid
        // panel with opaque keys.
        let opacity = c.opacity.clamp(0.08, 1.0);
        let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
        // With the combo HUD on, mirror physically-held keys so a hold lights
        // up live on the minimap; otherwise no press highlight.
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
        let show_instructions = layer_cfg.show_instructions && !layer_cfg.instructions.is_empty();
        let instructions = layer_cfg.instructions.clone();

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("keyjitsu_peek"),
            builder,
            move |ctx, _class| {
                // The window is transparent; the opacity is baked directly into
                // every color's alpha (card, keys, text), so the whole overlay
                // genuinely becomes see-through as the slider goes down - the
                // desktop shows through more, not a fade-to-black.
                let a = (opacity * 255.0) as u8;
                // Background (dark card) is optional - off = keys float on pure
                // transparency. Colors are optional too (monochrome high-contrast).
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
                        .inner_margin(egui::Margin::same(pad as i8))
                        .corner_radius(egui::CornerRadius::same(16))
                        .shadow(if show_bg {
                            egui::epaint::Shadow {
                                offset: [0, 6],
                                blur: 22,
                                spread: 0,
                                color: Color32::from_black_alpha((90.0 * opacity) as u8),
                            }
                        } else {
                            egui::epaint::Shadow::NONE
                        });
                    card.show(ui, |ui| {
                        if show_name {
                            ui.horizontal(|ui| {
                                egui::Frame::new()
                                    .fill(Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        a,
                                    ))
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(9, 4))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(format!("L{peek_layer}")).strong().color(
                                                Color32::from_rgba_unmultiplied(255, 255, 255, a),
                                            ),
                                        );
                                    });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(&title)
                                        .size(16.0)
                                        .color(Color32::from_rgba_unmultiplied(235, 236, 242, a)),
                                );
                            });
                            ui.add_space(8.0);
                        }
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
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
                                    ui.add_space(6.0);
                                    combo_strip(ui, &combo, opacity, accent, show_combo_ms);
                                }
                            });
                            if show_instructions {
                                ui.add_space(12.0);
                                minimap_instruction_panel(ui, &instructions, opacity, accent);
                            }
                        });
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
) {
    let a = (opacity.clamp(0.08, 1.0) * 255.0) as u8;
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
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(210.0);
            for row in rows
                .iter()
                .filter(|r| !r.keys.trim().is_empty() || !r.desc.trim().is_empty())
            {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(36, 38, 49, a))
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&row.keys)
                                    .monospace()
                                    .size(11.5)
                                    .color(Color32::from_rgba_unmultiplied(245, 245, 250, a)),
                            );
                        });
                    ui.label(
                        RichText::new(&row.desc)
                            .size(11.5)
                            .color(Color32::from_rgba_unmultiplied(215, 216, 225, a)),
                    );
                });
            }
        });
}
