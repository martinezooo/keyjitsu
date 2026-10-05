//! Live keyboard view and key editor.
//!
//! Rendering and editing live here; device/Oryx/firmware truth lives in
//! `state.rs`. This separation is intentional: Live must display one
//! canonical state instead of reinterpreting raw sources on its own.

use super::*;

impl App {
    /// Compact connection/device text for the native window title.
    pub(super) fn connection_title(&self) -> String {
        match (&self.connected, self.device_state_kind()) {
            (Some((model, _)), DeviceStateKind::MissingFirmwareState) => {
                format!("{model} · state unknown")
            }
            (Some((model, _)), DeviceStateKind::OryxBaseline) => match &self.layout {
                Some(layout) => format!("{model} · {} · Oryx baseline", layout.title),
                None => format!("{model} · Oryx baseline"),
            },
            (Some((model, _)), DeviceStateKind::RecoveredLocalBuild) => match &self.layout {
                Some(layout) => format!("{model} · {} · local build", layout.title),
                None => format!("{model} · local build"),
            },
            (Some((model, serial)), DeviceStateKind::VerifiedFirmware) => match &self.layout {
                Some(layout) => format!("{model} · {}", layout.title),
                None => format!("{model} · {serial}"),
            },
            (None, DeviceStateKind::OfflineSnapshot) => "No keyboard".to_string(),
            _ => "device state inconsistent".to_string(),
        }
    }

    /// Stable workspace header for Layout. Layers are first-class tabs here,
    /// rather than nested navigation items mixed with app destinations.
    fn ui_layout_header(&mut self, ui: &mut egui::Ui) {
        let layer_count = self.layer_count();
        let names: Vec<String> = (0..layer_count).map(|n| self.layer_name(n)).collect();
        let view = self.view_layer.min(layer_count.saturating_sub(1));
        let active = self.active_layer;
        let oryx = self.oryx_layer_count();

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("LAYOUT EDITOR")
                        .strong()
                        .size(10.5)
                        .color(pal::TEXT_DIM),
                );
                ui.label(
                    RichText::new(names.get(view as usize).cloned().unwrap_or_default())
                        .strong()
                        .size(21.0)
                        .color(pal::TEXT),
                );
                let source = if view >= oryx {
                    "Keyjitsu layer"
                } else {
                    "Oryx layer"
                };
                let activity = if view == active {
                    "active on keyboard"
                } else {
                    "previewing"
                };
                ui.label(
                    RichText::new(format!(
                        "Layer {} of {} · {source} · {activity}",
                        view + 1,
                        layer_count
                    ))
                    .size(11.5)
                    .color(if view == active {
                        pal::CYAN
                    } else {
                        pal::TEXT_MUTED
                    }),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.layout_profile_controls(ui);
            });
        });
        ui.add_space(8.0);

        let mut select_layer = None;
        let mut begin_rename = false;
        let mut request_remove = false;
        egui::Frame::new()
            .fill(pal::CARD)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("LAYERS")
                            .strong()
                            .size(10.0)
                            .color(pal::TEXT_DIM),
                    );
                    for (n, name) in names.iter().enumerate() {
                        let n = n as u8;
                        let label = if n == active {
                            format!("● {name}")
                        } else {
                            name.clone()
                        };
                        let button = egui::Button::new(RichText::new(label).size(11.5).color(
                            if n == view {
                                Color32::WHITE
                            } else {
                                pal::TEXT_MUTED
                            },
                        ))
                        .fill(if n == view { pal::VIOLET } else { pal::INPUT })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if n == active { pal::CYAN } else { pal::BORDER },
                        ));
                        if ui.add(button).clicked() {
                            select_layer = Some(n);
                        }
                    }

                    ui.menu_button("＋ Layer", |ui| {
                        if ui.button("Blank layer").clicked() {
                            self.new_layer_open = true;
                            self.new_layer_source = None;
                            self.new_layer_name = format!("Layer {layer_count}");
                            self.delete_layer_confirm = None;
                            ui.close();
                        }
                        if !names.is_empty() {
                            ui.separator();
                            ui.weak("Duplicate existing layer");
                            for (n, name) in names.iter().enumerate() {
                                if ui.button(name).clicked() {
                                    self.new_layer_open = true;
                                    self.new_layer_source = Some(n as u8);
                                    self.new_layer_name = format!("{name} copy");
                                    self.delete_layer_confirm = None;
                                    ui.close();
                                }
                            }
                        }
                    });

                    ui.menu_button("Layer actions", |ui| {
                        if view >= oryx && ui.button("Rename layer…").clicked() {
                            begin_rename = true;
                            ui.close();
                        }
                        if layer_count > 1
                            && ui
                                .button(RichText::new("Remove layer…").color(pal::RED))
                                .clicked()
                        {
                            request_remove = true;
                            ui.close();
                        }
                    });

                    ui.separator();
                    let mut follow = self.follow;
                    if toggle(ui, &mut follow)
                        .on_hover_text("Keep this view on the layer active on the keyboard")
                        .changed()
                    {
                        self.follow = follow;
                        if follow {
                            self.view_layer = active;
                        }
                    }
                    ui.label(RichText::new("Follow").size(11.0).color(pal::TEXT_MUTED));

                    let mut glow = self.sync_glow;
                    if toggle(ui, &mut glow)
                        .on_hover_text("Mirror preview colors on the physical keyboard")
                        .changed()
                    {
                        self.sync_glow = glow;
                        if glow {
                            self.needs_push = true;
                        } else {
                            let _ = self.cmd_tx.send(KbCmd::RgbRelease);
                        }
                    }
                    ui.label(RichText::new("Live glow").size(11.0).color(pal::TEXT_MUTED));
                });
            });

        if let Some(layer) = select_layer {
            self.view_layer = layer;
            self.follow = false;
            self.selected_key = None;
            self.delete_layer_confirm = None;
        }
        if begin_rename {
            self.rename_layer_target = Some(view);
            self.rename_layer_name = self.layer_name(view);
            self.delete_layer_confirm = None;
        }
        if request_remove {
            self.delete_layer_confirm = Some(view);
            self.rename_layer_target = None;
        }

        if self.new_layer_open {
            ui.add_space(7.0);
            egui::Frame::new()
                .fill(pal::INPUT)
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("New layer").strong());
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_layer_name)
                                .hint_text("Layer name")
                                .desired_width(180.0),
                        );
                        let can_create = !self.new_layer_name.trim().is_empty();
                        if ui
                            .add_enabled(can_create, egui::Button::new("Create"))
                            .clicked()
                        {
                            let name = self.new_layer_name.trim().to_string();
                            let source = self.new_layer_source;
                            self.add_custom_layer_from(name, source);
                            self.new_layer_open = false;
                            self.new_layer_name.clear();
                            self.new_layer_source = None;
                        }
                        if ui.small_button("Cancel").clicked() {
                            self.new_layer_open = false;
                            self.new_layer_name.clear();
                            self.new_layer_source = None;
                        }
                    });
                });
        }

        if self.rename_layer_target == Some(view) {
            ui.add_space(7.0);
            egui::Frame::new()
                .fill(pal::INPUT)
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Rename layer").strong());
                        ui.add(
                            egui::TextEdit::singleline(&mut self.rename_layer_name)
                                .desired_width(180.0),
                        );
                        let ok = !self.rename_layer_name.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                            self.rename_custom_layer(
                                view,
                                self.rename_layer_name.trim().to_string(),
                            );
                            self.rename_layer_target = None;
                            self.rename_layer_name.clear();
                        }
                        if ui.small_button("Cancel").clicked() {
                            self.rename_layer_target = None;
                            self.rename_layer_name.clear();
                        }
                    });
                });
        }

        if self.delete_layer_confirm == Some(view) {
            ui.add_space(7.0);
            egui::Frame::new()
                .fill(pal::RED.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0, pal::RED.gamma_multiply(0.55)))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Remove ‘{}’? Layer references will be renumbered in this draft.",
                                self.layer_name(view)
                            ))
                            .color(pal::TEXT),
                        );
                        if ui
                            .button(RichText::new("Remove").color(Color32::WHITE))
                            .clicked()
                        {
                            self.remove_layer(view);
                            self.delete_layer_confirm = None;
                            self.rename_layer_target = None;
                        }
                        if ui.small_button("Keep layer").clicked() {
                            self.delete_layer_confirm = None;
                        }
                    });
                });
        }
        self.layout_profile_editor(ui);
    }

    pub(super) fn ui_live(&mut self, ui: &mut egui::Ui, avail_h: f32) {
        self.ui_layout_header(ui);
        ui.add_space(10.0);
        // No layout at all (first run, nothing cached): a friendly hint beats a
        // grid of blank keys.
        if self.layout.is_none() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("⌨").size(46.0).color(pal::TEXT_DIM));
                ui.add_space(10.0);
                if let Some(error) = &self.layout_error {
                    ui.label(
                        RichText::new("Keyboard layout unavailable")
                            .strong()
                            .size(18.0)
                            .color(pal::TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(error)
                            .size(12.0)
                            .color(pal::RED),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("HID is still connected; this is a layout/Oryx/cache problem, not a keyboard connection failure.")
                            .size(11.5)
                            .color(pal::TEXT_DIM),
                    );
                } else if self.connected.is_some() {
                    ui.label(
                        RichText::new(if matches!(
                            self.device_state_kind(),
                            DeviceStateKind::MissingFirmwareState
                                | DeviceStateKind::UnknownDeviceIdentity
                        ) {
                            "Connected keyboard state is unknown"
                        } else {
                            "Reading keyboard layout…"
                        })
                        .strong()
                        .size(18.0)
                        .color(pal::TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if matches!(
                            self.device_state_kind(),
                            DeviceStateKind::MissingFirmwareState
                                | DeviceStateKind::UnknownDeviceIdentity
                        ) {
                            "Keyjitsu will not guess from Oryx when the connected firmware reports a state that cannot be reconstructed locally."
                        } else {
                            "The keyboard is connected. Waiting for its matching layout definition."
                        })
                        .size(12.5)
                        .color(pal::TEXT_MUTED),
                    );
                } else {
                    ui.label(
                        RichText::new("No keyboard connected")
                            .strong()
                            .size(18.0)
                            .color(pal::TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Plug in your Voyager (and quit Keymapp) so Keyjitsu can read your layout. It remembers the last one, so next time you can view and plan it here even with the keyboard unplugged.")
                            .size(12.5)
                            .color(pal::TEXT_MUTED),
                    );
                }
            });
            return;
        }
        // Fit the board to BOTH dimensions: the canvas shrinks (down to the
        // widget's 34px/unit legibility floor) so keyboard + inspector share
        // the window without scrolling; `avail_h` is measured by the caller
        // before any scroll wrapper, so it's the true remaining height.
        let (cols, rows) = self.board_units();
        let chrome = 88.0; // edit bar + canvas margins/shadow
        let (board_w, board_h) = live_board_size(ui.available_width(), avail_h, cols, rows);
        // Keep the board visually centered when there is spare room, but cap
        // the spacer so large windows do not turn the Live page into empty
        // chrome above the primary object.
        ui.add_space(((avail_h - chrome - board_h) / 2.0).clamp(0.0, 24.0));

        let view = self.view_layer;
        // Mirror the LEDs: while the animation engine drives the keyboard, show
        // its live frame; otherwise the static per-layer glow.
        let anim_frame: Option<Vec<Option<Color32>>> = self.anim.lock().ok().and_then(|a| {
            if a.frame.is_empty() {
                None
            } else {
                Some(
                    a.frame
                        .iter()
                        .map(|c| (*c != [0, 0, 0]).then(|| Color32::from_rgb(c[0], c[1], c[2])))
                        .collect(),
                )
            }
        });
        let glow = anim_frame.unwrap_or_else(|| self.glow_colors(view));
        // Live previews the same effective state the editor and build use.
        let editing_layer = self.editing_layer(view);
        let layer = editing_layer.as_ref();
        let combo_keys = self.combo_member_mask(view);
        let sel = self.selected_key;
        // The keyboard sits on its own raised canvas card with a soft top
        // sheen + shadow, so it reads as the main object.
        let clicked = {
            let frame = egui::Frame::new()
                .fill(pal::CARD)
                .stroke(egui::Stroke::new(1.0, pal::BORDER))
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(14, 16))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(70),
                });
            // Center a height-budgeted canvas card; the board fills its width.
            let pad = ((ui.available_width() - board_w) / 2.0).max(0.0);
            let out = ui
                .horizontal(|ui| {
                    ui.add_space(pad);
                    frame.show(ui, |ui| {
                        ui.set_width(board_w - 28.0);
                        draw_keyboard(
                            ui,
                            self.geometry(),
                            layer,
                            &glow,
                            &self.pressed,
                            sel,
                            Some(&combo_keys),
                            1.0,
                            false,
                        )
                        .clicked
                    })
                })
                .inner;
            // Subtle top-light gradient over the card (very low alpha sheen).
            let r = out.response.rect;
            let sheen = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), 70.0));
            let mut mesh = egui::Mesh::default();
            let top = Color32::from_rgba_unmultiplied(255, 255, 255, 6);
            let bottom = Color32::TRANSPARENT;
            let idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(sheen.left_top(), top);
            mesh.colored_vertex(sheen.right_top(), top);
            mesh.colored_vertex(sheen.right_bottom(), bottom);
            mesh.colored_vertex(sheen.left_bottom(), bottom);
            mesh.add_triangle(idx, idx + 1, idx + 2);
            mesh.add_triangle(idx, idx + 2, idx + 3);
            ui.painter().add(egui::Shape::mesh(mesh));
            out.inner
        };

        // Clicking a key selects it for the bottom config panel.
        if let Some(i) = clicked {
            self.selected_key = Some(i);
            self.edit_color = self.current_key_srgb(view, i);
            self.sync_editor_from_key(view, i);
        }

        // Flash lives in its own window now, not in the main flow.
        if self.show_flash {
            let mut open = self.show_flash;
            egui::Window::new("⚡ Flash firmware")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .show(ui.ctx(), |ui| self.flash_controls(ui));
            self.show_flash = open;
        }
    }

    /// Bottom panel: configure whichever key is selected (pressed or clicked).
    pub(super) fn ui_key_panel(&mut self, ui: &mut egui::Ui) {
        let view = self.view_layer;
        let Some(i) = self.selected_key else {
            ui.add_space(4.0);
            if self.connected.is_some() {
                ui.weak("No key selected - press a key on the keyboard, or click one above.");
            } else {
                ui.weak("No key selected - click a key above.");
            }
            ui.add_space(4.0);
            return;
        };
        // Hydrate the slot editor whenever the inspected key/layer changes
        // (covers layer switches, profile loads and env preselection).
        if self.edit_synced != Some((view, i)) {
            self.sync_editor_from_key(view, i);
            self.edit_color = self.current_key_srgb(view, i);
            self.edit_synced = Some((view, i));
        }
        let tap = match self.editing_key(view, i) {
            Some(key) => legend::full_labels_for(&key).tap,
            None => format!("key {i}"),
        };

        let pos = self.geometry().keys[i].layout_pos as usize;
        let staged = self.key_edits.get(&(view, i)).cloned();
        let key_col = self.layout_glow(view, i).unwrap_or(pal::VIOLET);
        let combo_summaries = self.combo_summaries_for_key(view, i);

        // Inspector header: identity first, local draft state second. Build
        // actions live in the page toolbar, never inside a selected key.
        let (preview, warns) = self.compose_slots();
        let staged_dance = self.key_dances.contains_key(&(view, i));
        ui.add_space(2.0);
        egui::Frame::new()
            .fill(pal::CARD)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(9))
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    egui::Frame::new()
                        .fill(pal::INPUT)
                        .stroke(egui::Stroke::new(2.0, key_col))
                        .corner_radius(egui::CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(9, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(if tap.is_empty() {
                                    "-".into()
                                } else {
                                    tap.clone()
                                })
                                .size(16.0)
                                .strong()
                                .color(pal::TEXT),
                            );
                        });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{} · key {i}", self.layer_name(view)))
                            .size(12.5)
                            .color(pal::TEXT_MUTED),
                    );
                    let assigned = self
                        .editing_key(view, i)
                        .map(|k| self.describe_assignment(&k))
                        .unwrap_or_else(|| "No assignment".into());
                    ui.label(
                        RichText::new(assigned)
                            .strong()
                            .size(13.0)
                            .color(pal::VIOLET_HI),
                    );
                });
                ui.add_space(3.0);
                ui.horizontal_wrapped(|ui| {
                    if staged_dance {
                        ui.colored_label(
                            pal::AMBER,
                            format!("Pending · tap dance · firmware position {pos}"),
                        );
                    } else if let Some(sc) = &staged {
                        ui.colored_label(
                            pal::AMBER,
                            format!(
                                "Pending · {} · firmware position {pos}",
                                sc.replace('\n', " → ")
                            ),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("Effective action · {preview}"))
                                .size(11.5)
                                .color(pal::TEXT_DIM),
                        );
                    }
                    if (staged_dance || staged.is_some())
                        && ui
                            .small_button("Revert this key")
                            .on_hover_text("Remove the pending firmware assignment for this key")
                            .clicked()
                    {
                        self.key_edits.remove(&(view, i));
                        self.key_dances.remove(&(view, i));
                        self.save_staged();
                        self.sync_editor_from_key(view, i);
                    }
                });
            });
        if !combo_summaries.is_empty() {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(pal::CARD)
                .stroke(egui::Stroke::new(1.0, pal::AMBER.gamma_multiply(0.55)))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(pal::AMBER, "◆ COMBO");
                        for summary in &combo_summaries {
                            ui.label(RichText::new(summary).strong().color(pal::TEXT));
                        }
                    });
                });
        }
        ui.add_space(6.0);

        // Action cards are stacked for scanning and narrow-window resilience.
        // Each gesture owns its controls; key-level appearance stays with the
        // primary action instead of becoming another spreadsheet column.
        let assignment_editable = self.assignment_editable(view, i);
        if !assignment_editable {
            ui.colored_label(
                pal::AMBER,
                "This key contains Oryx behavior Keyjitsu cannot round-trip yet. Its device assignment is shown read-only so editing cannot silently destroy it.",
            );
            ui.add_space(4.0);
        }
        let mut open_picker: Option<usize> = None;
        let mut clear_slot: Option<usize> = None;
        ui.label(
            RichText::new("ACTIONS")
                .strong()
                .size(10.5)
                .color(pal::TEXT_DIM),
        );
        ui.add_space(3.0);
        ui.vertical(|ui| {
            let mut first = true;
            for slot in 0..4 {
                let visible = slot == 0 || self.edit_slots[slot].is_some() || self.slot_added[slot];
                if !visible {
                    continue;
                }
                egui::Frame::new()
                    .fill(pal::CARD)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(9))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // Type badge: a distinct color per action tier, clearly visible.
                        let tc = SLOT_COLORS[slot];
                        egui::Frame::new()
                            .fill(tc.gamma_multiply(0.28))
                            .stroke(egui::Stroke::new(1.2, tc))
                            .corner_radius(egui::CornerRadius::same(7))
                            .inner_margin(egui::Margin::symmetric(10, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(SLOT_LABELS[slot])
                                        .size(12.0)
                                        .strong()
                                        .color(pal::TEXT),
                                );
                            });

                        // Key chip(s) - click one to change it via the picker. A slot
                        // can hold more than one step ("then press another key"),
                        // tapped in order when the gesture fires; each step gets its
                        // own chip so it can be changed or removed on its own.
                        ui.add_space(5.0);
                        ui.label(RichText::new("Binding").size(10.0).color(pal::TEXT_DIM));
                        let steps: Vec<String> = match &self.edit_slots[slot] {
                            Some(c) => c.split('\n').map(str::to_string).collect(),
                            None => Vec::new(),
                        };
                        let mut remove_step: Option<usize> = None;
                        ui.horizontal_wrapped(|ui| {
                            if steps.is_empty() {
                                let chip_btn = egui::Button::new(
                                    RichText::new("- pick…").size(13.0).color(pal::TEXT),
                                )
                                .fill(pal::INPUT)
                                .stroke(egui::Stroke::new(1.0, pal::BORDER))
                                .min_size(egui::vec2(96.0, 22.0));
                                if ui
                                    .add_enabled(assignment_editable, chip_btn)
                                    .on_hover_text(if assignment_editable {
                                        "click to pick a key"
                                    } else {
                                        "read-only: unsupported Oryx action semantics"
                                    })
                                    .clicked()
                                {
                                    open_picker = Some(slot);
                                    self.picker_step_index = None;
                                    self.picker_append = false;
                                }
                            } else {
                                for (step_i, s) in steps.iter().enumerate() {
                                    if step_i > 0 {
                                        ui.label(
                                            RichText::new("then").size(10.5).color(pal::TEXT_DIM),
                                        );
                                    }
                                    let chip_btn = egui::Button::new(
                                        RichText::new(self.slot_chip_label(s))
                                            .size(13.0)
                                            .color(pal::TEXT),
                                    )
                                    .fill(pal::INPUT)
                                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                                    .min_size(egui::vec2(80.0, 22.0));
                                    if ui
                                        .add_enabled(assignment_editable, chip_btn)
                                        .on_hover_text(if assignment_editable {
                                            "click to change this step"
                                        } else {
                                            "read-only: unsupported Oryx action semantics"
                                        })
                                        .clicked()
                                    {
                                        open_picker = Some(slot);
                                        self.picker_step_index = Some(step_i);
                                        self.picker_append = false;
                                    }
                                    if steps.len() > 1
                                        && ui
                                            .add_enabled(
                                                assignment_editable,
                                                egui::Button::new("✕").small(),
                                            )
                                            .on_hover_text("remove this step")
                                            .clicked()
                                    {
                                        remove_step = Some(step_i);
                                    }
                                }
                            }
                            if ui
                                .add_enabled(assignment_editable, egui::Button::new("+").small())
                                .on_hover_text(
                                    "then press another key (taps in order when this fires)",
                                )
                                .clicked()
                            {
                                open_picker = Some(slot);
                                self.picker_step_index = None;
                                self.picker_append = true;
                            }
                        });
                        if let Some(step_i) = remove_step {
                            let mut steps = steps.clone();
                            steps.remove(step_i);
                            self.edit_slots[slot] = if steps.is_empty() {
                                None
                            } else {
                                Some(steps.join("\n"))
                            };
                            self.stage_slots(view, i);
                        }

                        if first {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("APPEARANCE")
                                    .strong()
                                    .size(10.0)
                                    .color(pal::TEXT_DIM),
                            );
                            ui.add_space(3.0);
                            // Glow color (key-level).
                            ui.horizontal(|ui| {
                                ui.label("Glow");
                                if ui.color_edit_button_srgb(&mut self.edit_color).changed() {
                                    self.set_glow(view, i, self.edit_color);
                                }
                                if ui
                                    .small_button("↺")
                                    .on_hover_text("reset to layout color")
                                    .clicked()
                                {
                                    self.clear_glow(view, i);
                                    self.edit_color = self.current_key_srgb(view, i);
                                }
                            });
                            // On-press effect (key-level): built-ins + ★ sequences.
                            ui.vertical(|ui| {
                                ui.label(RichText::new("On press").size(10.0).color(pal::TEXT_DIM));
                                let mut fx = self.key_fx.get(&(view, i)).cloned().unwrap_or((
                                    FxTrigger::Press,
                                    PressEffect::None,
                                    [255, 255, 255],
                                    None,
                                ));
                                let mut changed = false;
                                let custom_names: Vec<String> = self
                                    .custom_fx
                                    .iter()
                                    .filter(|c| c.preset.is_none())
                                    .map(|c| c.name.clone())
                                    .collect();
                                let saved_press: Vec<(String, PressEffect, [u8; 3])> = self
                                    .custom_fx
                                    .iter()
                                    .filter_map(|c| match c.preset {
                                        Some(FxPresetSource::Press { effect, color }) => {
                                            Some((c.name.clone(), effect, color))
                                        }
                                        _ => None,
                                    })
                                    .collect();
                                let sel_text = match &fx.3 {
                                    Some(n) => format!("★ {n}"),
                                    None => fx.1.label().to_string(),
                                };
                                egui::ComboBox::from_id_salt(("keyfx", i))
                                    .width(ui.available_width().min(260.0))
                                    .selected_text(sel_text)
                                    .show_ui(ui, |ui| {
                                        for (e, label) in PressEffect::ALL {
                                            let is = fx.3.is_none() && fx.1 == e;
                                            if ui.selectable_label(is, label).clicked() {
                                                fx.1 = e;
                                                fx.3 = None;
                                                changed = true;
                                            }
                                        }
                                        if !saved_press.is_empty() || !custom_names.is_empty() {
                                            ui.separator();
                                        }
                                        for (name, effect, color) in &saved_press {
                                            if ui
                                                .selectable_label(false, format!("★ {name}"))
                                                .clicked()
                                            {
                                                fx.1 = *effect;
                                                fx.2 = *color;
                                                fx.3 = None;
                                                changed = true;
                                            }
                                        }
                                        for name in &custom_names {
                                            let is = fx.3.as_deref() == Some(name.as_str());
                                            if ui
                                                .selectable_label(is, format!("★ {name}"))
                                                .clicked()
                                            {
                                                fx.3 = Some(name.clone());
                                                fx.1 = PressEffect::None;
                                                changed = true;
                                            }
                                        }
                                    });
                                if fx.1 != PressEffect::None || fx.3.is_some() {
                                    egui::ComboBox::from_id_salt(("keyfxtrig", i))
                                        .width(ui.available_width().min(180.0))
                                        .selected_text(fx.0.label())
                                        .show_ui(ui, |ui| {
                                            for (t, label) in FxTrigger::ALL {
                                                changed |= ui
                                                    .selectable_value(&mut fx.0, t, label)
                                                    .changed();
                                            }
                                        });
                                    if fx.3.is_none() && fx.1.uses_color() {
                                        changed |= ui.color_edit_button_srgb(&mut fx.2).changed();
                                    }
                                }
                                if changed {
                                    if fx.1 == PressEffect::None && fx.3.is_none() {
                                        self.key_fx.remove(&(view, i));
                                    } else {
                                        self.key_fx.insert((view, i), fx);
                                    }
                                    self.save_key_fx();
                                }
                            });
                        }

                        let can_clear = self.edit_slots[slot].is_some() || self.slot_added[slot];
                        if can_clear {
                            let hover = if slot == 0 {
                                "clear this key assignment (firmware: KC_NO)"
                            } else {
                                "remove this action"
                            };
                            let label = if slot == 0 {
                                "Clear assignment"
                            } else {
                                "Remove action"
                            };
                            if ui
                                .add_enabled(
                                    assignment_editable,
                                    egui::Button::new(
                                        RichText::new(label).size(10.5).color(pal::TEXT_DIM),
                                    )
                                    .small(),
                                )
                                .on_hover_text(hover)
                                .clicked()
                            {
                                clear_slot = Some(slot);
                            }
                        } else {
                            ui.add_space(1.0);
                        }
                    });
                ui.add_space(5.0);
                first = false;
            }
        });

        // ＋ under the table, centered.
        let missing: Vec<usize> = (1..4)
            .filter(|&sl| self.edit_slots[sl].is_none() && !self.slot_added[sl])
            .collect();
        if !missing.is_empty() {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.add_enabled_ui(assignment_editable, |ui| {
                    ui.menu_button(RichText::new("＋ add action").size(12.0), |ui| {
                        for sl in missing {
                            if ui.button(SLOT_LABELS[sl]).clicked() {
                                self.slot_added[sl] = true;
                                open_picker = Some(sl);
                                ui.close();
                            }
                        }
                    });
                });
            });
        }
        // Make the dual-role obvious: Tap + Hold together = "tap sends one,
        // holding sends the other" (LT/mod-tap). The double-* rows are the
        // extra tap-dance actions.
        if self.edit_slots[1].is_some()
            && self.edit_slots[2].is_none()
            && self.edit_slots[3].is_none()
        {
            ui.add_space(2.0);
            ui.label(RichText::new("Tap + Hold = dual-role: tapping sends the Tap key, holding does the Hold action.").size(10.5).color(pal::TEXT_DIM));
        }

        if let Some(slot) = clear_slot {
            self.edit_slots[slot] = None;
            self.slot_added[slot] = false;
            self.stage_slots(view, i);
        }
        if let Some(slot) = open_picker {
            self.picker_slot = slot;
            self.picker_open = true;
            self.picker_search.clear();
            self.picker_combo_mods = [false; 4];
            self.picker_combo_base = None;
            self.picker_combo_search.clear();
            self.picker_step_index = None;
            self.picker_append = false;
        }
        for w in warns {
            ui.colored_label(pal::AMBER, RichText::new(w).size(11.0));
        }

        ui.add_space(4.0);
    }

    /// Floating build/flash modal: phase, progress bar, and readable logs.
    pub(super) fn ui_build_modal(&mut self, ctx: &egui::Context) {
        if !self.build_open {
            return;
        }
        let mut open = true;
        egui::Window::new("⚙ Build & flash")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .max_height(ctx.screen_rect().height() * 0.8)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(520.0);
                // Phase line.
                ui.horizontal(|ui| {
                    if self.build_busy {
                        ui.spinner();
                    }
                    let (icon, col) = match &self.build_result {
                        Some(Ok(_)) => ("✓", pal::GREEN),
                        Some(Err(_)) => ("✗", pal::RED),
                        None => ("", pal::VIOLET_HI),
                    };
                    if !icon.is_empty() {
                        ui.label(RichText::new(icon).strong().color(col));
                    }
                    ui.label(
                        RichText::new(&self.build_phase)
                            .strong()
                            .size(15.0)
                            .color(pal::TEXT),
                    );
                });
                ui.add_space(6.0);
                // Progress bar (animated while running).
                ui.add(
                    egui::ProgressBar::new(self.build_progress.clamp(0.0, 1.0))
                        .desired_height(10.0)
                        .fill(if self.build_result.as_ref().is_some_and(|r| r.is_err()) {
                            pal::RED
                        } else {
                            pal::VIOLET
                        })
                        .animate(self.build_busy),
                );
                ui.add_space(8.0);

                // Result card (success/failure).
                if let Some(res) = self.build_result.clone() {
                    match res {
                        Ok(msg) => {
                            ui.colored_label(pal::GREEN, RichText::new(msg).size(12.5));
                        }
                        Err(e) => {
                            ui.colored_label(
                                pal::RED,
                                RichText::new(format!("Failed: {e}")).size(12.5),
                            );
                        }
                    }
                    ui.add_space(6.0);
                }

                // Bootloader hint during the wait.
                if matches!(self.flash_state, Some(FlashState::WaitingForBootloader)) {
                    ui.colored_label(
                        pal::AMBER,
                        "→ Press the small reset button on the Voyager now (don't unplug it).",
                    );
                    ui.add_space(6.0);
                }

                // Readable, auto-scrolled log.
                egui::CollapsingHeader::new("Logs")
                    .default_open(self.build_result.is_some())
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(pal::BG)
                            .stroke(egui::Stroke::new(1.0, pal::BORDER))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(220.0)
                                    .auto_shrink([false, false])
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        for line in self.build_log.lines() {
                                            let col = if line.contains('✗')
                                                || line.to_lowercase().contains("error")
                                            {
                                                pal::RED
                                            } else if line.contains("[OK]") || line.contains('✓')
                                            {
                                                pal::GREEN
                                            } else if line.contains("Compiling") {
                                                pal::TEXT_DIM
                                            } else {
                                                pal::TEXT_MUTED
                                            };
                                            ui.label(
                                                RichText::new(line)
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(col),
                                            );
                                        }
                                    });
                            });
                        if ui.button("⧉ Copy logs").clicked() {
                            ui.ctx().copy_text(self.build_log.clone());
                        }
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.build_busy {
                        let flash_is_writing = flash_is_writing(self.flash_state.as_ref());
                        if flash_is_writing {
                            ui.add_enabled(false, egui::Button::new("Flashing…"));
                            ui.label(
                                RichText::new(
                                    "Do not unplug the keyboard while firmware is being written.",
                                )
                                .size(11.0)
                                .color(pal::TEXT_DIM),
                            );
                        } else if ui.button("✕ Cancel").clicked() {
                            self.build_cancel.store(true, Ordering::SeqCst);
                            self.flash_cancel.store(true, Ordering::SeqCst);
                            self.build_log.push_str("canceling…\n");
                        }
                    } else if ui
                        .add(
                            egui::Button::new(RichText::new("Close").color(Color32::WHITE))
                                .fill(pal::VIOLET),
                        )
                        .clicked()
                    {
                        self.build_open = false;
                    }
                });
            });
        // Window's own ✕ closes it (only when not busy).
        if !open && !self.build_busy {
            self.build_open = false;
        }
    }

    /// Human name of a layer ("VimLife"), falling back to "Layer n".
    pub(super) fn layer_name(&self, n: u8) -> String {
        self.layer_def(n)
            .and_then(|l| l.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Layer {n}"))
    }

    /// Human description of what a key currently does, using layer NAMES -
    /// e.g. "Hold → VimLife", "Tap 1 · Hold ⇧", "Switch to Numpad".
    pub(super) fn describe_assignment(&self, key: &crate::oryx_api::OryxKey) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(tap) = &key.tap {
            if let Some(n) = tap.layer {
                let name = self.layer_name(n);
                let what = match tap.code.as_deref() {
                    Some("TO") => format!("Switch to {name}"),
                    Some("TG") => format!("Toggle {name}"),
                    Some("TT") => format!("Tap-toggle {name}"),
                    Some("OSL") => format!("One-shot {name}"),
                    Some("DF") => format!("Default {name}"),
                    _ => format!("Momentary {name}"),
                };
                parts.push(what);
            } else if let Some(code) = tap.code.as_deref() {
                match code {
                    "KC_NO" => parts.push("Disabled".to_string()),
                    "KC_TRANSPARENT" | "KC_TRNS" => parts.push("Transparent".to_string()),
                    _ => {
                        let label = legend::action_label(tap);
                        if !label.is_empty() {
                            parts.push(format!("Tap {label}"));
                        }
                    }
                }
            }
        }
        if let Some(hold) = &key.hold {
            if let Some(n) = hold.layer {
                parts.push(format!("Hold → {}", self.layer_name(n)));
            } else if hold.code.is_some() || hold.fallback_kind().is_some() {
                let label = legend::action_label(hold);
                if !label.is_empty() {
                    parts.push(format!("Hold {label}"));
                }
            }
        }
        // Some keys carry a third "tap-hold" action (e.g. a second momentary
        // layer) - surface it so the key's full behavior is visible.
        if let Some(th) = &key.tap_hold {
            if let Some(n) = th.layer {
                let name = self.layer_name(n);
                if !parts.iter().any(|p| p.contains(&name)) {
                    parts.push(format!("2×tap-hold → {name}"));
                }
            }
        }
        if parts.is_empty() {
            "No assignment".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// Prefill the editor (behavior/mod/layer/tap) from the key's CURRENT
    /// assignment, so the inspector reflects reality when a key is selected.
    pub(super) fn sync_editor_from_key(&mut self, layer: u8, i: usize) {
        self.slot_added = [false; 4];
        if let Some(slots) = self.key_dances.get(&(layer, i)) {
            self.edit_slots = slots.clone();
            return;
        }

        let Some(key) = self.editing_key(layer, i) else {
            self.edit_slots = [None, None, None, None];
            return;
        };

        let conv = |a: &Option<crate::oryx_api::KeyAction>| -> Option<String> {
            let a = a.as_ref()?;
            let code = a.qmk_code()?;
            (code != "KC_TRANSPARENT" && code != "KC_TRNS").then_some(code)
        };
        self.edit_slots = [
            conv(&key.tap),
            conv(&key.hold),
            conv(&key.double_tap),
            conv(&key.tap_hold),
        ];
    }

    /// Human chip label for a slot's working keycode ("MO → VimLife", "⇧"…).
    pub(super) fn slot_chip_label(&self, code: &str) -> String {
        for p in ["MO", "OSL", "TO", "TG", "TT", "DF", "LT"] {
            if let Some(rest) = code.strip_prefix(p).and_then(|r| r.strip_prefix('(')) {
                if let Some(inner) = rest.strip_suffix(')') {
                    if let Ok(n) = inner.split(',').next().unwrap_or("").trim().parse::<u8>() {
                        return format!("{p} → {}", self.layer_name(n));
                    }
                }
            }
        }
        legend::keycode_label(code)
    }

    /// Compose the buildable QMK keycode from the slot rows + any warnings
    /// about parts the local build can't express yet.
    pub(super) fn compose_slots(&self) -> (String, Vec<String>) {
        let mut warns = Vec::new();
        // No tap + a hold action = the hold code itself (a plain MO/mod key).
        if self.edit_slots[0].is_none() {
            if let Some(h) = self.edit_slots[1].clone() {
                if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
                    warns.push("double-tap / double-tap-hold → keyjitsu generates a tap dance in the firmware".to_string());
                }
                return (h, warns);
            }
        }
        let tap = self.edit_slots[0]
            .clone()
            .unwrap_or_else(|| "KC_NO".to_string());
        let code = match self.edit_slots[1].as_deref() {
            None => tap.clone(),
            Some(h) => match hold_wrap(h, &tap) {
                Some(c) => c,
                None => {
                    warns.push(format!(
                        "hold: {} isn't a modifier or MO(layer) - skipped in the build",
                        self.slot_chip_label(h)
                    ));
                    tap.clone()
                }
            },
        };
        if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
            warns.push(
                "double-tap / double-tap-hold → keyjitsu generates a tap dance in the firmware"
                    .to_string(),
            );
        }
        (code, warns)
    }

    /// Re-stage after a slot change: plain keys become one keycode, keys with
    /// double-tap / tap+hold become a generated tap dance.
    pub(super) fn stage_slots(&mut self, layer: u8, key: usize) {
        if !self.assignment_editable(layer, key)
            && !self.key_edits.contains_key(&(layer, key))
            && !self.key_dances.contains_key(&(layer, key))
        {
            return;
        }

        // Custom layers persist directly (they have no Oryx source to patch);
        // dances aren't wired for them yet, so use the composed base code.
        if self.is_custom_layer(layer) {
            let (code, _) = self.compose_slots();
            self.set_custom_key(layer, key, &code);
            return;
        }
        if self.edit_slots[2].is_some() || self.edit_slots[3].is_some() {
            self.key_dances
                .insert((layer, key), self.edit_slots.clone());
            self.key_edits.remove(&(layer, key));
            self.save_staged();
            return;
        }
        self.key_dances.remove(&(layer, key));
        let (code, _) = self.compose_slots();
        self.key_edits.insert(
            (layer, key),
            crate::key_action::canonicalize_qmk_code(&code),
        );
        self.save_staged();
    }

    /// Floating Oryx-style keycode picker for the selected key.
    pub(super) fn ui_picker(&mut self, ctx: &egui::Context) {
        if !self.picker_open {
            return;
        }
        let Some(key) = self.selected_key else {
            self.picker_open = false;
            return;
        };
        let view = self.view_layer;
        let mut open = self.picker_open;
        let mut pick: Option<String> = None;

        egui::Window::new(format!("Assign - {}", SLOT_LABELS[self.picker_slot.min(3)]))
            .open(&mut open)
            .default_width(520.0)
            .default_height(420.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("search:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.picker_search)
                            .hint_text("filter all keycodes…")
                            .desired_width(220.0),
                    );
                    if !self.picker_search.is_empty() && ui.button("clear").clicked() {
                        self.picker_search.clear();
                    }
                });
                ui.separator();

                let query = self.picker_search.trim().to_lowercase();
                if query.is_empty() {
                    // Category tabs.
                    let library_tab = keycodes::CATALOG.len();
                    let combo_tab = library_tab + 1;
                    ui.horizontal_wrapped(|ui| {
                        for (idx, cat) in keycodes::CATALOG.iter().enumerate() {
                            ui.selectable_value(&mut self.picker_cat, idx, cat.name);
                        }
                        ui.selectable_value(&mut self.picker_cat, library_tab, "📚 Shortcuts");
                        ui.selectable_value(&mut self.picker_cat, combo_tab, "🛠 Combo");
                    });
                    if self.picker_cat == combo_tab {
                        // Build a shortcut that isn't already in the library:
                        // toggle modifiers, pick one base key, assign the
                        // result. Modifier order has no effect on what the
                        // OS receives (they're held together, not sequenced),
                        // so there's no ordering to get "right" here.
                        ui.separator();
                        ui.label(
                            RichText::new("Hold these, then press the key:")
                                .size(12.0)
                                .color(pal::TEXT_DIM),
                        );
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.picker_combo_mods[0], "Ctrl");
                            ui.checkbox(&mut self.picker_combo_mods[1], "Shift");
                            ui.checkbox(&mut self.picker_combo_mods[2], "Opt/Alt");
                            ui.checkbox(&mut self.picker_combo_mods[3], "Cmd/Win");
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("Base key").size(12.0).color(pal::TEXT_DIM));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.picker_combo_search)
                                .hint_text("search a key…")
                                .desired_width(220.0),
                        );
                        let q = self.picker_combo_search.trim().to_lowercase();
                        if !q.is_empty() {
                            egui::ScrollArea::vertical()
                                .max_height(130.0)
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for cat in keycodes::CATALOG.iter().filter(|c| !c.templated)
                                        {
                                            for k in cat.keys {
                                                if (k.label.to_lowercase().contains(&q)
                                                    || k.code.to_lowercase().contains(&q))
                                                    && ui
                                                        .selectable_label(
                                                            self.picker_combo_base.map(|(c, _)| c)
                                                                == Some(k.code),
                                                            k.label,
                                                        )
                                                        .clicked()
                                                {
                                                    self.picker_combo_base =
                                                        Some((k.code, k.label));
                                                }
                                            }
                                        }
                                    });
                                });
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        let [ctrl, shift, alt, gui] = self.picker_combo_mods;
                        if let Some((base_code, base_label)) = self.picker_combo_base {
                            let preview =
                                crate::shortcuts::compose_label(ctrl, shift, alt, gui, base_label);
                            let code = crate::shortcuts::compose(ctrl, shift, alt, gui, base_code);
                            ui.label(
                                RichText::new(format!("{preview}  →  {code}"))
                                    .monospace()
                                    .color(pal::TEXT),
                            );
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("Assign {preview}"))
                                            .color(Color32::WHITE),
                                    )
                                    .fill(pal::VIOLET),
                                )
                                .clicked()
                            {
                                pick = Some(code);
                            }
                        } else {
                            ui.weak("Search and pick a base key above.");
                        }
                        return;
                    }
                    if self.picker_cat == library_tab {
                        // Browse the shortcut library directly (no typing
                        // needed): grouped the same way as the Cheatsheet,
                        // skipping entries you hid there.
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            if !self.custom_shortcuts.is_empty() {
                                ui.label(RichText::new("Your shortcuts").weak().size(11.0));
                                for c in &self.custom_shortcuts {
                                    shortcut_pick_row(ui, &c.keys, &c.desc, &mut pick);
                                }
                                ui.add_space(6.0);
                            }
                            let mut last_cat = "";
                            for d in crate::shortcuts::builtin() {
                                let id = format!("{}|{}|{}", d.category, d.keys, d.desc);
                                if self.hidden_shortcuts.contains(&id) {
                                    continue;
                                }
                                if d.category != last_cat {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(&d.category).weak().size(11.0));
                                    last_cat = &d.category;
                                }
                                shortcut_pick_row(ui, &d.keys, &d.desc, &mut pick);
                            }
                        });
                        // The library tab has no layer/keycode grid below
                        // it; `pick` (if set) is applied by the shared code
                        // after this closure returns, same as every other tab.
                        return;
                    }
                    let cat = &keycodes::CATALOG[self.picker_cat.min(keycodes::CATALOG.len() - 1)];
                    let templated = cat.templated;
                    let cat_keys = cat.keys;
                    if templated {
                        let names: Vec<String> = (0..self.layer_count().max(1))
                            .map(|n| self.layer_name(n))
                            .collect();
                        if self.picker_layer_arg as usize >= names.len() {
                            self.picker_layer_arg = 0;
                        }
                        ui.horizontal(|ui| {
                            ui.label("target layer:");
                            egui::ComboBox::from_id_salt("picker_target_layer")
                                .selected_text(format!(
                                    "{} · {}",
                                    self.picker_layer_arg,
                                    names
                                        .get(self.picker_layer_arg as usize)
                                        .cloned()
                                        .unwrap_or_default()
                                ))
                                .show_ui(ui, |ui| {
                                    for (i, nm) in names.iter().enumerate() {
                                        ui.selectable_value(
                                            &mut self.picker_layer_arg,
                                            i as u8,
                                            format!("{i} · {nm}"),
                                        );
                                    }
                                });
                        });
                    }
                    ui.separator();
                    let arg = self.picker_layer_arg;
                    let lname = self.layer_name(arg);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        keycode_grid(ui, cat_keys, templated, arg, &lname, &mut pick);
                    });
                } else {
                    // Flat search across every category.
                    let lname = self.layer_name(self.picker_layer_arg);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let arg = self.picker_layer_arg;
                        for cat in keycodes::CATALOG {
                            let hits: Vec<&keycodes::KeyDef> = cat
                                .keys
                                .iter()
                                .filter(|k| {
                                    k.code.to_lowercase().contains(&query)
                                        || k.label.to_lowercase().contains(&query)
                                })
                                .collect();
                            if hits.is_empty() {
                                continue;
                            }
                            ui.label(RichText::new(cat.name).weak().size(11.0));
                            let refs: Vec<keycodes::KeyDef> = hits
                                .iter()
                                .map(|k| keycodes::KeyDef {
                                    code: k.code,
                                    label: k.label,
                                })
                                .collect();
                            keycode_grid(ui, &refs, cat.templated, arg, &lname, &mut pick);
                        }

                        // Shortcut library: the same search also matches your
                        // Cheatsheet entries, so a combo you already have
                        // written down ("Cmd+Shift+4 - screenshot") can be
                        // assigned in one click instead of hand-typing QMK's
                        // modifier syntax. Custom entries first, then the
                        // built-ins (skipping ones you hid from the Cheatsheet).
                        let lib_matches = |c: &str, k: &str, d: &str| {
                            k.to_lowercase().contains(&query)
                                || d.to_lowercase().contains(&query)
                                || c.to_lowercase().contains(&query)
                        };
                        let mut lib_hits: Vec<(&str, &str, &str)> = Vec::new();
                        for c in &self.custom_shortcuts {
                            if lib_matches(&c.category, &c.keys, &c.desc) {
                                lib_hits.push((&c.keys, &c.desc, &c.category));
                            }
                        }
                        for d in crate::shortcuts::builtin() {
                            let id = format!("{}|{}|{}", d.category, d.keys, d.desc);
                            if !self.hidden_shortcuts.contains(&id)
                                && lib_matches(&d.category, &d.keys, &d.desc)
                            {
                                lib_hits.push((&d.keys, &d.desc, &d.category));
                            }
                        }
                        if !lib_hits.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(
                                RichText::new("📚 From your shortcut library")
                                    .weak()
                                    .size(11.0),
                            );
                            for (keys, desc, _cat) in lib_hits.iter().take(12) {
                                shortcut_pick_row(ui, keys, desc, &mut pick);
                            }
                            if lib_hits.len() > 12 {
                                ui.weak(format!(
                                    "+{} more - refine your search",
                                    lib_hits.len() - 12
                                ));
                            }
                        }

                        let custom_code = self.picker_search.trim().to_string();
                        if !custom_code.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            if ui
                                .button(
                                    RichText::new(format!("⚡ Use custom code: {custom_code}"))
                                        .color(pal::VIOLET_HI),
                                )
                                .clicked()
                            {
                                pick = Some(custom_code);
                            }
                        }
                    });
                }
            });

        if let Some(code) = pick {
            // The picked code lands in whichever slot row opened the picker;
            // the buildable keycode is recomposed from all slots. A slot can
            // hold more than one step ("KC_A\nKC_B", tapped in order): append
            // a new one, replace one step in place, or (the common case)
            // replace the whole slot.
            let slot = self.picker_slot.min(3);
            if self.picker_append {
                let existing = self.edit_slots[slot].clone().unwrap_or_default();
                self.edit_slots[slot] = Some(if existing.is_empty() {
                    code
                } else {
                    format!("{existing}\n{code}")
                });
            } else if let Some(step) = self.picker_step_index {
                let mut steps: Vec<String> = self.edit_slots[slot]
                    .as_deref()
                    .unwrap_or_default()
                    .split('\n')
                    .map(str::to_string)
                    .collect();
                if step < steps.len() {
                    steps[step] = code;
                }
                self.edit_slots[slot] = Some(steps.join("\n"));
            } else {
                self.edit_slots[slot] = Some(code);
            }
            self.slot_added[slot] = false;
            self.stage_slots(view, key);
            open = false;
        }
        self.picker_open = open;
    }
}
