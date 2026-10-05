//! Sidebar navigation, profiles and shortcut library.

use super::*;

impl App {
    pub(super) fn save_active_profile_target(&mut self) -> bool {
        let Some(name) = self.active_profile.clone() else {
            return true;
        };
        let Some(state) = self.desired_firmware_state() else {
            self.profile_error =
                Some("cannot save firmware profile without a matching loaded layout".into());
            return false;
        };
        match save_profile(&name, &state) {
            Ok(()) => {
                self.profile_state = Some(state);
                self.profile_error = None;
                self.glow_saved = self.glow_work.clone();
                true
            }
            Err(e) => {
                self.profile_error = Some(format!("could not save firmware profile {name}: {e:#}"));
                false
            }
        }
    }

    fn activate_profile_target(
        &mut self,
        target: Option<String>,
        state: Option<FirmwareState>,
    ) -> bool {
        if let Some(candidate) = state.as_ref() {
            if self.layout.is_some() && !self.profile_matches_layout(candidate) {
                self.profile_error = Some(format!(
                    "profile targets {}/{}, but the loaded keyboard layout is different",
                    candidate.layout_hash, candidate.revision
                ));
                return false;
            }
        }
        let active = target.clone();
        let clear_device_draft = target.is_none().then(|| self.layout_hash.clone()).flatten();
        if let Err(e) = config::update(move |cfg| {
            cfg.active_profile = active;
            if let Some(hash) = clear_device_draft.as_deref() {
                cfg.staged_edits.retain(|entry| entry.layout != hash);
                cfg.staged_dances.retain(|entry| entry.layout != hash);
                cfg.glow_overrides.retain(|entry| entry.layout != hash);
                cfg.glow_draft_layouts.retain(|layout| layout != hash);
                cfg.custom_layers.retain(|layer| layer.layout != hash);
                cfg.custom_layer_sets.retain(|set| set.layout != hash);
            }
        }) {
            self.profile_error = Some(format!("could not activate firmware profile: {e:#}"));
            return false;
        }
        self.active_profile = target;
        self.profile_state = state;
        self.key_edits.clear();
        self.key_dances.clear();
        // Load the complete new target before persisting empty scratch state;
        // otherwise profile autosave could copy glow/custom layers from the
        // previously selected target into the new profile.
        self.apply_firmware_profile_target();
        self.save_staged();
        self.profile_error = None;
        true
    }

    pub(super) fn switch_profile(&mut self, target: Option<String>) {
        if target == self.active_profile {
            return;
        }
        if self.active_profile.is_none() && target.is_some() && self.pending_firmware_count() > 0 {
            self.profile_error = Some(
                "current device draft has pending firmware changes; save it as a firmware profile, flash it, or discard it before switching profiles".into(),
            );
            return;
        }
        if self.active_profile.is_some() && !self.save_active_profile_target() {
            return;
        }
        let state = match target.as_deref() {
            Some(name) => match load_profile(name) {
                Ok(state) => Some(state),
                Err(e) => {
                    self.profile_error =
                        Some(format!("could not load firmware profile {name}: {e:#}"));
                    return;
                }
            },
            None => None,
        };
        self.activate_profile_target(target, state);
    }

    /// Compact saved-layout selector used in the Layout firmware bar.
    pub(super) fn layout_profile_controls(&mut self, ui: &mut egui::Ui) {
        let active = self.active_profile.clone();
        let active_label = active.clone().unwrap_or_else(|| {
            if self.pending_firmware_count() > 0 {
                "current + draft".into()
            } else {
                "current".into()
            }
        });
        let saved_profiles = match list_profiles() {
            Ok(saved) => saved,
            Err(e) => {
                self.profile_error = Some(format!("could not list saved layouts: {e:#}"));
                Vec::new()
            }
        };
        let mut switch: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("layout_profile_sel")
            .width(145.0)
            .selected_text(RichText::new(format!("Layout · {active_label}")).size(11.5))
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(active.is_none(), "current device")
                    .on_hover_text("Use the firmware state currently confirmed on the keyboard.")
                    .clicked()
                    && active.is_some()
                {
                    switch = Some(None);
                }
                for name in &saved_profiles {
                    let selected = active.as_deref() == Some(name.as_str());
                    if ui
                        .selectable_label(selected, name)
                        .on_hover_text(
                            "Load this saved layout target. Flash to apply it to the keyboard.",
                        )
                        .clicked()
                        && !selected
                    {
                        switch = Some(Some(name.clone()));
                    }
                }
            });
        if let Some(target) = switch {
            self.switch_profile(target);
        }
        ui.menu_button("Manage", |ui| {
            if ui.button("Save current layout…").clicked() {
                self.prof_new_open = true;
                self.profile_draft.clear();
                ui.close();
            }
            if ui.button(format!("Duplicate '{active_label}'")).clicked() {
                let result = self
                    .desired_firmware_state()
                    .ok_or_else(|| anyhow!("no matching layout target to duplicate"))
                    .and_then(|state| {
                        next_profile_copy_name(&active_label).and_then(|clone| {
                            create_profile(&clone, &state).map(|_| (clone, state))
                        })
                    });
                match result {
                    Ok((clone, state)) => {
                        self.activate_profile_target(Some(clone), Some(state));
                    }
                    Err(e) => {
                        self.profile_error =
                            Some(format!("could not duplicate saved layout: {e:#}"));
                    }
                }
                ui.close();
            }
            if active.is_some() && ui.button(format!("Delete '{active_label}'")).clicked() {
                let deleted = active_label.clone();
                self.switch_profile(None);
                if self.active_profile.is_none() {
                    match profile_path(&deleted) {
                        Ok(path) => {
                            if let Err(e) = std::fs::remove_file(path) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    self.profile_error =
                                        Some(format!("could not delete {deleted}: {e}"));
                                }
                            }
                        }
                        Err(e) => {
                            self.profile_error = Some(format!("could not delete {deleted}: {e:#}"));
                        }
                    }
                }
                ui.close();
            }
        });
    }

    pub(super) fn layout_profile_editor(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.profile_error {
            ui.colored_label(pal::RED, RichText::new(e).size(10.5));
        }
        if !self.prof_new_open {
            return;
        }
        let saved_profiles = list_profiles().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.weak("Save layout as");
            ui.add(
                egui::TextEdit::singleline(&mut self.profile_draft)
                    .hint_text("name…")
                    .desired_width(150.0),
            );
            let draft = self.profile_draft.trim();
            let ok = profile_file_name(draft).is_ok()
                && !saved_profiles
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(draft));
            if ui
                .add_enabled(ok, egui::Button::new("Save"))
                .on_disabled_hover_text("Use 1-64 letters, numbers, spaces, '-' or '_'.")
                .clicked()
            {
                let name = self.profile_draft.trim().to_string();
                let result = self
                    .desired_firmware_state()
                    .ok_or_else(|| anyhow!("no matching layout target to save"))
                    .and_then(|state| create_profile(&name, &state).map(|_| state));
                match result {
                    Ok(state) => {
                        if self.activate_profile_target(Some(name), Some(state)) {
                            self.prof_new_open = false;
                            self.profile_draft.clear();
                        }
                    }
                    Err(e) => {
                        self.profile_error = Some(format!("could not save layout: {e:#}"));
                    }
                }
            }
            if ui.button("Cancel").clicked() {
                self.prof_new_open = false;
            }
        });
    }

    /// Task-local navigation under the active top-level destination.
    pub(super) fn nav_children(&mut self, ui: &mut egui::Ui, tab: Tab) {
        let names: Vec<String> = (0..self.layer_count())
            .map(|n| self.layer_name(n))
            .collect();
        let active = self.active_layer;
        match tab {
            Tab::Live => {
                let has_layout = self.layout.is_some();
                let oryx = self.oryx_layer_count();
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    let label = if has_layout && n >= oryx {
                        format!("★ {name}")
                    } else {
                        name.clone()
                    };
                    if sub_item(ui, self.view_layer == n, n == active, &label) {
                        self.view_layer = n;
                        self.follow = false;
                    }
                }

                if self.view_layer < self.layer_count() && self.layer_count() > 1 {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        if self.view_layer >= oryx && ui.small_button("rename").clicked() {
                            self.rename_layer_target = Some(self.view_layer);
                            self.rename_layer_name = self.layer_name(self.view_layer);
                            self.delete_layer_confirm = None;
                        }
                        let deleting = self.delete_layer_confirm == Some(self.view_layer);
                        if ui
                            .small_button(if deleting { "confirm remove" } else { "remove" })
                            .on_hover_text(
                                "Remove this layer from the current firmware draft and renumber layer references. The Oryx source is not modified.",
                            )
                            .clicked()
                        {
                            if deleting {
                                let deleted = self.view_layer;
                                self.remove_layer(deleted);
                                self.delete_layer_confirm = None;
                                self.rename_layer_target = None;
                            } else {
                                self.delete_layer_confirm = Some(self.view_layer);
                            }
                        }
                    });
                }
                if let Some(target) = self.rename_layer_target {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.rename_layer_name)
                                .desired_width(105.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        let ok = !self.rename_layer_name.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                            self.rename_custom_layer(
                                target,
                                self.rename_layer_name.trim().to_string(),
                            );
                            self.rename_layer_target = None;
                            self.rename_layer_name.clear();
                        }
                        if ui.small_button("cancel").clicked() {
                            self.rename_layer_target = None;
                            self.rename_layer_name.clear();
                        }
                    });
                }

                // Layer creation belongs to Layout: choose a blank layer or
                // duplicate an existing effective layer, then name the result.
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.menu_button("＋ Add layer", |ui| {
                        if ui.button("Blank layer").clicked() {
                            self.new_layer_open = true;
                            self.new_layer_source = None;
                            self.new_layer_name = format!("Layer {}", self.layer_count());
                            ui.close();
                        }
                        if !names.is_empty() {
                            ui.separator();
                            ui.weak("Duplicate from");
                            for (n, name) in names.iter().enumerate() {
                                if ui.button(name).clicked() {
                                    self.new_layer_open = true;
                                    self.new_layer_source = Some(n as u8);
                                    self.new_layer_name = format!("{} copy", name);
                                    ui.close();
                                }
                            }
                        }
                    });
                });
                if self.new_layer_open {
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_layer_name)
                                .hint_text("layer name")
                                .desired_width(105.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
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
                        if ui.small_button("cancel").clicked() {
                            self.new_layer_open = false;
                            self.new_layer_name.clear();
                            self.new_layer_source = None;
                        }
                    });
                }

                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    let mut f = self.follow;
                    if toggle(ui, &mut f)
                        .on_hover_text("view follows the keyboard's active layer")
                        .changed()
                    {
                        self.follow = f;
                        if f {
                            self.view_layer = self.active_layer;
                        }
                    }
                    ui.label(
                        RichText::new("follow board")
                            .size(11.5)
                            .color(pal::TEXT_DIM),
                    );
                });
            }
            Tab::Heatmap => {
                if sub_item(ui, self.heat_layer.is_none(), false, "all layers") {
                    self.heat_layer = None;
                }
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    if sub_item(ui, self.heat_layer == Some(n), n == active, name) {
                        self.heat_layer = Some(n);
                    }
                }
            }
            Tab::Peek => {
                if sub_item(ui, self.minimap_edit_global, false, "Global · all layers") {
                    self.minimap_edit_global = true;
                    if self.minimap_global_enabled {
                        let c = self.minimap_global.settings.clone();
                        self.arm_minimap_overlay(&c, 2000);
                    }
                }
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    if sub_item(
                        ui,
                        !self.minimap_edit_global && self.peek_layer == n,
                        n == active,
                        name,
                    ) {
                        self.minimap_edit_global = false;
                        self.peek_layer = n;
                        if !self.minimap_global_enabled {
                            let c = self.minimap_settings(n);
                            self.arm_minimap_overlay(&c, 2000);
                        }
                    }
                }
            }
            Tab::Fx | Tab::Perf | Tab::Auto | Tab::Tools => {}
        }
        ui.add_space(4.0);
    }

    pub(super) fn save_custom_shortcuts(&mut self) {
        let shortcuts = self.custom_shortcuts.clone();
        self.persist_config("saving shortcuts", move |cfg| {
            cfg.custom_shortcuts = shortcuts;
        });
    }

    fn add_imported_shortcuts(&mut self, found: Vec<crate::shortcuts::ShortcutDef>) -> usize {
        let mut added = 0usize;
        for d in found {
            let builtin_duplicate = crate::shortcuts::builtin()
                .iter()
                .any(|b| b.category == d.category && b.keys == d.keys);
            let custom_duplicate = self
                .custom_shortcuts
                .iter()
                .any(|c| c.category == d.category && c.keys == d.keys);
            if builtin_duplicate || custom_duplicate {
                continue;
            }
            self.custom_shortcuts.push(config::CustomShortcut {
                category: d.category,
                keys: d.keys,
                desc: d.desc,
                high: d.high,
            });
            added += 1;
        }
        if added > 0 {
            self.save_custom_shortcuts();
        }
        added
    }

    pub(super) fn import_terminal_shortcuts_into_library(&mut self) {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            self.shortcut_import_status = Some("HOME is unavailable".into());
            return;
        };
        let sources = crate::shortcuts::detect_terminal_configs(&home);
        if sources.is_empty() {
            self.shortcut_import_status =
                Some("No supported terminal config detected. Try a custom path.".into());
            self.shortcut_import_custom_open = true;
            return;
        }

        let found = crate::shortcuts::import_terminal_shortcuts(&home);
        let imported = found.len();
        let added = self.add_imported_shortcuts(found);
        let names = sources
            .iter()
            .map(|s| s.terminal)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" + ");
        self.shortcut_import_status = Some(if added > 0 {
            format!("Detected {names}: imported {added} new shortcuts ({imported} parsed)")
        } else {
            format!("Detected {names}: everything parsed is already in the library")
        });
    }

    pub(super) fn import_terminal_shortcuts_custom(&mut self) {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            self.shortcut_import_status = Some("HOME is unavailable".into());
            return;
        };
        let path = crate::shortcuts::expand_user_path(&home, &self.shortcut_import_path);
        match crate::shortcuts::import_terminal_shortcuts_from_path(&path) {
            Ok(found) => {
                let parsed = found.len();
                let added = self.add_imported_shortcuts(found);
                self.shortcut_import_status = Some(format!(
                    "Custom config: imported {added} new shortcuts ({parsed} parsed)"
                ));
            }
            Err(e) => self.shortcut_import_status = Some(e),
        }
    }

    /// The Shortcuts tab: a searchable cheatsheet of ready-made shortcuts
    /// (category chips in the panel) plus the user's own entries.
    pub(super) fn ui_shortcuts(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("🔎").size(14.0));
            ui.add(
                egui::TextEdit::singleline(&mut self.keys_search)
                    .hint_text("search the cheatsheet…")
                    .desired_width(240.0),
            );
            if !self.keys_search.is_empty() && ui.button("✕").clicked() {
                self.keys_search.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("⇩ import terminal shortcuts")
                    .on_hover_text("Auto-detect Ghostty/Kitty in their standard config locations")
                    .clicked()
                {
                    self.import_terminal_shortcuts_into_library();
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("＋ add shortcut").color(Color32::WHITE))
                            .fill(pal::VIOLET),
                    )
                    .clicked()
                {
                    self.keys_adding = true;
                    self.draft_sc.category = self.keys_cat.clone().unwrap_or_default();
                }
                if !self.hidden_shortcuts.is_empty()
                    && ui
                        .button(
                            RichText::new(format!(
                                "↺ restore {} hidden",
                                self.hidden_shortcuts.len()
                            ))
                            .size(11.5),
                        )
                        .clicked()
                {
                    self.hidden_shortcuts.clear();
                    self.persist_config("restoring hidden shortcuts", |cfg| {
                        cfg.hidden_shortcuts.clear();
                    });
                }
            });
        });
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            let sources = crate::shortcuts::detect_terminal_configs(&home);
            if !sources.is_empty() {
                let detected = sources
                    .iter()
                    .map(|s| {
                        let shown = s
                            .path
                            .strip_prefix(&home)
                            .map(|p| format!("~/{}", p.display()))
                            .unwrap_or_else(|_| s.path.display().to_string());
                        format!("{} · {shown}", s.terminal)
                    })
                    .collect::<Vec<_>>()
                    .join("   ");
                ui.label(
                    RichText::new(format!("Detected: {detected}"))
                        .size(10.5)
                        .color(pal::TEXT_DIM),
                );
            }
        }
        if let Some(status) = &self.shortcut_import_status {
            ui.label(RichText::new(status).size(11.0).color(pal::TEXT_DIM));
        }
        ui.horizontal(|ui| {
            if ui
                .small_button(if self.shortcut_import_custom_open {
                    "hide custom path"
                } else {
                    "custom path…"
                })
                .clicked()
            {
                self.shortcut_import_custom_open = !self.shortcut_import_custom_open;
            }
            if self.shortcut_import_custom_open {
                ui.add(
                    egui::TextEdit::singleline(&mut self.shortcut_import_path)
                        .hint_text("~/path/to/config")
                        .desired_width(300.0),
                );
                if ui
                    .add_enabled(
                        !self.shortcut_import_path.trim().is_empty(),
                        egui::Button::new("import file"),
                    )
                    .clicked()
                {
                    self.import_terminal_shortcuts_custom();
                }
            }
        });
        ui.add_space(6.0);

        // Category chips (all + every category present).
        let mut cats: Vec<String> = Vec::new();
        for d in crate::shortcuts::builtin() {
            if !cats.contains(&d.category) {
                cats.push(d.category.clone());
            }
        }
        for c in &self.custom_shortcuts {
            if !cats.contains(&c.category) && !c.category.is_empty() {
                cats.push(c.category.clone());
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(self.keys_cat.is_none(), "all")
                .clicked()
            {
                self.keys_cat = None;
            }
            for cat in &cats {
                if ui
                    .selectable_label(self.keys_cat.as_deref() == Some(cat.as_str()), cat)
                    .clicked()
                {
                    self.keys_cat = Some(cat.clone());
                }
            }
        });
        ui.add_space(6.0);

        // Add/edit form.
        if self.keys_adding {
            card(ui, "New shortcut", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("category").size(11.5).color(pal::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft_sc.category)
                            .hint_text("e.g. Burp")
                            .desired_width(140.0),
                    );
                    ui.label(RichText::new("keys").size(11.5).color(pal::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft_sc.keys)
                            .hint_text("Cmd + Shift + X")
                            .desired_width(170.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("does").size(11.5).color(pal::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft_sc.desc)
                            .hint_text("what it does")
                            .desired_width(340.0),
                    );
                    ui.checkbox(&mut self.draft_sc.high, "essential");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let ok = !self.draft_sc.category.trim().is_empty()
                        && !self.draft_sc.keys.trim().is_empty();
                    if ui
                        .add_enabled(
                            ok,
                            egui::Button::new(RichText::new("Save").color(Color32::WHITE))
                                .fill(pal::VIOLET),
                        )
                        .clicked()
                    {
                        self.custom_shortcuts.push(self.draft_sc.clone());
                        self.save_custom_shortcuts();
                        self.keys_adding = false;
                        self.draft_sc = config::CustomShortcut {
                            category: String::new(),
                            keys: String::new(),
                            desc: String::new(),
                            high: true,
                        };
                    }
                    if ui.button("Cancel").clicked() {
                        self.keys_adding = false;
                    }
                });
            });
            ui.add_space(6.0);
        }

        // Collect the visible rows: (category, keys, desc, high, custom-index).
        let q = self.keys_search.to_lowercase();
        let cat = self.keys_cat.clone();
        let matches = |c: &str, k: &str, d: &str| {
            (cat.as_deref().is_none_or(|w| w == c))
                && (q.is_empty()
                    || k.to_lowercase().contains(&q)
                    || d.to_lowercase().contains(&q)
                    || c.to_lowercase().contains(&q))
        };
        let mut rows: Vec<(String, String, String, bool, Option<usize>)> = Vec::new();
        for (i, c) in self.custom_shortcuts.iter().enumerate() {
            if matches(&c.category, &c.keys, &c.desc) {
                rows.push((
                    c.category.clone(),
                    c.keys.clone(),
                    c.desc.clone(),
                    c.high,
                    Some(i),
                ));
            }
        }
        for d in crate::shortcuts::builtin() {
            let id = format!("{}|{}|{}", d.category, d.keys, d.desc);
            if !self.hidden_shortcuts.contains(&id) && matches(&d.category, &d.keys, &d.desc) {
                rows.push((
                    d.category.clone(),
                    d.keys.clone(),
                    d.desc.clone(),
                    d.high,
                    None,
                ));
            }
        }

        ui.label(
            RichText::new(format!("{} shortcuts", rows.len()))
                .size(11.0)
                .color(pal::TEXT_DIM),
        );
        ui.add_space(4.0);

        let mut delete: Option<usize> = None;
        let mut hide: Option<String> = None;
        let mut last_cat = String::new();
        for (rcat, keys, desc, high, custom_i) in &rows {
            // Group header when browsing "all".
            if cat.is_none() && *rcat != last_cat {
                last_cat = rcat.clone();
                ui.add_space(8.0);
                ui.label(
                    RichText::new(rcat.to_uppercase())
                        .size(10.5)
                        .strong()
                        .color(pal::VIOLET_HI),
                );
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                // Key chip.
                egui::Frame::new()
                    .fill(pal::INPUT)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(keys).monospace().size(12.0).color(pal::TEXT));
                    });
                ui.add_space(6.0);
                ui.label(RichText::new(desc).size(12.5).color(pal::TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match custom_i {
                        Some(i) => {
                            if ui
                                .button(RichText::new("✕").size(10.0))
                                .on_hover_text("delete your shortcut")
                                .clicked()
                            {
                                delete = Some(*i);
                            }
                            ui.label(RichText::new("yours").size(10.0).color(pal::CYAN));
                        }
                        None => {
                            if ui
                                .button(RichText::new("✕").size(10.0))
                                .on_hover_text("hide this shortcut")
                                .clicked()
                            {
                                hide = Some(format!("{rcat}|{keys}|{desc}"));
                            }
                        }
                    }
                    if *high {
                        ui.label(RichText::new("●").size(9.0).color(pal::VIOLET));
                    }
                });
            });
        }
        if let Some(i) = delete {
            self.custom_shortcuts.remove(i);
            self.save_custom_shortcuts();
        }
        if let Some(id) = hide {
            self.hidden_shortcuts.push(id);
            let hidden = self.hidden_shortcuts.clone();
            self.persist_config("hiding shortcut", move |cfg| {
                cfg.hidden_shortcuts = hidden;
            });
        }
        ui.add_space(10.0);
    }
}
