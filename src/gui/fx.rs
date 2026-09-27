//! FX Studio page and RGB effect editing.

use super::*;

impl App {
    pub(super) fn ui_fx_studio(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        // "board RGB" (sidebar) = the application panel: what the physical
        // keyboard actually runs, moved here from Tools.
        if self.fx_lib == FxLib::Apply {
            page_header(ui, "FX Studio", "What the board runs right now: the constant effect plus the global press reaction.");
            card(ui, "Board RGB", |ui| self.ui_rgb_effects(ui));
            return;
        }
        page_header(
            ui,
            "FX Studio",
            "Pick an effect, tune it, and test it in the preview or on the board.",
        );

        // --- Library: one category at a time (picked in the sidebar), so the
        //     list stays a single short row of chips.
        card(ui, "Library", |ui| {
            match self.fx_lib {
                FxLib::Const => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("CONSTANT").size(10.5).color(pal::TEXT_DIM));
                        for (e, label) in Effect::ALL {
                            if e == Effect::Off {
                                continue;
                            }
                            let name = label
                                .split(" -")
                                .next()
                                .unwrap_or(label)
                                .split(" (")
                                .next()
                                .unwrap_or(label);
                            if ui
                                .selectable_label(self.fx_sel == FxSel::Const(e), name)
                                .clicked()
                            {
                                self.fx_sel = FxSel::Const(e);
                                self.fx_t0 = Instant::now();
                            }
                        }
                    });
                }
                FxLib::Press => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("ON PRESS").size(10.5).color(pal::TEXT_DIM));
                        for (e, label) in PressEffect::ALL {
                            if e == PressEffect::None {
                                continue;
                            }
                            let name = label
                                .replace("This key - ", "")
                                .replace("Whole board - ", "🌐 ");
                            if ui
                                .selectable_label(self.fx_sel == FxSel::Press(e), name)
                                .clicked()
                            {
                                self.fx_sel = FxSel::Press(e);
                                self.fx_events.clear();
                                self.fx_t0 = Instant::now();
                            }
                        }
                        ui.label(
                            RichText::new("🌐 = whole board")
                                .size(10.5)
                                .color(pal::TEXT_DIM),
                        );
                    });
                }
                FxLib::Apply => unreachable!(),
                FxLib::Custom => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("CUSTOM").size(10.5).color(pal::TEXT_DIM));
                        let mut select: Option<usize> = None;
                        for (i, c) in self.custom_fx.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    self.fx_sel == FxSel::Custom(i),
                                    format!("★ {}", c.name),
                                )
                                .clicked()
                            {
                                select = Some(i);
                            }
                        }
                        if let Some(i) = select {
                            self.fx_sel = FxSel::Custom(i);
                            self.fx_step = 0;
                            self.fx_t0 = Instant::now();
                        }
                        if ui.button("＋ new").clicked() {
                            let n = self.custom_fx.len() + 1;
                            self.custom_fx.push(CustomFx {
                                name: format!("my effect {n}"),
                                steps: vec![FxStep {
                                    keys: Vec::new(),
                                    color: [138, 92, 246],
                                    ms: 220,
                                }],
                            });
                            self.fx_sel = FxSel::Custom(self.custom_fx.len() - 1);
                            self.fx_step = 0;
                            self.fx_playing = false; // start in paint mode
                            self.save_custom_fx();
                        }
                    });
                }
            }
        });
        ui.add_space(8.0);

        // --- Tune + Preview: side by side when there's room, stacked when not
        //     - sized from the available width so nothing is ever cut off.
        let full = ui.available_width();
        let ed_w = 250.0;
        if full - ed_w - 16.0 >= 540.0 {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ed_w);
                    self.fx_tune_card(ui);
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(full - ed_w - 16.0);
                    self.fx_preview_card(ui);
                });
            });
        } else {
            self.fx_tune_card(ui);
            ui.add_space(8.0);
            self.fx_preview_card(ui);
        }
    }

    /// FX Studio: the tuning card - the effect's knobs plus a way to test it
    /// on the physical board. Assigning lives elsewhere (key editor / Tools).
    pub(super) fn fx_tune_card(&mut self, ui: &mut egui::Ui) {
        if let FxSel::Custom(i) = self.fx_sel {
            if i < self.custom_fx.len() {
                self.fx_custom_editor(ui, i);
            } else {
                self.fx_sel = FxSel::Press(PressEffect::Ripple);
            }
            return;
        }
        card(ui, "Tune", |ui| {
            let (name, uses_color, is_press) = match self.fx_sel {
                FxSel::Const(e) => (e.label().to_string(), e.uses_color(), false),
                FxSel::Press(e) => (e.label().to_string(), e.uses_color(), true),
                FxSel::Custom(_) => unreachable!(),
            };
            ui.label(RichText::new(name).strong().size(15.0).color(pal::TEXT));
            ui.label(
                RichText::new(if is_press {
                    "plays from a key, over your RGB"
                } else {
                    "whole board, runs continuously"
                })
                .size(11.5)
                .color(pal::TEXT_DIM),
            );
            ui.add_space(8.0);
            if uses_color {
                labeled(ui, "Color", |ui| {
                    ui.color_edit_button_srgb(&mut self.fx_color);
                });
            }
            if !is_press {
                labeled(ui, "Speed", |ui| {
                    ui.add(egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false));
                });
                labeled(ui, "Brightness", |ui| {
                    ui.add(egui::Slider::new(&mut self.fx_bright, 0.05..=1.0).show_value(false));
                });
            }
            if is_press {
                ui.add_space(8.0);
                let on = self.connected.is_some();
                let test = ui
                    .add_enabled(
                        on,
                        egui::Button::new(
                            RichText::new("⚡ Test on keyboard").color(Color32::WHITE),
                        )
                        .fill(pal::VIOLET),
                    )
                    .on_disabled_hover_text("Plug in the Voyager to test on it");
                if test.clicked() {
                    if let (FxSel::Press(e), Ok(mut a)) = (self.fx_sel, self.anim.lock()) {
                        let now = Instant::now();
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos() as u64)
                            .unwrap_or(0);
                        // Fire from a central key; board-wide effects ignore it.
                        a.events.push(FxEvent {
                            key: 16,
                            effect: e,
                            color: self.fx_color,
                            at: now,
                            seed,
                            seq: None,
                        });
                    }
                }
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                RichText::new("Use it per key in Live (key → On press), or board-wide under board RGB (left).")
                    .size(11.0)
                    .color(pal::TEXT_MUTED),
            );
        });
    }

    /// FX Studio: the step-sequencer editor for a user-built effect. Paint
    /// keys in the preview, duplicate the step, nudge it a row up - repeat.
    pub(super) fn fx_custom_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        card(ui, "Sequence", |ui| {
            let mut dirty = false;
            let mut name = self.custom_fx[i].name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY))
                .changed()
            {
                self.custom_fx[i].name = name;
                dirty = true;
            }
            ui.add_space(6.0);

            // A sequence loaded from a hand-edited config could have no steps;
            // seed one so the per-step indexing below can't panic.
            if self.custom_fx[i].steps.is_empty() {
                self.custom_fx[i].steps.push(FxStep {
                    keys: Vec::new(),
                    color: [138, 92, 246],
                    ms: 220,
                });
            }
            // Step chips: select the one being painted; ＋ duplicates it.
            let n_steps = self.custom_fx[i].steps.len();
            self.fx_step = self.fx_step.min(n_steps.saturating_sub(1));
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("STEPS").size(10.5).color(pal::TEXT_DIM));
                for s in 0..n_steps {
                    if ui
                        .selectable_label(self.fx_step == s, format!("{}", s + 1))
                        .clicked()
                    {
                        self.fx_step = s;
                        self.fx_playing = false;
                    }
                }
                if ui
                    .button("＋")
                    .on_hover_text("duplicate this step (then nudge it)")
                    .clicked()
                {
                    let copy = self.custom_fx[i].steps[self.fx_step].clone();
                    self.custom_fx[i].steps.insert(self.fx_step + 1, copy);
                    self.fx_step += 1;
                    self.fx_playing = false;
                    dirty = true;
                }
            });
            ui.add_space(4.0);

            // Active step controls - compact rows so the card fits the window.
            let s = self.fx_step;
            let step = &mut self.custom_fx[i].steps[s];
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("step {} · {} keys", s + 1, step.keys.len()))
                        .size(12.0)
                        .color(pal::TEXT_MUTED),
                );
                dirty |= ui.color_edit_button_srgb(&mut step.color).changed();
                dirty |= ui
                    .add(
                        egui::DragValue::new(&mut step.ms)
                            .range(40..=2000)
                            .speed(10)
                            .suffix(" ms"),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                let mut mv: Option<(f32, f32)> = None;
                if ui.button("←").on_hover_text("nudge left").clicked() {
                    mv = Some((-1.0, 0.0));
                }
                if ui.button("↑").on_hover_text("nudge up").clicked() {
                    mv = Some((0.0, -1.0));
                }
                if ui.button("↓").on_hover_text("nudge down").clicked() {
                    mv = Some((0.0, 1.0));
                }
                if ui.button("→").on_hover_text("nudge right").clicked() {
                    mv = Some((1.0, 0.0));
                }
                if let Some((dx, dy)) = mv {
                    self.shift_step(i, s, dx, dy);
                }
                if ui
                    .button("clear")
                    .on_hover_text("unpaint all keys of this step")
                    .clicked()
                {
                    self.custom_fx[i].steps[s].keys.clear();
                    dirty = true;
                }
                if n_steps > 1 && ui.button("✕").on_hover_text("delete this step").clicked() {
                    self.custom_fx[i].steps.remove(s);
                    self.fx_step = self.fx_step.saturating_sub(1);
                    dirty = true;
                }
            });
            labeled(ui, "tempo", |ui| {
                ui.add(egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false));
            });
            ui.add_space(6.0);

            let on = self.connected.is_some();
            if ui
                .add_enabled(
                    on,
                    egui::Button::new(
                        RichText::new("⚡ Test 5 s on keyboard").color(Color32::WHITE),
                    )
                    .fill(pal::VIOLET),
                )
                .clicked()
            {
                if let Ok(mut a) = self.anim.lock() {
                    let prev = if a.effect == Effect::Custom {
                        Effect::Off
                    } else {
                        a.effect
                    };
                    a.custom = self.custom_fx[i].steps.clone();
                    a.custom_name = self.custom_fx[i].name.clone();
                    a.speed = self.fx_speed;
                    a.effect = Effect::Custom;
                    self.fx_board_restore = Some((Instant::now() + Duration::from_secs(5), prev));
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Use: ▶ board RGB → constant effect → ★")
                        .size(11.0)
                        .color(pal::TEXT_MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new("🗑").size(11.5))
                        .on_hover_text("delete this effect")
                        .clicked()
                    {
                        self.custom_fx.remove(i);
                        self.fx_sel = FxSel::Press(PressEffect::Ripple);
                        dirty = true;
                    }
                });
            });
            if dirty {
                self.save_custom_fx();
            }
        });
    }

    /// FX Studio: the preview card (pure `compute`, never touches the LEDs).
    pub(super) fn fx_preview_card(&mut self, ui: &mut egui::Ui) {
        // Fit the preview into the window's remaining height (the widget keeps
        // its own legible floor), so the studio fits without scrolling.
        let (cols, rows) = self.board_units();
        // Reserve room for the card header/margins and the play-controls row.
        let budget = (ui.ctx().screen_rect().height() - ui.cursor().top() - 118.0).max(170.0);
        card(ui, "Preview", |ui| {
            // Match the widget's own legible floor (34px/unit) so the container
            // is never narrower than what draw_keyboard will actually paint.
            let w = ui
                .available_width()
                .min((budget / rows).max(34.0) * cols + 24.0);
            let pad = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    self.fx_preview(ui);
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if self.fx_playing {
                    "⏸ Pause"
                } else {
                    "▶ Play"
                };
                if ui.button(label).clicked() {
                    self.fx_playing = !self.fx_playing;
                    self.fx_t0 = Instant::now();
                }
                let hint = match self.fx_sel {
                    FxSel::Custom(_) if !self.fx_playing => format!(
                        "painting step {} - click keys to toggle them",
                        self.fx_step + 1
                    ),
                    FxSel::Custom(_) => {
                        "playing the loop - pause to paint · clicks still paint".to_string()
                    }
                    _ => "click keys in the preview to fire the effect".to_string(),
                };
                ui.label(RichText::new(hint).size(11.0).color(pal::TEXT_DIM));
            });
        });
    }

    /// The FX Studio live preview: renders the engine's pure `compute` on the
    /// on-screen keyboard - never touches the physical LEDs.
    pub(super) fn fx_preview(&mut self, ui: &mut egui::Ui) {
        let geo = self.geometry();
        let n = geo.len();
        let now = Instant::now();
        // Auto-fire press effects periodically so the preview animates itself.
        if self.fx_playing {
            if let FxSel::Press(e) = self.fx_sel {
                self.fx_events
                    .retain(|ev| now.duration_since(ev.at).as_secs_f32() < 1.5);
                if now.duration_since(self.fx_last_fire) > Duration::from_millis(1400) {
                    self.fx_last_fire = now;
                    let key = [16usize, 30, 8, 42][(now.elapsed().subsec_nanos() as usize) % 4];
                    self.fx_events.push(FxEvent {
                        key: key % n,
                        effect: e,
                        color: self.fx_color,
                        at: now,
                        seed: now.elapsed().subsec_nanos() as u64,
                        seq: None,
                    });
                }
            }
        }

        let base = self.glow_rgb(self.view_layer);
        let t = self.fx_t0.elapsed().as_secs_f32();
        let frame = match self.fx_sel {
            FxSel::Const(e) => rgb_anim::compute(
                e,
                self.fx_color,
                self.fx_speed,
                self.fx_bright,
                if self.fx_playing { t } else { 0.35 },
                &base,
                &[],
                &[],
                geo,
            ),
            FxSel::Press(_) => rgb_anim::compute(
                Effect::Off,
                [0, 0, 0],
                1.0,
                1.0,
                t,
                &base,
                &self.fx_events,
                &[],
                geo,
            ),
            FxSel::Custom(ci) => {
                let steps = self
                    .custom_fx
                    .get(ci)
                    .map(|c| c.steps.clone())
                    .unwrap_or_default();
                if self.fx_playing {
                    rgb_anim::compute(
                        Effect::Custom,
                        [0, 0, 0],
                        self.fx_speed,
                        self.fx_bright,
                        t,
                        &base,
                        &[],
                        &steps,
                        geo,
                    )
                } else {
                    // Paint mode: the active step bright, the previous step as
                    // a dim onion-skin so nudged copies line up visually.
                    let mut fr = vec![[0u8, 0, 0]; n];
                    if self.fx_step > 0 {
                        if let Some(p) = steps.get(self.fx_step - 1) {
                            for &k in &p.keys {
                                if (k as usize) < n {
                                    fr[k as usize] =
                                        [p.color[0] / 4, p.color[1] / 4, p.color[2] / 4];
                                }
                            }
                        }
                    }
                    if let Some(sdef) = steps.get(self.fx_step) {
                        for &k in &sdef.keys {
                            if (k as usize) < n {
                                fr[k as usize] = sdef.color;
                            }
                        }
                    }
                    fr
                }
            }
        };
        let glow: Vec<Option<Color32>> = frame
            .iter()
            .map(|c| (*c != [0, 0, 0]).then(|| Color32::from_rgb(c[0], c[1], c[2])))
            .collect();
        let no_press = vec![false; n];
        let device_layer = self.device_layer(self.view_layer);
        let combo_keys = self.combo_member_mask(self.view_layer);
        let kb = draw_keyboard(
            ui,
            geo,
            device_layer.as_ref(),
            &glow,
            &no_press,
            None,
            Some(&combo_keys),
            1.0,
            false,
        );
        // Clicking the preview: fire the press effect, or paint the custom step.
        if let Some(i) = kb.clicked {
            match self.fx_sel {
                FxSel::Press(e) => {
                    self.fx_events.push(FxEvent {
                        key: i,
                        effect: e,
                        color: self.fx_color,
                        at: now,
                        seed: now.elapsed().subsec_nanos() as u64,
                        seq: None,
                    });
                }
                FxSel::Custom(ci) => {
                    let step = self.fx_step;
                    if let Some(st) = self
                        .custom_fx
                        .get_mut(ci)
                        .and_then(|c| c.steps.get_mut(step))
                    {
                        match st.keys.iter().position(|&k| k as usize == i) {
                            Some(p) => {
                                st.keys.remove(p);
                            }
                            None => st.keys.push(i as u16),
                        }
                        self.save_custom_fx();
                    }
                }
                FxSel::Const(_) => {}
            }
        }
        if self.fx_playing {
            ui.ctx().request_repaint();
        }
    }

    /// Write the current heatmap view to ~/Downloads as a CSV.
}
