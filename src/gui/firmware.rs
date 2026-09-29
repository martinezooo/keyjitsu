//! Firmware build/flash workflow and firmware-status controls.
//!
//! This module owns the irreversible path: compose desired state, persist its
//! identity, build QMK firmware, flash it, and confirm the reconnect marker.

use super::*;

impl App {
    pub(super) fn flash_in_progress(&self) -> bool {
        self.flash_rx.is_some()
            && matches!(
                self.flash_state,
                None | Some(FlashState::Downloading)
                    | Some(FlashState::WaitingForBootloader)
                    | Some(FlashState::Working { .. })
            )
    }

    pub(super) fn start_flash_job(
        &mut self,
        input: Option<String>,
        latest: bool,
        expected_state: Option<String>,
    ) {
        if self.flash_in_progress() {
            return;
        }
        self.expected_firmware_generation = expected_state.as_ref().and(self.connection_generation);
        self.expected_firmware_state = expected_state;
        self.flash_write_completed = false;
        self.flash_state = None;
        self.flash_cancel = Arc::new(AtomicBool::new(false));
        let current_serial = latest
            .then(|| self.connected.as_ref().map(|(_, serial)| serial.clone()))
            .flatten();
        self.flash_rx = Some(worker::spawn_flash(
            input,
            latest,
            current_serial,
            self.flash_cancel.clone(),
            self.egui_ctx.clone(),
        ));
    }

    pub(super) fn confirm_expected_firmware(&mut self, generation: u64, reported: Option<&str>) {
        if !self.flash_write_completed {
            return;
        }
        if !is_post_flash_generation(self.expected_firmware_generation, generation) {
            return;
        }
        let Some(expected) = self.expected_firmware_state.take() else {
            return;
        };
        self.expected_firmware_generation = None;
        self.flash_write_completed = false;

        match confirm_firmware_state(&expected, reported) {
            FirmwareConfirmation::Confirmed => {
                self.build_phase = "Firmware confirmed ✓".into();
                self.build_result =
                    Some(Ok("Firmware flashed and confirmed by the keyboard.".into()));
            }
            FirmwareConfirmation::Mismatch => {
                self.build_phase = "Firmware not confirmed".into();
                self.build_result = Some(Err(
                    "The keyboard reconnected with a different firmware state. Pending changes were kept.".into(),
                ));
            }
        }
    }

    pub(super) fn flash_last_build(&mut self) {
        if self.build_busy || self.flash_in_progress() {
            return;
        }
        let Some(bin) = self.last_build_bin.clone() else {
            return;
        };
        let Some(state_id) = self.last_build_state_id.clone() else {
            return;
        };

        self.build_busy = true;
        self.build_open = true;
        self.build_phase = "Flashing built firmware…".into();
        self.build_progress = 0.97;
        self.build_result = None;
        self.start_flash_job(
            Some(bin.to_string_lossy().into_owned()),
            false,
            Some(state_id),
        );
    }

    pub(super) fn start_local_build(&mut self, flash_after: bool) {
        // Don't start a build while one is running, or on top of a flash that's
        // still writing to the device (would spawn a second concurrent flasher).
        if self.build_busy || self.flash_in_progress() {
            return;
        }
        let Some((_, serial)) = &self.connected else {
            return;
        };
        let Ok(id) = LayoutId::from_serial(serial) else {
            return;
        };
        if self.active_profile.is_some()
            && !self
                .profile_state
                .as_ref()
                .is_some_and(|state| self.profile_matches_layout(state))
        {
            self.build_open = true;
            self.build_busy = false;
            self.build_phase = "Profile mismatch".into();
            self.build_result = Some(Err(
                "The selected firmware profile targets a different Oryx layout/revision. Switch to device or choose a matching profile before building.".into(),
            ));
            return;
        }

        if matches!(
            self.device_state_kind(),
            DeviceStateKind::MissingFirmwareState | DeviceStateKind::UnknownDeviceIdentity
        ) {
            self.build_open = true;
            self.build_busy = false;
            self.build_phase = "State unknown".into();
            self.build_result = Some(Err(
                "The keyboard reports a Keyjitsu firmware state that is not available locally. Rebuilding from Oryx could discard working firmware changes.".into(),
            ));
            return;
        }

        if self.unsaved_glow_count() > 0 && !self.save_glow() {
            self.build_open = true;
            self.build_busy = false;
            self.build_phase = "Save failed".into();
            self.build_result = Some(Err(
                "Could not save the glow draft before building firmware.".into(),
            ));
            return;
        }

        self.build_state_id = None;
        self.expected_firmware_state = None;
        self.expected_firmware_generation = None;
        self.flash_write_completed = false;
        self.last_build_bin = None;
        self.last_build_state_id = None;
        self.build_result = None;

        let n_keys = self.geometry().len();
        let oryx = self.oryx_layer_count();
        let (mut full_edits, mut full_dances) = self.desired_firmware_maps();
        for code in full_edits.values_mut() {
            *code = crate::key_action::canonicalize_qmk_code(code);
        }
        for slots in full_dances.values_mut() {
            for slot in slots.iter_mut().flatten() {
                *slot = crate::key_action::canonicalize_qmk_code(slot);
            }
        }

        let invalid_edit = full_edits
            .keys()
            .chain(full_dances.keys())
            .find(|(layer, key)| *layer >= oryx || *key >= n_keys)
            .copied();
        let invalid_custom = self
            .custom_layers
            .iter()
            .enumerate()
            .find_map(|(layer, custom)| {
                let mut seen = std::collections::HashSet::new();
                custom.keys.iter().find_map(|entry| {
                    let key = entry.key as usize;
                    (key >= n_keys || entry.code.trim().is_empty() || !seen.insert(entry.key))
                        .then_some((oryx + layer as u8, key))
                })
            });
        if let Some((layer, key)) = invalid_edit.or(invalid_custom) {
            self.build_open = true;
            self.build_phase = "Invalid state".into();
            self.build_result = Some(Err(format!(
                "Cannot build: invalid key state at layer {layer}, key {key}."
            )));
            return;
        }

        let state = FirmwareState::new(
            id.hash.clone(),
            id.revision.clone(),
            full_edits
                .iter()
                .map(|(&(layer, key), code)| FirmwareEdit {
                    layer,
                    key: key as u16,
                    code: code.clone(),
                })
                .collect(),
            full_dances
                .iter()
                .map(|(&(layer, key), slots)| FirmwareDance {
                    layer,
                    key: key as u16,
                    slots: slots.clone(),
                })
                .collect(),
            self.custom_layers.clone(),
            self.glow_work
                .iter()
                .map(|(&(layer, key), &rgb)| FirmwareGlow {
                    layer,
                    key: key as u16,
                    rgb,
                })
                .collect(),
        );
        let state_id = match state.save() {
            Ok(id) => id,
            Err(e) => {
                self.build_open = true;
                self.build_busy = false;
                self.build_phase = "Failed".into();
                self.build_result =
                    Some(Err(format!("could not persist firmware identity: {e:#}")));
                return;
            }
        };
        self.build_state_id = Some(state_id.clone());
        let firmware_serial = format!(
            "{}/{}{}{}",
            id.hash,
            id.revision,
            firmware_state::SERIAL_MARKER,
            state_id
        );

        // Translate the complete effective state (layer, visual key) into QMK
        // LAYOUT positions for the local source patcher.
        let edits: Vec<KeyEdit> = full_edits
            .iter()
            .filter(|(&(_, key), _)| key < n_keys)
            .map(|(&(layer, key), code)| KeyEdit {
                layer,
                position: self.geometry().keys[key].layout_pos as usize,
                keycode: code.clone(),
            })
            .collect();
        let dances: Vec<crate::keymap::DanceSpec> = full_dances
            .iter()
            .filter(|(&(_, key), _)| key < n_keys)
            .map(|(&(layer, key), slots)| crate::keymap::DanceSpec {
                layer,
                position: self.geometry().keys[key].layout_pos as usize,
                tap: slots[0].clone(),
                hold: slots[1].clone(),
                double_tap: slots[2].clone(),
                tap_hold: slots[3].clone(),
            })
            .collect();
        // User-authored layers → new LAYOUT blocks (keys visual→LAYOUT pos).
        let new_layers: Vec<localbuild::NewLayer> = self
            .custom_layers
            .iter()
            .enumerate()
            .map(|(i, cl)| localbuild::NewLayer {
                position: oryx + i as u8,
                keys: cl
                    .keys
                    .iter()
                    .map(|k| {
                        (
                            self.geometry().keys[k.key as usize].layout_pos as usize,
                            crate::key_action::canonicalize_qmk_code(&k.code),
                        )
                    })
                    .collect(),
            })
            .collect();
        let glow: Vec<localbuild::GlowEdit> = self
            .glow_work
            .iter()
            .filter(|((layer, key), _)| *layer < oryx && *key < n_keys)
            .map(|(&(layer, key), &rgb)| localbuild::GlowEdit {
                layer,
                // Oryx `ledmap` follows visual key/LED order, not LAYOUT order.
                led: key,
                rgb,
            })
            .collect();
        self.build_log.clear();
        if self.device_state_kind() == DeviceStateKind::OryxBaseline {
            self.build_log.push_str(
                "WARNING: building from the Oryx baseline. Existing untracked custom firmware changes cannot be reconstructed from the device.\n",
            );
        }
        self.build_busy = true;
        self.build_flash_after = flash_after;
        self.build_open = true;
        self.build_phase = "Starting…".into();
        self.build_progress = 0.02;
        self.build_compiles = 0;
        self.build_result = None;
        // Clear any terminal flash state from a PREVIOUS run so the modal
        // doesn't immediately read "Flashed ✓" over a build that's still going.
        self.flash_state = None;
        self.flash_rx = None;
        self.build_cancel = Arc::new(AtomicBool::new(false));
        self.build_rx = Some(localbuild::spawn_build(
            localbuild::BuildSpec {
                revision: id.revision.clone(),
                edits,
                dances,
                new_layers,
                glow,
                firmware_serial: Some(firmware_serial),
            },
            self.build_cancel.clone(),
            self.egui_ctx.clone(),
        ));
    }

    /// Live state bar: device/firmware truth, pending firmware edits, and
    /// local glow edits are separate states with separate actions.
    pub(super) fn ui_edit_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.sync_glow, "show glow on keyboard")
                .on_hover_text("mirror these colors onto the physical LEDs (takes RGB control)")
                .changed()
            {
                if self.sync_glow {
                    self.needs_push = true;
                } else {
                    let _ = self.cmd_tx.send(KbCmd::RgbRelease);
                }
            }

            ui.separator();
            let pending = self.pending_firmware_count();
            if matches!(
                self.device_state_kind(),
                DeviceStateKind::MissingFirmwareState | DeviceStateKind::UnknownDeviceIdentity
            ) {
                status_pill(ui, "⚠ device state unknown", pal::AMBER);
            } else if pending > 0 {
                status_pill(
                    ui,
                    &format!(
                        "{pending} pending firmware change{}",
                        if pending == 1 { "" } else { "s" }
                    ),
                    pal::AMBER,
                );
            } else if self.firmware_state_verified() {
                status_pill(ui, "firmware synced", pal::GREEN);
            } else if self.device_state_kind() == DeviceStateKind::RecoveredLocalBuild {
                status_pill(ui, "local firmware state restored", pal::GREEN);
            } else if self.connected.is_some() {
                status_pill(ui, "device state unverified", pal::AMBER);
            } else {
                ui.weak("no pending firmware changes");
            }

            let glow_unsaved = self.unsaved_glow_count();
            if glow_unsaved > 0 {
                ui.separator();
                ui.colored_label(
                    pal::AMBER,
                    format!(
                        "{glow_unsaved} unsaved glow change{}",
                        if glow_unsaved == 1 { "" } else { "s" }
                    ),
                );
                let save_label = if self.active_profile.is_some() {
                    "save profile"
                } else {
                    "save draft"
                };
                if ui.button(save_label).clicked() {
                    let _ = self.save_glow();
                }
                if ui.button("discard glow draft").clicked() {
                    self.discard_glow();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Flash file…").clicked() {
                    self.show_flash = true;
                }
                self.layout_profile_controls(ui);
                if pending > 0 {
                    let ready = self.env.is_ready()
                        && self.connected.is_some()
                        && !matches!(
                            self.device_state_kind(),
                            DeviceStateKind::MissingFirmwareState
                                | DeviceStateKind::UnknownDeviceIdentity
                        )
                        && !self.build_busy
                        && !self.flash_in_progress();
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(
                                RichText::new("⚙ Build & flash").color(Color32::WHITE),
                            )
                            .fill(pal::VIOLET),
                        )
                        .clicked()
                    {
                        self.start_local_build(true);
                    }
                }
                if let Some(
                    FlashState::Working { .. }
                    | FlashState::WaitingForBootloader
                    | FlashState::Downloading,
                ) = self.flash_state
                {
                    status_pill(ui, "flashing…", pal::AMBER);
                }
                if self.layout.is_none() && self.connected.is_some() {
                    ui.label(
                        RichText::new("no Oryx layout - keys light without legends")
                            .size(11.5)
                            .color(pal::TEXT_DIM),
                    );
                }
            });
        });
        self.layout_profile_editor(ui);
    }

    pub(super) fn ui_localbuild(&mut self, ui: &mut egui::Ui) {
        // Uses the cached env - recompute only on demand (each check spawns
        // `which` processes, so never do it per frame).
        ui.horizontal(|ui| {
            status_dot(ui, self.env.qmk_cli);
            ui.label(RichText::new("qmk CLI").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.env.firmware_dir.is_some());
            ui.label(RichText::new("qmk_firmware tree").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.env.arm_gcc);
            ui.label(RichText::new("arm-gcc").color(pal::TEXT_MUTED));
            ui.add_space(10.0);
            status_dot(ui, self.connected.is_some());
            ui.label(RichText::new("zsa/voyager").color(pal::TEXT_MUTED));
        });
        if let Some(d) = &self.env.firmware_dir {
            ui.label(
                RichText::new(format!("tree: {}", d.display()))
                    .size(11.5)
                    .monospace()
                    .color(pal::TEXT_DIM),
            );
        }

        if !self.env.is_ready() {
            egui::CollapsingHeader::new(RichText::new("Setup guide").color(pal::AMBER))
                .default_open(true)
                .show(ui, |ui| {
                    if !self.env.qmk_cli {
                        ui.label("1. Install the QMK CLI:");
                        ui.code("pip3 install qmk   # or: brew install qmk/qmk/qmk");
                    }
                    if self.env.firmware_dir.is_none() {
                        ui.label("2. Fetch ZSA's firmware tree (one-time):");
                        ui.code("qmk setup zsa/qmk_firmware -b firmware25");
                    }
                    ui.horizontal(|ui| {
                        if ui.button("↻ Recheck setup").clicked() {
                            self.env = localbuild::detect_env();
                        }
                    });
                });
            return;
        }

        // Ready - power-user actions. Everything here runs locally; the only
        // network is an anonymous read of the generated QMK source.
        ui.add_space(4.0);
        let pending = self.pending_firmware_count();
        if matches!(
            self.device_state_kind(),
            DeviceStateKind::MissingFirmwareState | DeviceStateKind::UnknownDeviceIdentity
        ) {
            ui.colored_label(
                pal::AMBER,
                "⚠ The connected firmware state is unknown. Building is blocked to avoid losing working keyboard changes.",
            );
        } else if pending > 0 {
            ui.colored_label(
                pal::AMBER,
                format!(
                    "● {pending} pending firmware change{}",
                    if pending == 1 { "" } else { "s" }
                ),
            );
        } else {
            ui.weak("No pending firmware changes. Build can still reproduce the confirmed device state.");
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let can = self.connected.is_some()
                && !self.build_busy
                && !matches!(
                    self.device_state_kind(),
                    DeviceStateKind::MissingFirmwareState | DeviceStateKind::UnknownDeviceIdentity
                );
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(RichText::new("⚙ Build firmware").color(Color32::WHITE))
                        .fill(pal::VIOLET),
                )
                .clicked()
            {
                self.start_local_build(false);
            }
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(RichText::new("⚡ Build & flash").color(Color32::WHITE))
                        .fill(pal::VIOLET),
                )
                .clicked()
            {
                self.start_local_build(true);
            }
            if ui.button("🔦 Flash a file / Oryx URL…").clicked() {
                self.show_flash = true;
            }
            if ui.button("📂 Open firmware folder").clicked() {
                if let Some(d) = &self.env.firmware_dir {
                    match crate::platform::reveal_path(
                        &d.join("keyboards/zsa/voyager/keymaps/keyjitsu"),
                    ) {
                        Ok(()) => self.file_action_error = None,
                        Err(e) => {
                            self.file_action_error =
                                Some(format!("could not open firmware folder: {e:#}"));
                        }
                    }
                }
            }
            if ui.button("↻ Recheck").clicked() {
                self.env = localbuild::detect_env();
            }
            if self.build_busy {
                ui.spinner();
                if ui.button("✕ cancel").clicked() {
                    self.build_cancel.store(true, Ordering::SeqCst);
                }
            }
        });

        if let Some(e) = &self.file_action_error {
            ui.colored_label(pal::RED, e);
        }

        if let Some(bin) = self.last_build_bin.clone() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(pal::GREEN, "✓ built");
                if ui
                    .link(
                        RichText::new(bin.display().to_string())
                            .size(11.5)
                            .monospace(),
                    )
                    .clicked()
                {
                    match crate::platform::reveal_path(&bin) {
                        Ok(()) => self.file_action_error = None,
                        Err(e) => {
                            self.file_action_error =
                                Some(format!("could not open build location: {e:#}"));
                        }
                    }
                }
                let can_flash = self.last_build_state_id.is_some()
                    && !self.build_busy
                    && !self.flash_in_progress();
                if ui
                    .add_enabled(can_flash, egui::Button::new("Flash this build"))
                    .clicked()
                {
                    self.flash_last_build();
                }
            });
        }
        if !self.build_log.is_empty() {
            egui::CollapsingHeader::new("Build log").show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&self.build_log)
                                .monospace()
                                .size(11.0)
                                .color(pal::TEXT_MUTED),
                        );
                    });
            });
        }
        ui.add_space(2.0);
        ui.weak("To flash: keyjitsu waits for the bootloader - press the Voyager's reset button when prompted, and don't unplug it while it writes.");
    }

    pub(super) fn flash_controls(&mut self, ui: &mut egui::Ui) {
        ui.weak("Firmware, separate from the glow above. This flashes a complete firmware file or Oryx URL. To change what keys do, stage edits in Layout and use Build & flash.");
        if let Some((_, serial)) = &self.connected {
            ui.label(format!("current firmware/layout: {serial}"));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Oryx URL or .bin path:");
            ui.text_edit_singleline(&mut self.flash_input);
        });

        let busy = matches!(
            self.flash_state,
            Some(FlashState::Downloading)
                | Some(FlashState::WaitingForBootloader)
                | Some(FlashState::Working { .. })
        );
        ui.horizontal(|ui| {
            let can_latest = self.connected.is_some() && !busy;
            if ui
                .add_enabled(can_latest, egui::Button::new("⚡ flash latest from Oryx"))
                .on_hover_text(
                    "updates to the newest revision of the layout already on the keyboard",
                )
                .clicked()
            {
                self.start_flash_job(None, true, None);
            }
            let can_input = !self.flash_input.trim().is_empty() && !busy;
            if ui
                .add_enabled(can_input, egui::Button::new("flash from URL/file"))
                .clicked()
            {
                self.start_flash_job(Some(self.flash_input.trim().to_string()), false, None);
            }
            let flash_is_writing = flash_is_writing(self.flash_state.as_ref());
            if busy
                && flash_can_cancel(self.flash_state.as_ref())
                && ui.button("✕ cancel").clicked()
            {
                self.flash_cancel.store(true, Ordering::SeqCst);
            } else if flash_is_writing {
                ui.label(
                    RichText::new("Writing firmware - do not unplug")
                        .size(11.0)
                        .color(pal::TEXT_DIM),
                );
            }
        });
        ui.add_space(8.0);

        match &self.flash_state {
            None => {
                ui.weak("After starting, press the keyboard's RESET button (Voyager: tiny button on the left half).");
            }
            Some(FlashState::Downloading) => {
                ui.label("Downloading firmware…");
                ui.add(ProgressBar::new(0.0).animate(true));
            }
            Some(FlashState::WaitingForBootloader) => {
                ui.label(RichText::new("Press the RESET button on the keyboard now").strong());
                ui.add(ProgressBar::new(0.0).animate(true));
            }
            Some(FlashState::Working { phase, fraction }) => {
                ui.label(*phase);
                ui.add(ProgressBar::new(*fraction).show_percentage());
            }
            Some(FlashState::Done) => {
                ui.colored_label(
                    pal::GREEN,
                    "✓ Flash complete - the keyboard reconnects automatically.",
                );
            }
            Some(FlashState::Failed(e)) => {
                ui.colored_label(pal::RED, e);
            }
        }
    }
}
