//! FX Studio page and RGB effect editing.

use super::*;

const FX_TEST_LEAD_MS: u64 = 350;
const FX_TEST_TAIL_MS: u64 = 350;
const FX_TEST_BOARD_MS: u64 = 5000;
const FX_TEST_PRESS_MS: u64 = 1400;

fn fx_test_active_ms(spec: &FxTestSpec) -> u64 {
    match spec {
        FxTestSpec::Press { .. } => FX_TEST_PRESS_MS,
        _ => FX_TEST_BOARD_MS,
    }
}

fn fx_preview_step(custom: &CustomFx, elapsed: Duration) -> usize {
    if custom.steps.is_empty() {
        return 0;
    }
    let total_ms: u64 = custom.steps.iter().map(|step| step.ms.max(30)).sum();
    if total_ms == 0 {
        return 0;
    }
    let speed = custom.speed.max(0.01);
    let cursor_ms = ((elapsed.as_secs_f32() * speed * 1000.0) as u64) % total_ms;
    let mut boundary = 0;
    for (index, step) in custom.steps.iter().enumerate() {
        boundary += step.ms.max(30);
        if cursor_ms < boundary {
            return index;
        }
    }
    custom.steps.len() - 1
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FxLibraryGroup {
    BuiltIn,
    Press,
    Mine,
}

impl App {
    pub(super) fn ui_fx_studio(&mut self, ui: &mut egui::Ui) {
        centered_page(ui, PAGE_MAX_WIDTH, |ui| {
            page_header(
                ui,
                "FX Studio",
                "Choose an effect, tune it, then test it on the physical keyboard.",
            );

            if let Some(label) = self.fx_test_status() {
                status_pill(ui, &label, pal::VIOLET_HI);
                ui.add_space(6.0);
            }

            self.fx_selector_card(ui);
            ui.add_space(8.0);

            let custom_sequence = match self.fx_sel {
                FxSel::Custom(i) => self.custom_fx.get(i).is_some_and(|c| c.preset.is_none()),
                _ => false,
            };

            if custom_sequence {
                self.fx_tune_card(ui);
                ui.add_space(8.0);
                self.fx_step_keyboard_card(ui);
                ui.add_space(8.0);
                card(ui, "Active keyboard RGB", |ui| self.ui_rgb_effects(ui));
            } else if ui.available_width() >= 780.0 {
                ui.columns(2, |cols| {
                    self.fx_tune_card(&mut cols[0]);
                    card(&mut cols[1], "Active keyboard RGB", |ui| {
                        self.ui_rgb_effects(ui)
                    });
                });
            } else {
                self.fx_tune_card(ui);
                ui.add_space(8.0);
                card(ui, "Active keyboard RGB", |ui| self.ui_rgb_effects(ui));
            }
        });
    }

    fn fx_library_group(&self) -> FxLibraryGroup {
        match self.fx_sel {
            FxSel::Const(_) => FxLibraryGroup::BuiltIn,
            FxSel::Press(_) => FxLibraryGroup::Press,
            FxSel::Custom(_) => FxLibraryGroup::Mine,
        }
    }

    fn fx_selector_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Effect", |ui| {
            let mut group = self.fx_library_group();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .selectable_value(&mut group, FxLibraryGroup::BuiltIn, "Built-in")
                    .clicked()
                {
                    let effect = Effect::ALL
                        .iter()
                        .map(|(effect, _)| *effect)
                        .find(|effect| *effect != Effect::Off)
                        .unwrap_or(Effect::Layout);
                    self.fx_sel = FxSel::Const(effect);
                    self.fx_preview = None;
                    self.fx_delete_confirm = None;
                    self.fx_delete_step_confirm = None;
                }
                if ui
                    .selectable_value(&mut group, FxLibraryGroup::Press, "On key press")
                    .clicked()
                {
                    let effect = PressEffect::ALL
                        .iter()
                        .map(|(effect, _)| *effect)
                        .find(|effect| *effect != PressEffect::None)
                        .unwrap_or(PressEffect::Ripple);
                    self.fx_sel = FxSel::Press(effect);
                    self.fx_preview = None;
                    self.fx_delete_confirm = None;
                    self.fx_delete_step_confirm = None;
                }
                let mine = ui
                    .add_enabled(
                        !self.custom_fx.is_empty(),
                        egui::Button::selectable(group == FxLibraryGroup::Mine, "My effects"),
                    )
                    .on_disabled_hover_text("Create your first custom effect with + New");
                if mine.clicked() {
                    let last = self.fx_last_custom.min(self.custom_fx.len() - 1);
                    self.fx_sel = FxSel::Custom(last);
                    self.fx_step = 0;
                    self.fx_preview = None;
                }
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Effect").size(11.0).color(pal::TEXT_DIM));
                match self.fx_library_group() {
                    FxLibraryGroup::BuiltIn => {
                        let selected = match self.fx_sel {
                            FxSel::Const(effect) => effect,
                            _ => Effect::Layout,
                        };
                        egui::ComboBox::from_id_salt("fx_library_builtin")
                            .width(250.0)
                            .selected_text(selected.label())
                            .show_ui(ui, |ui| {
                                for (effect, label) in Effect::ALL {
                                    if effect != Effect::Off {
                                        ui.selectable_value(
                                            &mut self.fx_sel,
                                            FxSel::Const(effect),
                                            label,
                                        );
                                    }
                                }
                            });
                    }
                    FxLibraryGroup::Press => {
                        let selected = match self.fx_sel {
                            FxSel::Press(effect) => effect,
                            _ => PressEffect::Ripple,
                        };
                        egui::ComboBox::from_id_salt("fx_library_press")
                            .width(250.0)
                            .selected_text(selected.label())
                            .show_ui(ui, |ui| {
                                for (effect, label) in PressEffect::ALL {
                                    if effect != PressEffect::None {
                                        ui.selectable_value(
                                            &mut self.fx_sel,
                                            FxSel::Press(effect),
                                            label,
                                        );
                                    }
                                }
                            });
                    }
                    FxLibraryGroup::Mine => {
                        let selected = match self.fx_sel {
                            FxSel::Custom(i) => self
                                .custom_fx
                                .get(i)
                                .map(|c| c.name.as_str())
                                .unwrap_or("My effect"),
                            _ => "My effect",
                        };
                        egui::ComboBox::from_id_salt("fx_library_custom")
                            .width(250.0)
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (i, custom) in self.custom_fx.iter().enumerate() {
                                    if ui
                                        .selectable_label(
                                            self.fx_sel == FxSel::Custom(i),
                                            &custom.name,
                                        )
                                        .clicked()
                                    {
                                        self.fx_sel = FxSel::Custom(i);
                                        self.fx_last_custom = i;
                                        self.fx_step = 0;
                                        self.fx_preview = None;
                                        self.fx_delete_confirm = None;
                                        self.fx_delete_step_confirm = None;
                                    }
                                }
                            });
                    }
                }

                if ui
                    .button("＋ New sequence")
                    .on_hover_text("Create a new custom sequence")
                    .clicked()
                {
                    let n = self.custom_fx.len() + 1;
                    self.custom_fx.push(CustomFx {
                        name: format!("Untitled sequence {n}"),
                        steps: vec![FxStep {
                            keys: Vec::new(),
                            color: [138, 92, 246],
                            ms: 220,
                        }],
                        speed: 1.0,
                        background: config::FxBackgroundMode::Preserve,
                        preset: None,
                    });
                    let created = self.custom_fx.len() - 1;
                    self.fx_sel = FxSel::Custom(created);
                    self.fx_last_custom = created;
                    self.fx_step = 0;
                    self.fx_preview = None;
                    self.fx_delete_confirm = None;
                    self.fx_delete_step_confirm = None;
                    self.save_custom_fx();
                }
            });
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
            speed: 1.0,
            background: config::FxBackgroundMode::Preserve,
            preset: Some(preset),
        });
        let created = self.custom_fx.len() - 1;
        self.fx_sel = FxSel::Custom(created);
        self.fx_last_custom = created;
        self.fx_preview = None;
        self.save_custom_fx();
    }

    /// Built-ins are immutable. Tuning either tests them temporarily or saves
    /// an editable copy in My effects.
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

        card(ui, "Selected effect", |ui| {
            let (name, uses_color, is_press) = match self.fx_sel {
                FxSel::Const(e) => (e.label().to_string(), e.uses_color(), false),
                FxSel::Press(e) => (e.label().to_string(), e.uses_color(), true),
                FxSel::Custom(_) => unreachable!(),
            };
            ui.label(RichText::new(name).strong().size(15.0).color(pal::TEXT));
            ui.label(
                RichText::new("Built-in preset · test it first or save your own editable copy.")
                    .size(11.5)
                    .color(pal::TEXT_DIM),
            );
            ui.add_space(8.0);

            if uses_color {
                fx_field(ui, "Color", |ui| {
                    ui.color_edit_button_srgb(&mut self.fx_color);
                });
            }
            if !is_press {
                fx_field(ui, "Speed", |ui| {
                    ui.add_sized(
                        [220.0, 20.0],
                        egui::Slider::new(&mut self.fx_speed, 0.2..=3.0).show_value(false),
                    );
                });
                fx_field(ui, "Brightness", |ui| {
                    ui.add_sized(
                        [220.0, 20.0],
                        egui::Slider::new(&mut self.fx_bright, 0.05..=1.0).show_value(false),
                    );
                });
            }

            ui.horizontal(|ui| {
                let enabled = self.connected.is_some();
                if ui
                    .add_enabled(enabled, egui::Button::new("⚡ Test on keyboard"))
                    .on_disabled_hover_text("Plug in the Voyager to test effects")
                    .clicked()
                {
                    let spec = match self.fx_sel {
                        FxSel::Const(effect) => FxTestSpec::Board {
                            effect,
                            color: self.fx_color,
                            speed: self.fx_speed,
                            brightness: self.fx_bright,
                        },
                        FxSel::Press(effect) => FxTestSpec::Press {
                            effect,
                            color: self.fx_color,
                        },
                        FxSel::Custom(_) => unreachable!(),
                    };
                    self.queue_fx_test(spec);
                }
                if ui.button("Save editable copy").clicked() {
                    self.save_builtin_as_editable_copy();
                }
            });
        });
    }

    fn fx_preset_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        self.fx_last_custom = i;
        card(ui, "My effect", |ui| {
            let mut dirty = false;
            let mut name = self.custom_fx[i].name.clone();
            if ui
                .add_sized(
                    [280.0, 24.0],
                    egui::TextEdit::singleline(&mut name).hint_text("effect name"),
                )
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
                    fx_field(ui, "Effect", |ui| {
                        egui::ComboBox::from_id_salt(("saved_const_fx", i))
                            .width(240.0)
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
                        fx_field(ui, "Color", |ui| {
                            dirty |= ui.color_edit_button_srgb(&mut color).changed();
                        });
                    }
                    fx_field(ui, "Speed", |ui| {
                        dirty |= ui
                            .add_sized(
                                [220.0, 20.0],
                                egui::Slider::new(&mut speed, 0.2..=3.0).show_value(false),
                            )
                            .changed();
                    });
                    fx_field(ui, "Brightness", |ui| {
                        dirty |= ui
                            .add_sized(
                                [220.0, 20.0],
                                egui::Slider::new(&mut brightness, 0.05..=1.0).show_value(false),
                            )
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
                            egui::Button::new("⚡ Test on keyboard"),
                        )
                        .clicked()
                    {
                        self.queue_fx_test(FxTestSpec::Board {
                            effect,
                            color,
                            speed,
                            brightness,
                        });
                    }
                }
                Some(FxPresetSource::Press {
                    mut effect,
                    mut color,
                }) => {
                    fx_field(ui, "Effect", |ui| {
                        egui::ComboBox::from_id_salt(("saved_press_fx", i))
                            .width(240.0)
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
                        fx_field(ui, "Color", |ui| {
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
                        self.queue_fx_test(FxTestSpec::Press { effect, color });
                    }
                }
                None => {}
            }

            ui.add_space(6.0);
            if dirty {
                self.save_custom_fx();
            }
            self.fx_delete_effect_row(ui, i, "Your copy · built-in preset stays unchanged.");
        });
    }

    /// Step editor paired with an animated in-app preview below it.
    pub(super) fn fx_custom_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        self.fx_last_custom = i;
        card(ui, "Custom sequence", |ui| {
            let mut dirty = false;
            let mut name = self.custom_fx[i].name.clone();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_sized(
                        [320.0, 26.0],
                        egui::TextEdit::singleline(&mut name).hint_text("effect name"),
                    )
                    .changed()
                {
                    self.rename_custom_fx(i, name);
                }
                if let Some(error) = self.persist_error.as_deref() {
                    ui.colored_label(pal::RED, "Save failed")
                        .on_hover_text(error);
                } else {
                    ui.colored_label(pal::GREEN, "✓ Saved automatically");
                }
            });
            ui.label(
                RichText::new("Build the sequence, preview it here, then test it on the keyboard.")
                    .size(11.5)
                    .color(pal::TEXT_DIM),
            );
            ui.add_space(10.0);

            if self.custom_fx[i].steps.is_empty() {
                self.custom_fx[i].steps.push(FxStep {
                    keys: Vec::new(),
                    color: [138, 92, 246],
                    ms: 220,
                });
                dirty = true;
            }
            let n_steps = self.custom_fx[i].steps.len();
            self.fx_step = self.fx_step.min(n_steps.saturating_sub(1));
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("STEPS").size(10.5).color(pal::TEXT_DIM));
                for step_i in 0..n_steps {
                    let step = &self.custom_fx[i].steps[step_i];
                    let label = format!(
                        "Step {} · {} keys · {} ms",
                        step_i + 1,
                        step.keys.len(),
                        step.ms
                    );
                    if ui.selectable_label(self.fx_step == step_i, label).clicked() {
                        self.fx_step = step_i;
                        self.fx_preview = None;
                        self.fx_delete_step_confirm = None;
                    }
                }
                if ui
                    .button("＋ Duplicate step")
                    .on_hover_text("Copy the selected step and insert it after this one")
                    .clicked()
                {
                    let copy = self.custom_fx[i].steps[self.fx_step].clone();
                    self.custom_fx[i].steps.insert(self.fx_step + 1, copy);
                    self.fx_step += 1;
                    self.fx_preview = None;
                    self.fx_delete_step_confirm = None;
                    dirty = true;
                }
            });
            ui.add_space(10.0);

            if ui.available_width() >= 700.0 {
                ui.columns(2, |cols| {
                    self.fx_step_controls(&mut cols[0], i, &mut dirty);
                    self.fx_playback_controls(&mut cols[1], i, &mut dirty);
                });
            } else {
                self.fx_step_controls(ui, i, &mut dirty);
                ui.add_space(8.0);
                self.fx_playback_controls(ui, i, &mut dirty);
            }

            if dirty {
                self.save_custom_fx();
            }
            ui.add_space(8.0);
            self.fx_delete_effect_row(
                ui,
                i,
                "Assign it from Layout → On press, or use it as keyboard RGB.",
            );
        });
    }

    fn fx_step_controls(&mut self, ui: &mut egui::Ui, i: usize, dirty: &mut bool) {
        let step_i = self.fx_step;
        let key_count = self.custom_fx[i].steps[step_i].keys.len();
        ui.label(
            RichText::new(format!("CURRENT STEP · {} KEYS", key_count))
                .size(10.5)
                .color(pal::TEXT_DIM),
        );
        fx_field(ui, "Color", |ui| {
            *dirty |= ui
                .color_edit_button_srgb(&mut self.custom_fx[i].steps[step_i].color)
                .changed();
        });
        fx_field(ui, "Duration", |ui| {
            *dirty |= ui
                .add(
                    egui::DragValue::new(&mut self.custom_fx[i].steps[step_i].ms)
                        .range(40..=2000)
                        .speed(10)
                        .suffix(" ms"),
                )
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Move keys").size(11.0).color(pal::TEXT_DIM));
            let mut mv = None;
            if ui.small_button("←").on_hover_text("Nudge left").clicked() {
                mv = Some((-1.0, 0.0));
            }
            if ui.small_button("↑").on_hover_text("Nudge up").clicked() {
                mv = Some((0.0, -1.0));
            }
            if ui.small_button("↓").on_hover_text("Nudge down").clicked() {
                mv = Some((0.0, 1.0));
            }
            if ui.small_button("→").on_hover_text("Nudge right").clicked() {
                mv = Some((1.0, 0.0));
            }
            if let Some((dx, dy)) = mv {
                self.shift_step(i, step_i, dx, dy);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Clear keys").clicked() {
                self.custom_fx[i].steps[step_i].keys.clear();
                self.fx_preview = None;
                *dirty = true;
            }
            if self.custom_fx[i].steps.len() > 1 {
                if self.fx_delete_step_confirm == Some((i, step_i)) {
                    ui.colored_label(pal::RED, "Delete this step?");
                    if ui.small_button("Keep").clicked() {
                        self.fx_delete_step_confirm = None;
                    }
                    if ui.small_button("Delete step").clicked() {
                        self.custom_fx[i].steps.remove(step_i);
                        self.fx_step = self.fx_step.saturating_sub(1);
                        self.fx_delete_step_confirm = None;
                        self.fx_preview = None;
                        *dirty = true;
                    }
                } else if ui.small_button("Delete step").clicked() {
                    self.fx_delete_step_confirm = Some((i, step_i));
                }
            }
        });
    }

    fn fx_playback_controls(&mut self, ui: &mut egui::Ui, i: usize, dirty: &mut bool) {
        ui.label(RichText::new("PLAYBACK").size(10.5).color(pal::TEXT_DIM));
        let speed_text = format!("{:.2}×", self.custom_fx[i].speed);
        fx_field(ui, "Tempo", |ui| {
            *dirty |= ui
                .add_sized(
                    [220.0, 20.0],
                    egui::Slider::new(&mut self.custom_fx[i].speed, 0.2..=3.0)
                        .show_value(false)
                        .text(speed_text),
                )
                .changed();
        });
        fx_field(ui, "Other LEDs", |ui| {
            ui.vertical(|ui| {
                *dirty |= ui
                    .radio_value(
                        &mut self.custom_fx[i].background,
                        config::FxBackgroundMode::Preserve,
                        "Keep current RGB",
                    )
                    .changed();
                *dirty |= ui
                    .radio_value(
                        &mut self.custom_fx[i].background,
                        config::FxBackgroundMode::Blackout,
                        "Turn other LEDs off",
                    )
                    .changed();
            });
        });

        ui.horizontal_wrapped(|ui| {
            let previewing = self.fx_preview.is_some_and(|(fx, _)| fx == i);
            let label = if previewing {
                "■ Stop preview"
            } else {
                "▶ Preview here"
            };
            if ui.button(label).clicked() {
                self.fx_preview = if previewing {
                    None
                } else {
                    Some((i, Instant::now()))
                };
            }
            if ui
                .add_enabled(
                    self.connected.is_some(),
                    egui::Button::new(RichText::new("⚡ Test on keyboard").color(Color32::WHITE))
                        .fill(pal::VIOLET),
                )
                .on_disabled_hover_text("Plug in the Voyager to test effects")
                .clicked()
            {
                self.queue_fx_test(FxTestSpec::Custom {
                    steps: self.custom_fx[i].steps.clone(),
                    name: self.custom_fx[i].name.clone(),
                    speed: self.custom_fx[i].speed,
                    replace_base: matches!(
                        self.custom_fx[i].background,
                        config::FxBackgroundMode::Blackout
                    ),
                });
            }
        });
    }

    fn fx_delete_effect_row(&mut self, ui: &mut egui::Ui, i: usize, helper: &str) {
        if self.fx_delete_confirm == Some(i) {
            let name = self.custom_fx[i].name.clone();
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(pal::RED, format!("Delete “{name}”?"));
                if ui.button("Keep").clicked() {
                    self.fx_delete_confirm = None;
                }
                if ui.button("Delete permanently").clicked() {
                    self.remove_custom_fx(i);
                    self.fx_preview = None;
                    self.fx_delete_confirm = None;
                    self.fx_delete_step_confirm = None;
                    if self.custom_fx.is_empty() {
                        self.fx_sel = FxSel::Press(PressEffect::Ripple);
                        self.fx_last_custom = 0;
                    } else {
                        let next = i.min(self.custom_fx.len() - 1);
                        self.fx_sel = FxSel::Custom(next);
                        self.fx_last_custom = next;
                        self.fx_step = 0;
                    }
                }
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.weak(helper);
                if ui.button("Delete effect…").clicked() {
                    self.fx_delete_confirm = Some(i);
                }
            });
        }
    }

    fn fx_step_keyboard_card(&mut self, ui: &mut egui::Ui) {
        let FxSel::Custom(i) = self.fx_sel else {
            return;
        };
        let Some(custom) = self.custom_fx.get(i) else {
            return;
        };
        if custom.preset.is_some() || custom.steps.is_empty() {
            return;
        }
        let preview_started = self
            .fx_preview
            .and_then(|(fx, started)| (fx == i).then_some(started));
        let previewing = preview_started.is_some();
        let step_i = preview_started
            .map(|started| fx_preview_step(custom, started.elapsed()))
            .unwrap_or_else(|| self.fx_step.min(custom.steps.len() - 1));
        let step = custom.steps[step_i].clone();
        let title = if previewing {
            format!("Preview · Step {} of {}", step_i + 1, custom.steps.len())
        } else {
            format!("Keys in step {}", step_i + 1)
        };
        if previewing {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        card(ui, &title, |ui| {
            ui.label(
                RichText::new(if previewing {
                    "Live in-app preview · stop it above to edit the selected keys."
                } else {
                    "Click keys to include or remove them from this step."
                })
                .size(11.5)
                .color(pal::TEXT_DIM),
            );
            ui.add_space(6.0);
            let n = self.geometry().len();
            let mut glow = vec![None; n];
            for key in &step.keys {
                if let Some(slot) = glow.get_mut(*key as usize) {
                    *slot = Some(Color32::from_rgb(
                        step.color[0],
                        step.color[1],
                        step.color[2],
                    ));
                }
            }
            let no_press = vec![false; n];
            let layer = self.device_layer(self.view_layer);
            let combo = self.combo_member_mask(self.view_layer);
            let kb = draw_keyboard(
                ui,
                self.geometry(),
                layer.as_ref(),
                &glow,
                &no_press,
                None,
                Some(&combo),
                None,
                1.0,
                false,
            );
            if !previewing {
                if let Some(key) = kb.clicked {
                    if let Some(step) = self
                        .custom_fx
                        .get_mut(i)
                        .and_then(|custom| custom.steps.get_mut(step_i))
                    {
                        match step.keys.iter().position(|&k| k as usize == key) {
                            Some(pos) => {
                                step.keys.remove(pos);
                            }
                            None => step.keys.push(key as u16),
                        }
                        self.save_custom_fx();
                    }
                }
            }
        });
    }

    fn queue_fx_test(&mut self, spec: FxTestSpec) {
        if self.connected.is_none() {
            return;
        }
        let restore = if let Some(previous) = self.fx_test_run.take() {
            let restore = previous.restore;
            if let Ok(mut anim) = self.anim.lock() {
                restore.clone().restore(&mut anim);
            }
            restore
        } else {
            let Ok(anim) = self.anim.lock() else {
                return;
            };
            FxBoardRestore::capture(&anim)
        };
        let now = Instant::now();
        let starts_at = now + Duration::from_millis(FX_TEST_LEAD_MS);
        let active_ms = fx_test_active_ms(&spec);
        let effect_ends_at = starts_at + Duration::from_millis(active_ms);
        let restore_at = effect_ends_at + Duration::from_millis(FX_TEST_TAIL_MS);
        self.fx_test_run = Some(FxTestRun {
            restore,
            spec,
            starts_at,
            effect_ends_at,
            restore_at,
            started: false,
            effect_ended: false,
        });
    }

    pub(super) fn tick_fx_test(&mut self, ctx: &egui::Context) {
        let test_key = 16.min(self.geometry().len().saturating_sub(1));
        let Some(run) = self.fx_test_run.as_mut() else {
            return;
        };
        let now = Instant::now();
        if !run.started && now >= run.starts_at {
            if let Ok(mut anim) = self.anim.lock() {
                match &run.spec {
                    FxTestSpec::Board {
                        effect,
                        color,
                        speed,
                        brightness,
                    } => {
                        anim.effect = *effect;
                        anim.color = *color;
                        anim.speed = *speed;
                        anim.brightness = *brightness;
                    }
                    FxTestSpec::Press { effect, color } => {
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos() as u64)
                            .unwrap_or(0);
                        anim.events.push(FxEvent {
                            key: test_key,
                            effect: *effect,
                            color: *color,
                            at: now,
                            seed,
                            seq: None,
                            replace_base: false,
                        });
                    }
                    FxTestSpec::Custom {
                        steps,
                        name,
                        speed,
                        replace_base,
                    } => {
                        anim.custom = steps.clone();
                        anim.custom_name = name.clone();
                        anim.custom_replace_base = *replace_base;
                        anim.speed = *speed;
                        anim.effect = Effect::Custom;
                    }
                }
            }
            run.started = true;
        }

        if run.started && !run.effect_ended && now >= run.effect_ends_at {
            if !matches!(run.spec, FxTestSpec::Press { .. }) {
                if let Ok(mut anim) = self.anim.lock() {
                    // Short neutral tail before the user's previous RGB returns.
                    anim.effect = Effect::Off;
                }
            }
            run.effect_ended = true;
        }

        if now >= run.restore_at {
            let run = self
                .fx_test_run
                .take()
                .expect("FX test disappeared while ticking");
            if let Ok(mut anim) = self.anim.lock() {
                run.restore.restore(&mut anim);
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn fx_test_status(&self) -> Option<String> {
        let run = self.fx_test_run.as_ref()?;
        Some(if !run.started {
            "FX test · starting…".to_string()
        } else if !run.effect_ended {
            "FX test · running on keyboard".to_string()
        } else {
            "FX test · finishing…".to_string()
        })
    }
}

fn fx_field(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(10.5).color(pal::TEXT_DIM));
        ui.add_space(2.0);
        body(ui);
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod fx_ui_tests {
    use super::*;

    fn preview_sequence(speed: f32) -> CustomFx {
        CustomFx {
            name: "preview".to_string(),
            steps: vec![
                FxStep {
                    keys: vec![0],
                    color: [255, 0, 0],
                    ms: 100,
                },
                FxStep {
                    keys: vec![1],
                    color: [0, 255, 0],
                    ms: 200,
                },
                FxStep {
                    keys: vec![2],
                    color: [0, 0, 255],
                    ms: 300,
                },
            ],
            speed,
            background: config::FxBackgroundMode::Preserve,
            preset: None,
        }
    }

    #[test]
    fn press_tests_are_shorter_than_board_tests() {
        let press = FxTestSpec::Press {
            effect: PressEffect::Ripple,
            color: [1, 2, 3],
        };
        let board = FxTestSpec::Board {
            effect: Effect::Rainbow,
            color: [1, 2, 3],
            speed: 1.0,
            brightness: 0.8,
        };
        assert!(fx_test_active_ms(&press) < fx_test_active_ms(&board));
    }

    #[test]
    fn preview_uses_step_durations_and_loops() {
        let sequence = preview_sequence(1.0);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(0)), 0);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(99)), 0);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(100)), 1);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(299)), 1);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(300)), 2);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(599)), 2);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(600)), 0);
    }

    #[test]
    fn preview_applies_sequence_tempo() {
        let sequence = preview_sequence(2.0);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(49)), 0);
        assert_eq!(fx_preview_step(&sequence, Duration::from_millis(50)), 1);
    }
}
