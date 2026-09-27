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
        if self.firmware_state_unknown {
            self.build_open = true;
            self.build_busy = false;
            self.build_phase = "State unknown".into();
            self.build_result = Some(Err(
                "The keyboard reports a Keyjitsu firmware state that is not available locally. Rebuilding from Oryx could discard working firmware changes.".into(),
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
        let (full_edits, full_dances) = self.desired_firmware_maps();

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
                            k.code.clone(),
                        )
                    })
                    .collect(),
            })
            .collect();
        self.build_log.clear();
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
            id.revision.clone(),
            edits,
            dances,
            new_layers,
            Some(firmware_serial),
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
            if self.firmware_state_unknown {
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
                if ui.button("save glow").clicked() {
                    self.save_glow();
                }
                if ui.button("discard glow").clicked() {
                    self.discard_glow();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Flash file…").clicked() {
                    self.show_flash = true;
                }
                if pending > 0 {
                    let ready = self.env.is_ready()
                        && self.connected.is_some()
                        && !self.firmware_state_unknown
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
    }
}
