//! FX Studio page and RGB effect editing.

use super::*;

impl App {
    pub(super) fn ui_fx_studio(&mut self, ui: &mut egui::Ui) {
        centered_page(ui, 1120.0, |ui| {
            page_header(
                ui,
                "FX Studio",
                "Browse built-ins, keep editable copies in My effects, or paint a custom sequence.",
            );

            card(ui, "Library", |ui| {
                ui.label(
                    RichText::new("DEFAULT · CONTINUOUS")
                        .size(10.5)
                        .color(pal::TEXT_DIM),
                );
                ui.horizontal_wrapped(|ui| {
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
                ui.add_space(6.0);
                ui.label(
                    RichText::new("DEFAULT · ON KEY PRESS")
                        .size(10.5)
                        .color(pal::TEXT_DIM),
                );
                ui.horizontal_wrapped(|ui| {
                    for (e, label) in PressEffect::ALL {
                        if e == PressEffect::None {
                            continue;
                        }
                        let name = label
                            .replace("This key - ", "")
                            .replace("Whole board - ", "Board · ");
                        if ui
                            .selectable_label(self.fx_sel == FxSel::Press(e), name)
                            .clicked()
                        {
                            self.fx_sel = FxSel::Press(e);
                            self.fx_events.clear();
                            self.fx_t0 = Instant::now();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("MY EFFECTS").size(10.5).color(pal::TEXT_DIM));
                    let mut select = None;
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
                        self.fx_events.clear();
                    }
                    if ui.button("＋ New custom").clicked() {
                        let n = self.custom_fx.len() + 1;
                        self.custom_fx.push(CustomFx {
                            name: format!("my effect {n}"),
                            steps: vec![FxStep {
                                keys: Vec::new(),
                                color: [138, 92, 246],
                                ms: 220,
                            }],
                            background: config::FxBackgroundMode::Preserve,
                            preset: None,
                        });
                        self.fx_sel = FxSel::Custom(self.custom_fx.len() - 1);
                        self.fx_step = 0;
                        self.fx_playing = false;
                        self.save_custom_fx();
                    }
                });
            });

            let full = ui.available_width();
            let ed_w = 270.0;
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

            card(ui, "Board output", |ui| self.ui_rgb_effects(ui));
        });
    }

    fn unique_fx_name(&self, base: &str) -> String {
        let base = format!("{base} copy");
        if !self.custom_fx.iter().any(|c| c.name == base) {
            return base;
        }
        for n in 2..1000 {
            let candidate = format!("{base} {n}");
            if !self.custom_fx.iter().any(|c| c.name == candidate) {
                return candidate;
            }
        }
        format!("{base} new")
    }

    fn save_builtin_as_editable_copy(&mut self) {
        let (name, preset) = match self.fx_sel {
            FxSel::Const(effect) => (
                self.unique_fx_name(effect.label().split(" -").next().unwrap_or(effect.label())),
                FxPresetSource::Constant {
                    effect,
                    color: self.fx_color,
                    speed: self.fx_speed,
                    brightness: self.fx_bright,
                },
            ),
            FxSel::Press(effect) => (
                self.unique_fx_name(
                    effect
                        .label()
                        .replace("This key - ", "")
                        .replace("Whole board - ", "Board ")
                        .as_str(),
                ),
                FxPresetSource::Press {
                    effect,
                    color: self.fx_color,
                },
            ),
            FxSel::Custom(_) => return,
        };
        self.custom_fx.push(CustomFx {
            name,
            steps: Vec::new(),
            background: config::FxBackgroundMode::Preserve,
            preset: Some(preset),
        });
        self.fx_sel = FxSel::Custom(self.custom_fx.len() - 1);
        self.fx_t0 = Instant::now();
        self.save_custom_fx();
    }

    /// Tuning always edits a user-owned copy. Built-ins remain immutable and
    /// expose a single explicit "Save editable copy" action.
    pub(super) fn fx_tune_card(&mut self, ui: &mut egui::Ui) {
        if let FxSel::Custom(i) = self.fx_sel {
            if i >= self.custom_fx.len() {
                self.fx_sel = FxSel::Press(PressEffect::Ripple);
                return;
            }
            if self.custom_fx[i].preset.is_some() {
                self.fx_preset_editor(ui, i);
            } else {
                self.fx_custom_editor(ui, i);
            }
            return;
        }

        card(ui, "Default effect", |ui| {
            let (name, uses_color, is_press) = match self.fx_sel {
                FxSel::Const(e) => (e.label().to_string(), e.uses_color(), false),
                FxSel::Press(e) => (e.label().to_string(), e.uses_color(), true),
                FxSel::Custom(_) => unreachable!(),
            };
            ui.label(RichText::new(name).strong().size(15.0).color(pal::TEXT));
            ui.label(
                RichText::new(if is_press {
                    "Built-in key reaction · preview settings are temporary until you save a copy."
                } else {
                    "Built-in board effect · preview settings are temporary until you save a copy."
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
                let on = self.connected.is_some();
                if ui
                    .add_enabled(on, egui::Button::new("⚡ Test on keyboard"))
                    .on_disabled_hover_text("Plug in the Voyager to test on it")
                    .clicked()
                {
                    if let (FxSel::Press(e), Ok(mut a)) = (self.fx_sel, self.anim.lock()) {
                        let now = Instant::now();
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos() as u64)
                            .unwrap_or(0);
                        a.events.push(FxEvent {
                            key: 16,
                            effect: e,
                            color: self.fx_color,
                            at: now,
                            seed,
                            seq: None,
                            replace_base: false,
                        });
                    }
                }
            }
            ui.add_space(8.0);
            if ui.button("Save editable copy").clicked() {
                self.save_builtin_as_editable_copy();
            }
        });
    }

    fn fx_preset_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        card(ui, "My effect", |ui| {
            let mut dirty = false;
            let mut name = self.custom_fx[i].name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY))
                .changed()
            {
                self.rename_custom_fx(i, name);
            }
            ui.add_space(6.0);

            let preset = self.custom_fx[i].preset.clone();
            match preset {
                Some(FxPresetSource::Constant {
                    mut effect,
                    mut color,
                    mut speed,
                    mut brightness,
                }) => {
                    labeled(ui, "Effect", |ui| {
                        egui::ComboBox::from_id_salt(("saved_const_fx", i))
                            .selected_text(effect.label())
                            .show_ui(ui, |ui| {
                                for (candidate, label) in Effect::ALL {
                                    if candidate != Effect::Off {
                                        dirty |= ui
                                            .selectable_value(&mut effect, candidate, label)
                                            .changed();
                                    }
                                }
                            });
                    });
                    if effect.uses_color() {
                        labeled(ui, "Color", |ui| {
                            dirty |= ui.color_edit_button_srgb(&mut color).changed();
                        });
                    }
                    labeled(ui, "Speed", |ui| {
                        dirty |= ui
                            .add(egui::Slider::new(&mut speed, 0.2..=3.0).show_value(false))
                            .changed();
                    });
                    labeled(ui, "Brightness", |ui| {
                        dirty |= ui
                            .add(egui::Slider::new(&mut brightness, 0.05..=1.0).show_value(false))
                            .changed();
                    });
                    if dirty {
                        self.custom_fx[i].preset = Some(FxPresetSource::Constant {
                            effect,
                            color,
                            speed,
                            brightness,
                        });
                    }
                    if ui
                        .add_enabled(
                            self.connected.is_some(),
                            egui::Button::new("⚡ Test 5 s on keyboard"),
                        )
                        .clicked()
                    {
                        if let Ok(mut a) = self.anim.lock() {
                            let until = Instant::now() + Duration::from_secs(5);
                            let mut restore = self
                                .fx_board_restore
                                .take()
                                .unwrap_or_else(|| FxBoardRestore::capture(&a, until));
                            restore.until = until;
                            a.effect = effect;
                            a.color = color;
                            a.speed = speed;
                            a.brightness = brightness;
                            self.fx_board_restore = Some(restore);
                        }
                    }
                }
                Some(FxPresetSource::Press {
                    mut effect,
                    mut color,
                }) => {
                    labeled(ui, "Effect", |ui| {
                        egui::ComboBox::from_id_salt(("saved_press_fx", i))
                            .selected_text(effect.label())
                            .show_ui(ui, |ui| {
                                for (candidate, label) in PressEffect::ALL {
                                    if candidate != PressEffect::None {
                                        dirty |= ui
                                            .selectable_value(&mut effect, candidate, label)
                                            .changed();
                                    }
                                }
                            });
                    });
                    if effect.uses_color() {
                        labeled(ui, "Color", |ui| {
                            dirty |= ui.color_edit_button_srgb(&mut color).changed();
                        });
                    }
                    if dirty {
                        self.custom_fx[i].preset = Some(FxPresetSource::Press { effect, color });
                    }
                    if ui
                        .add_enabled(
                            self.connected.is_some(),
                            egui::Button::new("⚡ Test on keyboard"),
                        )
                        .clicked()
                    {
                        if let Ok(mut a) = self.anim.lock() {
                            let now = Instant::now();
                            a.events.push(FxEvent {
                                key: 16,
                                effect,
                                color,
                                at: now,
                                seed: now.elapsed().subsec_nanos() as u64,
                                seq: None,
                                replace_base: false,
                            });
                        }
                    }
                }
                None => {}
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.weak("This is your copy. The built-in it came from stays unchanged.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗑").on_hover_text("delete this effect").clicked() {
                        self.remove_custom_fx(i);
                        self.fx_sel = FxSel::Press(PressEffect::Ripple);
                        dirty = false;
                    }
                });
            });
            if dirty {
                self.save_custom_fx();
            }
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
                self.rename_custom_fx(i, name);
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
            labeled(ui, "Tempo", |ui| {
                ui.add(egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false));
            });
            labeled(ui, "Other LEDs", |ui| {
                ui.horizontal(|ui| {
                    dirty |= ui
                        .selectable_value(
                            &mut self.custom_fx[i].background,
                            config::FxBackgroundMode::Preserve,
                            "Keep current RGB",
                        )
                        .changed();
                    dirty |= ui
                        .selectable_value(
                            &mut self.custom_fx[i].background,
                            config::FxBackgroundMode::Blackout,
                            "Turn off",
                        )
                        .changed();
                });
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
                    let until = Instant::now() + Duration::from_secs(5);
                    let mut restore = self
                        .fx_board_restore
                        .take()
                        .unwrap_or_else(|| FxBoardRestore::capture(&a, until));
                    restore.until = until;
                    a.custom = self.custom_fx[i].steps.clone();
                    a.custom_name = self.custom_fx[i].name.clone();
                    a.custom_replace_base = matches!(
                        self.custom_fx[i].background,
                        config::FxBackgroundMode::Blackout
                    );
                    a.speed = self.fx_speed;
                    a.effect = Effect::Custom;
                    self.fx_board_restore = Some(restore);
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        "Assign from Layout → On press, or choose it in Board output below.",
                    )
                    .size(11.0)
                    .color(pal::TEXT_MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new("🗑").size(11.5))
                        .on_hover_text("delete this effect")
                        .clicked()
                    {
                        self.remove_custom_fx(i);
                        self.fx_sel = FxSel::Press(PressEffect::Ripple);
                        dirty = false;
                    }
                });
            });
            if dirty {
                self.save_custom_fx();
            }
        });
    }

    /// FX Studio: the preview card (pure renderer; never touches LEDs).
    pub(super) fn fx_preview_card(&mut self, ui: &mut egui::Ui) {
        let (cols, rows) = self.board_units();
        let budget = (ui.ctx().screen_rect().height() - ui.cursor().top() - 118.0).max(170.0);
        card(ui, "Preview", |ui| {
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
                let sequence = match self.fx_sel {
                    FxSel::Custom(i) => self.custom_fx.get(i).is_some_and(|c| c.preset.is_none()),
                    _ => false,
                };
                if sequence {
                    let label = if self.fx_playing {
                        "⏸ Pause"
                    } else {
                        "▶ Play"
                    };
                    if ui.button(label).clicked() {
                        self.fx_playing = !self.fx_playing;
                        self.fx_t0 = Instant::now();
                    }
                    let hint = if self.fx_playing {
                        "playing the loop · pause to paint keys"
                    } else {
                        "paint mode · click keys to toggle the active step"
                    };
                    ui.label(RichText::new(hint).size(11.0).color(pal::TEXT_DIM));
                } else {
                    ui.label(
                        RichText::new("Click a key to trigger key-press effects.")
                            .size(11.0)
                            .color(pal::TEXT_DIM),
                    );
                }
            });
        });
    }

    fn preview_press_event(
        &mut self,
        key: usize,
        effect: PressEffect,
        color: [u8; 3],
        now: Instant,
    ) {
        self.fx_events.push(FxEvent {
            key,
            effect,
            color,
            at: now,
            seed: now.elapsed().subsec_nanos() as u64,
            seq: None,
            replace_base: false,
        });
    }

    /// The FX Studio live preview: built-ins and saved copies use the same pure
    /// render path as the device animation engine.
    pub(super) fn fx_preview(&mut self, ui: &mut egui::Ui) {
        let geo = self.geometry();
        let n = geo.len();
        let now = Instant::now();
        self.fx_events
            .retain(|ev| now.duration_since(ev.at).as_secs_f32() < 2.0);

        let selected_press = match self.fx_sel {
            FxSel::Press(e) => Some((e, self.fx_color)),
            FxSel::Custom(i) => self.custom_fx.get(i).and_then(|c| match c.preset {
                Some(FxPresetSource::Press { effect, color }) => Some((effect, color)),
                _ => None,
            }),
            _ => None,
        };
        if self.fx_playing {
            if let Some((effect, color)) = selected_press {
                if now.duration_since(self.fx_last_fire) > Duration::from_millis(1400) {
                    self.fx_last_fire = now;
                    let key = [16usize, 30, 8, 42][(now.elapsed().subsec_nanos() as usize) % 4];
                    self.preview_press_event(key % n, effect, color, now);
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
                false,
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
                false,
                geo,
            ),
            FxSel::Custom(ci) => match self.custom_fx.get(ci).cloned() {
                Some(CustomFx {
                    preset:
                        Some(FxPresetSource::Constant {
                            effect,
                            color,
                            speed,
                            brightness,
                        }),
                    ..
                }) => rgb_anim::compute(
                    effect,
                    color,
                    speed,
                    brightness,
                    if self.fx_playing { t } else { 0.35 },
                    &base,
                    &[],
                    &[],
                    false,
                    geo,
                ),
                Some(CustomFx {
                    preset: Some(FxPresetSource::Press { .. }),
                    ..
                }) => rgb_anim::compute(
                    Effect::Off,
                    [0, 0, 0],
                    1.0,
                    1.0,
                    t,
                    &base,
                    &self.fx_events,
                    &[],
                    false,
                    geo,
                ),
                Some(custom) => {
                    let replace = matches!(custom.background, config::FxBackgroundMode::Blackout);
                    if self.fx_playing {
                        rgb_anim::compute(
                            Effect::Custom,
                            [0, 0, 0],
                            self.fx_speed,
                            self.fx_bright,
                            t,
                            &base,
                            &[],
                            &custom.steps,
                            replace,
                            geo,
                        )
                    } else {
                        let mut fr = if replace {
                            vec![[0u8, 0, 0]; n]
                        } else {
                            base.clone()
                        };
                        if self.fx_step > 0 {
                            if let Some(prev) = custom.steps.get(self.fx_step - 1) {
                                for &k in &prev.keys {
                                    if (k as usize) < n {
                                        fr[k as usize] = [
                                            prev.color[0] / 4,
                                            prev.color[1] / 4,
                                            prev.color[2] / 4,
                                        ];
                                    }
                                }
                            }
                        }
                        if let Some(step) = custom.steps.get(self.fx_step) {
                            for &k in &step.keys {
                                if (k as usize) < n {
                                    fr[k as usize] = step.color;
                                }
                            }
                        }
                        fr
                    }
                }
                None => base.clone(),
            },
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
        if let Some(i) = kb.clicked {
            match self.fx_sel {
                FxSel::Press(e) => self.preview_press_event(i, e, self.fx_color, now),
                FxSel::Custom(ci) => {
                    let preset = self.custom_fx.get(ci).and_then(|c| c.preset.clone());
                    match preset {
                        Some(FxPresetSource::Press { effect, color }) => {
                            self.preview_press_event(i, effect, color, now);
                        }
                        Some(FxPresetSource::Constant { .. }) => {}
                        None => {
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
                    }
                }
                FxSel::Const(_) => {}
            }
        }
        if self.fx_playing {
            ui.ctx().request_repaint();
        }
    }
}
