//! Event ingestion and background-job reconciliation.
//!
//! Device, build, flash and update events are reduced here before rendering.
//! Keeping this out of page code makes event ordering and reconnect behavior
//! testable without mixing it with egui layout.

use super::*;

impl App {
    pub(super) fn drain_events(&mut self) {
        let key_count = self.geometry().len();
        while let Ok(ev) = self.erx.try_recv() {
            match ev {
                DevEvent::Connected {
                    model,
                    serial,
                    generation,
                } => {
                    self.connection_generation = Some(generation);
                    self.layout_error = None;
                    if let Ok(id) = LayoutId::from_serial(&serial) {
                        self.hydrate_heatmap(&id.hash, key_count);
                        self.hydrate_glow(&id.hash);
                        self.hydrate_key_fx(&id.hash);

                        let state_marker = firmware_state::state_id_from_serial(&serial);
                        self.firmware_state = match state_marker {
                            Some(marker) => FirmwareState::load(marker),
                            None => FirmwareState::load_legacy_for_serial(&serial),
                        }
                        .filter(|state| {
                            state.layout_hash == id.hash && state.revision == id.revision
                        });
                        self.confirm_expected_firmware(generation, state_marker);

                        self.layout = crate::oryx_api::cached_layout(&id, "voyager");

                        self.hydrate_custom_layers(&id.hash);
                        self.hydrate_staged(&id.hash);
                        self.drop_applied_from_staged();

                        let last_layout = serial.clone();
                        self.persist_config("remembering the connected layout", move |cfg| {
                            if cfg.last_layout.as_deref() != Some(last_layout.as_str()) {
                                cfg.last_layout = Some(last_layout);
                            }
                        });
                    } else {
                        self.layout = None;
                        self.firmware_state = None;
                    }
                    self.connected = Some((model, serial));
                    self.edit_synced = None;
                    self.push_anim_base();
                }
                DevEvent::LayoutLoaded { generation, layout } => {
                    if !layout_event_is_current(self.connection_generation, generation) {
                        continue;
                    }
                    self.layout_error = None;
                    self.layout = Some(*layout);
                    if self.firmware_state.is_some() {
                        self.rebuild_synth_layers();
                    } else if !matches!(
                        self.device_state_kind(),
                        DeviceStateKind::MissingFirmwareState
                            | DeviceStateKind::UnknownDeviceIdentity
                    ) {
                        if let Some(hash) = self.layout_hash.clone() {
                            self.hydrate_custom_layers(&hash);
                        }
                    }
                    self.edit_synced = None;
                    self.push_anim_base();
                }
                DevEvent::LayoutFailed { generation, error } => {
                    if !layout_event_is_current(self.connection_generation, generation) {
                        continue;
                    }
                    self.layout = None;
                    self.layout_error = Some(error);
                    self.edit_synced = None;
                }
                DevEvent::Disconnected { generation } => {
                    if !disconnect_event_is_current(self.connection_generation, generation) {
                        continue;
                    }
                    self.connected = None;
                    self.connection_generation = None;
                    self.pressed.iter_mut().for_each(|p| *p = false);
                    self.combo_down.clear();
                    self.peek_until = None;
                    let heat_save_error = self
                        .heat
                        .as_mut()
                        .and_then(|heat| heat.save().err())
                        .map(|e| format!("saving heatmap: {e:#}"));
                    if let Some(e) = heat_save_error {
                        self.persist_error = Some(e);
                    }
                }
                DevEvent::Hid(Event::Layer(n)) => {
                    let changed = self.active_layer != n;
                    self.active_layer = n;
                    if self.follow {
                        self.view_layer = n;
                    }
                    if self.sync_glow {
                        self.needs_push = true; // physical board now shows another layer
                    }
                    if changed {
                        self.maybe_peek(n);
                        self.push_anim_base();
                    }
                }
                DevEvent::Hid(Event::KeyDown { col, row }) => {
                    // Peek shortcut: bind mode collects the whole combo (all
                    // keys held before the first release); otherwise holding
                    // the full chord shows the minimap.
                    if self.binding_overlay {
                        if !self.binding_draft.contains(&[row, col]) {
                            self.binding_draft.push([row, col]);
                        }
                    } else if !self.overlay_chord.is_empty()
                        && self.overlay_chord.contains(&[row, col])
                    {
                        let all_down = self.overlay_chord.iter().all(|&[r, c]| {
                            (r == row && c == col)
                                || self
                                    .geometry()
                                    .key_index(r, c)
                                    .is_some_and(|k| self.pressed.get(k).copied().unwrap_or(false))
                        });
                        if all_down {
                            self.peek_layer = self.active_layer;
                            self.peek_until = Some(Instant::now() + Duration::from_secs(3600));
                        }
                    }
                    if let Some(idx) = self.geometry().key_index(row, col) {
                        self.pressed[idx] = true;
                        self.combo_down.insert(idx, Instant::now());
                        // Keystroke HUD: surface the minimap the moment a key
                        // goes down (independent of only-non-base).
                        if self.peek.show_combo && self.peek.enabled {
                            self.peek_layer = self.active_layer;
                            self.peek_until = Some(Instant::now() + Duration::from_millis(1600));
                        }
                        let heat_save_error = if let Some(heat) = &mut self.heat {
                            heat.record(self.active_layer, idx, key_count);
                            heat.autosave().err()
                        } else {
                            None
                        };
                        if let Some(e) = heat_save_error {
                            self.persist_error = Some(format!("saving heatmap: {e:#}"));
                        }
                        // Keep the Assign picker bound to the key it opened for.
                        if !self.picker_open && self.selected_key != Some(idx) {
                            self.selected_key = Some(idx);
                            self.edit_color = self.current_key_srgb(self.view_layer, idx);
                            self.sync_editor_from_key(self.view_layer, idx);
                        }
                        // Resolve which effect this press fires (per-key first,
                        // global fallback) and queue it for the LED thread.
                        self.fire_key_fx(idx);
                    }
                }
                DevEvent::Hid(Event::KeyUp { col, row }) => {
                    // First release while binding commits the collected combo.
                    if self.binding_overlay && !self.binding_draft.is_empty() {
                        self.binding_overlay = false;
                        self.overlay_chord = std::mem::take(&mut self.binding_draft);
                        let chord = self.overlay_chord.clone();
                        self.persist_config("saving peek shortcut", move |cfg| {
                            cfg.overlay_trigger = chord.first().copied();
                            cfg.overlay_chord = chord;
                        });
                    } else if self.overlay_chord.contains(&[row, col]) {
                        self.peek_until = Some(Instant::now());
                    }
                    if let Some(idx) = self.geometry().key_index(row, col) {
                        self.pressed[idx] = false;
                        self.record_combo(idx);
                    }
                }
                DevEvent::Hid(_) => {}
            }
        }
        let mut flash_job_finished = false;
        if let Some(rx) = &self.flash_rx {
            while let Ok(s) = rx.try_recv() {
                self.flash_state = Some(s);
            }
            flash_job_finished = flash_job_terminal(self.flash_state.as_ref());
            // Drive the build modal's phase/progress from the flash stage.
            match &self.flash_state {
                Some(FlashState::Downloading) => {
                    self.build_phase = "Downloading firmware…".into();
                    self.build_progress = self.build_progress.max(0.97);
                }
                Some(FlashState::WaitingForBootloader) => {
                    self.build_phase = "Press the Voyager's reset button…".into();
                    self.build_progress = self.build_progress.max(0.97);
                }
                Some(FlashState::Working { phase, fraction }) => {
                    self.build_phase = format!("Flashing: {phase}");
                    self.build_progress = 0.97 + 0.03 * fraction.clamp(0.0, 1.0);
                }
                Some(FlashState::Done) => {
                    self.build_busy = false;
                    self.build_progress = 1.0;
                    if self.expected_firmware_state.is_some() {
                        self.flash_write_completed = true;
                        self.build_phase = "Flashed - waiting for reconnect…".into();
                        self.build_result = Some(Ok(
                            "Firmware was written. Waiting for the keyboard to confirm the new state.".into(),
                        ));
                        if let (Some(generation), Some((_, serial))) =
                            (self.connection_generation, self.connected.clone())
                        {
                            let reported = firmware_state::state_id_from_serial(&serial);
                            self.confirm_expected_firmware(generation, reported);
                        }
                    } else {
                        self.build_phase = "Flashed ✓".into();
                        self.build_result =
                            Some(Ok("Firmware flashed - the keyboard will reconnect.".into()));
                    }
                }
                Some(FlashState::Failed(e)) => {
                    self.build_busy = false;
                    self.build_state_id = None;
                    self.expected_firmware_state = None;
                    self.expected_firmware_generation = None;
                    self.flash_write_completed = false;
                    self.build_phase = "Flash failed".into();
                    self.build_result = Some(Err(e.clone()));
                }
                None => {}
            }
        }
        if flash_job_finished {
            // Keep the terminal state for the UI, but stop re-processing it on
            // every frame. Otherwise a confirmed/mismatched reconnect result
            // is overwritten on the next frame by the stale FlashState::Done.
            self.flash_rx = None;
        }
        if let Some(rx) = &self.build_rx {
            let mut msgs = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
            for msg in msgs {
                match msg {
                    BuildMsg::Log(line) => {
                        self.note_build_phase(&line);
                        self.build_log.push_str(&line);
                        self.build_log.push('\n');
                    }
                    BuildMsg::Built(bin) => {
                        let state_id = self.build_state_id.take();
                        self.last_build_bin = Some(bin.clone());
                        self.last_build_state_id = state_id.clone();
                        if self.build_flash_after {
                            self.build_phase = "Compiled - flashing…".into();
                            self.build_progress = 0.97;
                            self.build_log.push_str("✓ compiled - flashing…\n");
                            self.start_flash_job(
                                Some(bin.to_string_lossy().into_owned()),
                                false,
                                state_id,
                            );
                        } else {
                            self.build_busy = false;
                            self.build_phase = "Done".into();
                            self.build_progress = 1.0;
                            self.build_result = Some(Ok(format!("Built {}", bin.display())));
                            self.build_log
                                .push_str(&format!("✓ built: {}\n", bin.display()));
                        }
                    }
                    BuildMsg::Failed(e) => {
                        self.build_log.push_str(&format!("✗ {e}\n"));
                        self.build_busy = false;
                        self.build_state_id = None;
                        self.expected_firmware_state = None;
                        self.build_phase = "Failed".into();
                        self.build_result = Some(Err(e));
                    }
                }
            }
        }
    }

    /// Update the build phase + progress estimate from a streamed log line.
    pub(super) fn note_build_phase(&mut self, line: &str) {
        let (phase, prog): (&str, f32) = if line.contains("Fetching generated source") {
            ("Fetching layout source…", 0.08)
        } else if line.contains("Applying")
            || line.contains("Adding layer")
            || line.contains("Generating")
            || line.starts_with("Enabled")
        {
            ("Patching firmware source…", 0.20)
        } else if line.contains("Compiling with qmk") {
            ("Compiling firmware…", 0.30)
        } else if line.contains("Compiling:") || line.contains("Compiling ") {
            self.build_compiles += 1;
            // Asymptotic ramp across the compile band (0.30 → 0.90).
            let c = self.build_compiles as f32;
            ("Compiling firmware…", 0.30 + 0.60 * (c / (c + 40.0)))
        } else if line.contains("Linking") {
            ("Linking…", 0.93)
        } else if line.contains("Creating") || line.contains("Copying") {
            ("Finishing…", 0.96)
        } else {
            return;
        };
        self.build_phase = phase.to_string();
        self.build_progress = self.build_progress.max(prog);
    }

    pub(super) fn reconcile_background_jobs(&mut self, _ctx: &egui::Context) {
        // Guard: seize while enabled AND a keyboard is connected.
        #[cfg(target_os = "macos")]
        {
            let want = self.guard_enabled && self.connected.is_some();
            if want && self.guard.is_none() {
                match crate::macos_kb::ensure_input_monitoring()
                    .and_then(|()| crate::macos_kb::seize_builtin())
                {
                    Ok(g) => {
                        self.guard_error = None;
                        self.guard = Some(g);
                        // seize_builtin() only returns Ok after verifying with
                        // hidutil itself, so this is already ground-truthed.
                        self.guard_hidutil_ok = true;
                        self.guard_checked = Instant::now();
                    }
                    Err(e) => {
                        self.guard_error = Some(format!("{e:#}"));
                        self.guard_enabled = false;
                    }
                }
            } else if !want && self.guard.is_some() {
                self.guard = None;
                self.guard_hidutil_ok = false;
            }

            // Ground-truth recheck: hidutil can silently drop a remap (a
            // sleep/wake cycle, an OS update) without our own flag ever
            // noticing. Re-verify periodically and try one silent reapply
            // before telling the UI it's broken.
            if self.guard.is_some() && self.guard_checked.elapsed() > Duration::from_secs(5) {
                self.guard_hidutil_ok = crate::macos_kb::recheck();
                self.guard_checked = Instant::now();
            }
        }

        // Glow: re-push to the physical keyboard when something changed - but
        // not while an RGB animation owns the LEDs (they'd fight).
        let anim_active = self
            .anim
            .lock()
            .map(|a| a.effect != Effect::Off || !a.events.is_empty())
            .unwrap_or(false);
        if self.sync_glow && !anim_active && self.connected.is_some() && self.needs_push {
            if self.push_glow() {
                self.needs_push = false;
            } else {
                self.connected = None;
                self.connection_generation = None;
            }
        }

        // Restart the watcher after reconnect so the current frontmost app
        // is applied to the new HID session even if the app itself did not change.
        let want_autolayer = self.autolayer_enabled && self.connected.is_some();
        if want_autolayer && self.autolayer.is_none() {
            #[cfg(target_os = "macos")]
            {
                self.autolayer = Some(worker::spawn_autolayer(
                    self.rules.clone(),
                    self.cmd_tx.clone(),
                    _ctx.clone(),
                ));
            }
        } else if !want_autolayer && self.autolayer.is_some() {
            self.autolayer = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.update_frame(ctx, frame);
    }

    /// Clear to fully transparent so the peek viewport's low-alpha content
    /// shows the desktop through it. The main window stays opaque because its
    /// panels paint solid fills over every pixel.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Clean shutdown: hand the LEDs back to the firmware and restore the
    /// built-in keyboard, before threads are torn down by process exit. The
    /// anim thread's own release is racy at exit; this makes it reliable.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.cmd_tx.send(KbCmd::RgbRelease);
        #[cfg(target_os = "macos")]
        {
            self.guard = None;
            crate::macos_kb::force_restore_if_active();
        }
    }
}
