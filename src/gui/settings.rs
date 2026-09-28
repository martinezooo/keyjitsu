//! Settings/tools pages: application settings, performance, RGB, guard and autolayer.

use super::*;

impl App {
    pub(super) fn ui_tools(&mut self, ui: &mut egui::Ui) {
        centered_page(ui, 1000.0, |ui| {
            page_header(
                ui,
                "Settings",
                "Firmware, guard, profiles and app housekeeping.",
            );

            let fw_pill = if self.env.is_ready() {
                ("Ready".to_string(), pal::GREEN)
            } else {
                ("Setup required".to_string(), pal::AMBER)
            };
            tool_card(
                ui,
                "⚙",
                "Firmware build",
                "Remap keys and compile firmware 100% locally with QMK - no login, no cloud.",
                Some(fw_pill),
                self,
                |ui, app| app.ui_localbuild(ui),
            );

            #[cfg(target_os = "macos")]
            {
                let guard_pill = if self.guard.is_some() {
                    ("Active".to_string(), pal::GREEN)
                } else if self.guard_enabled {
                    ("Waiting for keyboard".to_string(), pal::AMBER)
                } else {
                    ("Off".to_string(), pal::TEXT_DIM)
                };
                tool_card(
                    ui,
                    "🔒",
                    "Keyboard guard",
                    "Disables the Mac's built-in keyboard while the ZSA board is connected.",
                    Some(guard_pill),
                    self,
                    |ui, app| app.ui_guard(ui),
                );
            }

            #[cfg(target_os = "macos")]
            let app_pill = if crate::platform::autostart_enabled() {
                ("Autostart on".to_string(), pal::GREEN)
            } else {
                ("Manual start".to_string(), pal::TEXT_DIM)
            };
            #[cfg(not(target_os = "macos"))]
            let app_pill = ("Updates".to_string(), pal::TEXT_DIM);
            let app_desc = if cfg!(target_os = "macos") {
                "Startup and update settings."
            } else {
                "Update settings."
            };
            tool_card(
                ui,
                "🚀",
                "App",
                app_desc,
                Some(app_pill),
                self,
                |ui, app| app.ui_app_card(ui),
            );

            tool_card(
                ui,
                "📚",
                "Shortcut library",
                "Common shortcuts to borrow when planning layers. Edit keys in Live.",
                Some(("Reference".to_string(), pal::TEXT_DIM)),
                self,
                |ui, app| {
                    egui::CollapsingHeader::new("Browse the library")
                        .id_salt("shortcut_library")
                        .default_open(false)
                        .show(ui, |ui| app.ui_shortcuts(ui));
                },
            );
            ui.add_space(10.0);
        });
    }

    /// Performance as its own page (same card style as Settings).
    pub(super) fn ui_perf_page(&mut self, ui: &mut egui::Ui) {
        if !perf::CPU_SUPPORTED {
            ui.add_space(20.0);
            ui.label("Process CPU sampling is not available on this platform.");
            return;
        }
        centered_page(ui, 1000.0, |ui| {
            let pill = {
                let c = self.perf_live;
                (
                    format!("{c:.1}% CPU"),
                    if c > 25.0 { pal::AMBER } else { pal::GREEN },
                )
            };
            tool_card(
                ui,
                "📈",
                "Performance",
                "keyjitsu samples its own CPU and tags each sample with what it was doing.",
                Some(pill),
                self,
                |ui, app| app.ui_performance(ui),
            );
        });
    }

    /// Autolayer as its own page.
    pub(super) fn ui_auto_page(&mut self, ui: &mut egui::Ui) {
        centered_page(ui, 1000.0, |ui| {
            let pill = if !cfg!(target_os = "macos") {
                ("macOS only".to_string(), pal::TEXT_DIM)
            } else if self.autolayer_enabled {
                (
                    format!(
                        "On · {} rule{}",
                        self.rules.len(),
                        if self.rules.len() == 1 { "" } else { "s" }
                    ),
                    pal::GREEN,
                )
            } else {
                ("Off".to_string(), pal::TEXT_DIM)
            };
            tool_card(
                ui,
                "⇆",
                "Autolayer",
                "Switches layers automatically based on the frontmost app.",
                Some(pill),
                self,
                |ui, app| app.ui_autolayer(ui),
            );
        });
    }

    pub(super) fn ui_app_card(&mut self, ui: &mut egui::Ui) {
        #[cfg(target_os = "macos")]
        {
            let mut on = crate::platform::autostart_enabled();
            if toggle_row(ui, "Start keyjitsu at login (GUI)", &mut on) {
                self.autostart_error = crate::platform::set_autostart(on)
                    .err()
                    .map(|e| format!("{e:#}"));
            }
            if let Some(e) = &self.autostart_error {
                ui.colored_label(pal::RED, format!("autostart failed: {e}"));
            }
            if let Some(p) = crate::platform::autostart_location() {
                ui.label(
                    RichText::new(format!("LaunchAgent: {}", p.display()))
                        .size(11.0)
                        .color(pal::TEXT_DIM),
                );
            }
            ui.label(
                RichText::new("Points at this binary - re-toggle after moving/rebuilding the app to refresh the path.")
                    .size(11.0)
                    .color(pal::TEXT_DIM),
            );
        }
        #[cfg(not(target_os = "macos"))]
        ui.label(
            RichText::new("Start at login is not implemented on this platform yet.")
                .size(11.0)
                .color(pal::TEXT_DIM),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("Updates").strong().color(pal::TEXT));
        ui.label(
            RichText::new(format!(
                "You are on v{}. Checking asks GitHub for the latest release tag. Nothing is downloaded or installed.",
                env!("CARGO_PKG_VERSION")
            ))
            .size(11.5)
            .color(pal::TEXT_DIM),
        );
        if toggle_row(
            ui,
            "Check for updates when keyjitsu starts",
            &mut self.auto_update_check,
        ) {
            let skip = !self.auto_update_check;
            self.persist_config("saving update preference", move |cfg| {
                cfg.skip_update_check_on_start = skip;
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.update_rx.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("Check for updates"))
                .clicked()
            {
                self.update_rx = Some(spawn_update_check());
                self.update_state = None;
            }
            if busy {
                ui.spinner();
                ui.label(RichText::new("checking…").size(11.5).color(pal::TEXT_DIM));
            }
        });
        match &self.update_state {
            Some(UpdateCheck::UpToDate) => {
                ui.colored_label(pal::GREEN, "You are up to date.");
            }
            Some(UpdateCheck::Available { tag, url }) => {
                let (tag, url) = (tag.clone(), url.clone());
                ui.colored_label(pal::AMBER, format!("{tag} is available."));
                ui.horizontal(|ui| {
                    if ui.button("Open release page").clicked() {
                        if let Err(e) = crate::platform::open_url(&url) {
                            self.update_state = Some(UpdateCheck::Error(format!(
                                "could not open release page: {e:#}"
                            )));
                        }
                    }
                    #[cfg(target_os = "macos")]
                    {
                        ui.label(
                            RichText::new("then rebuild: ")
                                .size(11.5)
                                .color(pal::TEXT_DIM),
                        );
                        ui.code("scripts/bundle.sh --install");
                    }
                });
            }
            Some(UpdateCheck::Error(e)) => {
                ui.colored_label(pal::RED, format!("could not check: {e}"));
            }
            None => {}
        }
    }

    pub(super) fn ui_performance(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let c = self.perf_live;
            let col = if c > 25.0 { pal::AMBER } else { pal::GREEN };
            ui.label("app CPU:");
            ui.label(
                RichText::new(format!("{c:.1}%"))
                    .strong()
                    .size(16.0)
                    .color(col),
            );
            ui.weak("keyjitsu only · % of one core");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("state: {}", self.perf_state().label()));
            });
        });
        ui.add_space(4.0);
        if toggle_row(
            ui,
            "Show CPU in the header (always visible)",
            &mut self.show_cpu_header,
        ) {
            let show = self.show_cpu_header;
            self.persist_config("saving CPU display preference", move |cfg| {
                cfg.show_cpu_header = show;
            });
        }
        ui.add_space(6.0);

        if let Some(run) = &self.perf_run {
            let now = Instant::now();
            let (phase, remaining) = if run.phases.is_empty() {
                (
                    "5-min sample".to_string(),
                    run.end_at.saturating_duration_since(now),
                )
            } else {
                (
                    format!("compare · {}", run.phases[run.phase_i].label),
                    run.phase_until.saturating_duration_since(now),
                )
            };
            let n = run.samples.len();
            let mut stop = false;
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("sampling - {phase}")).color(pal::VIOLET_HI));
                ui.weak(format!("{}s left · {n} samples", remaining.as_secs()));
                stop = ui.button("stop").clicked();
            });
            if stop {
                self.finish_perf();
            }
        } else {
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("▶ Start 5-min sample").color(Color32::WHITE)).fill(pal::VIOLET)).clicked() {
                    self.start_perf_observe();
                }
                if ui
                    .button("⚖ Compare modes")
                    .on_hover_text("cycles idle → layout RGB → rainbow → rainbow+peek (~12s each) and measures CPU in each")
                    .clicked()
                {
                    self.start_perf_compare();
                }
            });
        }

        if let Some(sum) = &self.perf_last {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new("Last results").strong());
            ui.weak(format!("{} samples over {}s", sum.n, sum.secs));
            ui.horizontal(|ui| {
                ui.label(format!("overall avg {:.1}%", sum.avg));
                ui.separator();
                ui.label(format!("peak {:.1}%", sum.max));
            });
            ui.add_space(6.0);
            let scale = sum.modes.iter().map(|m| m.max).fold(1.0f32, f32::max);
            egui::Grid::new("perf_modes")
                .num_columns(3)
                .spacing([14.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("mode");
                    ui.strong("avg");
                    ui.strong("peak");
                    ui.end_row();
                    for m in &sum.modes {
                        ui.label(&m.label).on_hover_text(format!("{} samples", m.n));
                        // avg as a small bar + number
                        ui.horizontal(|ui| {
                            perf_bar(ui, m.avg / scale, pal::VIOLET);
                            ui.label(format!("{:.1}%", m.avg));
                        });
                        ui.horizontal(|ui| {
                            perf_bar(ui, m.max / scale, pal::AMBER);
                            ui.label(format!("{:.1}%", m.max));
                        });
                        ui.end_row();
                    }
                });
        }
    }

    pub(super) fn ui_rgb_effects(&mut self, ui: &mut egui::Ui) {
        if self.connected.is_none() {
            ui.weak("connect the keyboard to use effects");
            return;
        }

        let Ok(mut a) = self.anim.lock() else { return };
        let wide = 220.0;
        let before = (
            a.effect,
            a.color,
            a.speed,
            a.brightness,
            a.press_effect,
            a.press_color,
            a.custom_name.clone(),
        );

        labeled(ui, "constant effect", |ui| {
            let sel_text = if a.effect == Effect::Custom {
                format!("★ {}", a.custom_name)
            } else {
                a.effect.label().to_string()
            };
            egui::ComboBox::from_id_salt("rgbfx")
                .width(wide)
                .selected_text(sel_text)
                .show_ui(ui, |ui| {
                    for (e, label) in Effect::ALL {
                        ui.selectable_value(&mut a.effect, e, label);
                    }
                    if !self.custom_fx.is_empty() {
                        ui.separator();
                    }
                    for c in &self.custom_fx {
                        let is = a.effect == Effect::Custom && a.custom_name == c.name;
                        if ui.selectable_label(is, format!("★ {}", c.name)).clicked() {
                            a.effect = Effect::Custom;
                            a.custom = c.steps.clone();
                            a.custom_name = c.name.clone();
                        }
                    }
                });
        });
        if a.effect.uses_color() {
            labeled(ui, "effect color", |ui| {
                ui.color_edit_button_srgb(&mut a.color);
            });
        }
        if a.effect != Effect::Off {
            labeled(ui, "speed", |ui| {
                ui.add_sized(
                    [wide, 20.0],
                    egui::Slider::new(&mut a.speed, 0.2..=3.0).show_value(false),
                );
            });
        }
        labeled(ui, "brightness", |ui| {
            ui.add_sized(
                [wide, 20.0],
                egui::Slider::new(&mut a.brightness, 0.05..=1.0).show_value(false),
            );
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new("On key press").strong());
        ui.weak("Instead of a static color, play an effect from the key you press.");
        ui.add_space(2.0);
        labeled(ui, "press effect", |ui| {
            egui::ComboBox::from_id_salt("pressfx")
                .width(wide)
                .selected_text(a.press_effect.label())
                .show_ui(ui, |ui| {
                    for (e, label) in rgb_anim::PressEffect::ALL {
                        ui.selectable_value(&mut a.press_effect, e, label);
                    }
                });
        });
        if a.press_effect != rgb_anim::PressEffect::None {
            labeled(ui, "press color", |ui| {
                ui.color_edit_button_srgb(&mut a.press_color);
            });
        }

        let owns_leds = a.effect != Effect::Off;
        let after = (
            a.effect,
            a.color,
            a.speed,
            a.brightness,
            a.press_effect,
            a.press_color,
            a.custom_name.clone(),
        );
        let rgb = (after != before).then(|| config::RgbState {
            effect: a.effect,
            color: a.color,
            speed: a.speed,
            brightness: a.brightness,
            press_effect: a.press_effect,
            press_color: a.press_color,
            custom_name: a.custom_name.clone(),
        });
        drop(a);

        if owns_leds {
            self.sync_glow = false;
        }
        if let Some(rgb) = rgb {
            self.persist_config("saving RGB settings", move |cfg| {
                cfg.rgb = rgb;
            });
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn clear_guard_verification(&mut self) {
        self.guard_test_rx = None;
        self.guard_test_result = None;
        self.guard_test_started = None;
    }

    #[cfg(target_os = "macos")]
    pub(super) fn set_guard_enabled(&mut self, enabled: bool) {
        if self.guard_enabled == enabled {
            return;
        }
        self.guard_enabled = enabled;
        self.guard_error = None;
        self.clear_guard_verification();
        if !enabled {
            // Drop immediately so the built-in keyboard is restored on the
            // same click instead of waiting for the next reconciliation tick.
            self.guard = None;
            self.guard_hidutil_ok = false;
        }
        self.persist_config("saving keyboard guard preference", move |cfg| {
            cfg.guard_enabled = enabled;
        });
    }

    #[cfg(target_os = "macos")]
    pub(super) fn guard_indicator(&self) -> (String, Color32, String) {
        use crate::macos_guard_test::GuardTestOutcome;

        if !self.guard_enabled {
            return (
                "guard off".into(),
                pal::TEXT_DIM,
                "Built-in keyboard guard is off.".into(),
            );
        }
        if let Some(e) = &self.guard_error {
            return ("⚠ guard".into(), pal::RED, format!("Guard failed: {e}"));
        }
        if self.guard.is_none() {
            return if self.connected.is_some() {
                (
                    "guard starting".into(),
                    pal::AMBER,
                    "Guard is enabled and waiting for the remap to engage.".into(),
                )
            } else {
                (
                    "guard armed".into(),
                    pal::TEXT_DIM,
                    "Guard is enabled and will engage when the Voyager connects.".into(),
                )
            };
        }
        if !self.guard_hidutil_ok {
            return (
                "⚠ guard".into(),
                pal::RED,
                "hidutil no longer reports the remap as applied.".into(),
            );
        }

        match self.guard_test_result {
            Some(GuardTestOutcome::Blocked) => (
                "🔒 guard verified".into(),
                pal::GREEN,
                "Functional test confirmed that no key press reached macOS during the test window.".into(),
            ),
            Some(GuardTestOutcome::Leaked) => (
                "⚠ guard leaks".into(),
                pal::RED,
                "A functional test detected a key press getting through the built-in keyboard guard.".into(),
            ),
            Some(GuardTestOutcome::PermissionNeeded) => (
                "guard unverified".into(),
                pal::AMBER,
                "The remap is applied, but Input Monitoring permission is needed for a functional test.".into(),
            ),
            None => (
                "guard unverified".into(),
                pal::AMBER,
                "hidutil reports the remap as applied, but that does not prove the built-in keyboard is blocked. Run Test the guard in Settings.".into(),
            ),
        }
    }

    pub(super) fn ui_guard(&mut self, ui: &mut egui::Ui) {
        #[cfg(target_os = "macos")]
        {
            let mut enabled = self.guard_enabled;
            if toggle_row(
                ui,
                "Disable built-in keyboard while connected",
                &mut enabled,
            ) {
                self.set_guard_enabled(enabled);
            }
            let (label, color, hover) = self.guard_indicator();
            ui.colored_label(color, label).on_hover_text(hover);
            if let Some(g) = &self.guard {
                ui.weak(format!("target: {}", g.describe()));
            }
            if let Some(e) = &self.guard_error {
                ui.colored_label(pal::RED, e);
            }
            if self.guard.is_some() {
                self.ui_guard_test(ui);
            }
            egui::CollapsingHeader::new("Manual recovery").show(ui, |ui| {
                ui.weak("Keys are remapped to no-ops with hidutil (no special permission). They are restored on toggle-off, disconnect, quit, and by any reboot. If keyjitsu is force-killed first, restore by hand:");
                let mut cmd = crate::macos_kb::restore_command();
                ui.add(
                    egui::TextEdit::singleline(&mut cmd)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
            });
        }
        #[cfg(not(target_os = "macos"))]
        ui.weak("(macOS only)");
    }

    /// The functional self-test: hidutil can report its remap as fully
    /// applied while the built-in keyboard still leaks key presses through a
    /// lower HID layer hidutil cannot reach (confirmed on real hardware).
    /// This listens system-wide for a moment to give a real yes/no answer.
    #[cfg(target_os = "macos")]
    pub(super) fn ui_guard_test(&mut self, ui: &mut egui::Ui) {
        use crate::macos_guard_test::GuardTestOutcome;
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(
            RichText::new("Does it actually work?")
                .strong()
                .size(12.5)
                .color(pal::TEXT),
        );
        ui.label(
            RichText::new("hidutil can report the remap as applied while the built-in keyboard still leaks presses through a lower HID layer hidutil cannot reach. This listens for a moment to give a real answer.")
                .size(11.0)
                .color(pal::TEXT_DIM),
        );
        ui.add_space(4.0);
        if self.guard_test_rx.is_some() {
            let left = self
                .guard_test_started
                .map(|t| 6u64.saturating_sub(t.elapsed().as_secs()))
                .unwrap_or(0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.colored_label(pal::AMBER, format!("Press any key on the MacBook's OWN keyboard now ({left}s)... not the Voyager."));
            });
            return;
        }
        match self.guard_test_result {
            Some(GuardTestOutcome::Blocked) => {
                ui.colored_label(
                    pal::GREEN,
                    "✓ Confirmed: no key reached the system during the test.",
                );
                if ui.button("Test again").clicked() {
                    self.start_guard_test();
                }
            }
            Some(GuardTestOutcome::Leaked) => {
                ui.colored_label(
                    pal::RED,
                    "⚠ A key press got through - the built-in keyboard is NOT fully blocked.",
                );
                ui.label(
                    RichText::new("Known limitation on some Macs: hidutil's remap doesn't reach every layer the built-in keyboard uses. Don't rely on the guard alone - keep the Voyager clear of accidental presses.")
                        .size(11.0)
                        .color(pal::TEXT_MUTED),
                );
                if ui.button("Test again").clicked() {
                    self.start_guard_test();
                }
            }
            Some(GuardTestOutcome::PermissionNeeded) => {
                ui.colored_label(pal::AMBER, "Needs the Input Monitoring permission to test.");
                ui.horizontal(|ui| {
                    if ui.button("Open Input Monitoring settings").clicked() {
                        if let Err(e) = crate::platform::open_url(
                            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
                        ) {
                            self.guard_error =
                                Some(format!("could not open Input Monitoring settings: {e:#}"));
                        }
                    }
                    if ui.button("Test again").clicked() {
                        self.start_guard_test();
                    }
                });
                ui.label(RichText::new("Enable keyjitsu there, then quit and reopen keyjitsu before testing again.").size(11.0).color(pal::TEXT_MUTED));
            }
            None => {
                if ui
                    .add(
                        egui::Button::new(RichText::new("⚡ Test the guard").color(Color32::WHITE))
                            .fill(pal::VIOLET),
                    )
                    .clicked()
                {
                    self.start_guard_test();
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn start_guard_test(&mut self) {
        self.guard_test_result = None;
        self.guard_test_started = Some(Instant::now());
        self.guard_test_rx = Some(crate::macos_guard_test::spawn_test(Duration::from_secs(6)));
    }

    pub(super) fn ui_autolayer(&mut self, ui: &mut egui::Ui) {
        #[cfg(target_os = "macos")]
        {
            if toggle_row(ui, "Enable autolayer", &mut self.autolayer_enabled) {
                let enabled = self.autolayer_enabled;
                self.persist_config("saving autolayer preference", move |cfg| {
                    cfg.autolayer_enabled = enabled;
                });
            }
            // Live feedback: what the frontmost app is and whether a rule hits.
            if self.autolayer_enabled {
                if let Some(front) = crate::cmd_autolayer::frontmost_bundle_id() {
                    let hit = self
                        .rules
                        .iter()
                        .find(|r| crate::cmd_autolayer::rule_matches(&front, &r.bundle));
                    ui.horizontal(|ui| {
                        ui.weak("frontmost:");
                        ui.label(
                            RichText::new(&front)
                                .size(11.5)
                                .monospace()
                                .color(pal::TEXT_MUTED),
                        );
                        match hit {
                            Some(r) => ui.colored_label(
                                pal::GREEN,
                                format!("→ {}", self.layer_name(r.layer)),
                            ),
                            None => ui.weak("→ base"),
                        };
                    });
                }
            }
            ui.add_space(4.0);

            let names: Vec<String> = (0..self.layer_count())
                .map(|n| self.layer_name(n))
                .collect();
            let mut remove: Option<usize> = None;
            if self.rules.is_empty() {
                ui.label(
                    RichText::new("No rules yet. Add one from a running app below.")
                        .size(12.0)
                        .color(pal::TEXT_DIM),
                );
                ui.add_space(2.0);
            }
            egui::Grid::new("rules")
                .num_columns(3)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    if !self.rules.is_empty() {
                        ui.strong("app (bundle id contains)");
                        ui.strong("switch to layer");
                        ui.strong("");
                        ui.end_row();
                    }
                    for (i, rule) in self.rules.iter_mut().enumerate() {
                        if ui
                            .add(egui::TextEdit::singleline(&mut rule.bundle).desired_width(240.0))
                            .changed()
                        {
                            self.rules_dirty = true;
                        }
                        egui::ComboBox::from_id_salt(("rule_layer", i))
                            .selected_text(
                                names
                                    .get(rule.layer as usize)
                                    .cloned()
                                    .unwrap_or_else(|| format!("Layer {}", rule.layer)),
                            )
                            .show_ui(ui, |ui| {
                                for (n, nm) in names.iter().enumerate() {
                                    if ui
                                        .selectable_value(
                                            &mut rule.layer,
                                            n as u8,
                                            format!("{n} · {nm}"),
                                        )
                                        .changed()
                                    {
                                        self.rules_dirty = true;
                                    }
                                }
                            });
                        if ui.button("✕").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
            if let Some(i) = remove {
                self.rules.remove(i);
                self.rules_dirty = true;
            }
            ui.add_space(6.0);

            // Add a rule from a running app (no need to know bundle ids).
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("add_running")
                    .selected_text("＋ Add from running app…")
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (name, bundle) in crate::cmd_autolayer::running_apps() {
                            if ui
                                .selectable_label(false, format!("{name}  ·  {bundle}"))
                                .clicked()
                            {
                                self.rules.push(AutolayerRule { bundle, layer: 1 });
                                self.rules_dirty = true;
                            }
                        }
                    });
                if ui.button("＋ blank rule").clicked() {
                    self.rules.push(AutolayerRule {
                        bundle: String::new(),
                        layer: 1,
                    });
                    self.rules_dirty = true;
                }
            });

            if self.rules_dirty {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Save & apply").color(Color32::WHITE))
                                .fill(pal::VIOLET),
                        )
                        .clicked()
                    {
                        let rules = self.rules.clone();
                        if self.persist_config("saving autolayer rules", move |cfg| {
                            cfg.autolayer_rules = rules;
                        }) {
                            self.rules_dirty = false;
                            self.autolayer = None;
                        }
                    }
                    status_pill(ui, "unsaved rules", pal::AMBER);
                });
            }
        }
        #[cfg(not(target_os = "macos"))]
        ui.weak("(macOS only)");
    }
}
