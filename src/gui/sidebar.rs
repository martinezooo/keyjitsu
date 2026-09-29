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

    /// Firmware target selector. "device" means the currently confirmed
    /// firmware state; a named profile is a draft target and must be flashed to
    /// become device truth.
    pub(super) fn profile_bar(&mut self, ui: &mut egui::Ui) {
        let active = self.active_profile.clone();
        let active_label = active.clone().unwrap_or_else(|| {
            if self.pending_firmware_count() > 0 {
                "device + draft".into()
            } else {
                "device".into()
            }
        });
        let saved_profiles = match list_profiles() {
            Ok(saved) => saved,
            Err(e) => {
                self.profile_error = Some(format!("could not list firmware profiles: {e:#}"));
                Vec::new()
            }
        };
        ui.horizontal(|ui| {
            let mut switch: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("profile_sel")
                .width(112.0)
                .selected_text(RichText::new(format!("⚙ {active_label}")).size(11.5))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(active.is_none(), "device (current)")
                        .on_hover_text("Use the firmware state currently confirmed on the keyboard as the target.")
                        .clicked()
                        && active.is_some()
                    {
                        switch = Some(None);
                    }
                    for name in &saved_profiles {
                        let is = active.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(is, name)
                            .on_hover_text("Firmware target; Build & flash is required to apply it to the keyboard.")
                            .clicked()
                            && !is
                        {
                            switch = Some(Some(name.clone()));
                        }
                    }
                });
            if let Some(t) = switch {
                self.switch_profile(t);
            }
            ui.menu_button(RichText::new("＋").size(12.0), |ui| {
                if ui.button("New firmware profile from current target…").clicked() {
                    self.prof_new_open = true;
                    self.profile_draft.clear();
                    ui.close();
                }
                if ui.button(format!("Clone '{active_label}'")).clicked() {
                    let result = self
                        .desired_firmware_state()
                        .ok_or_else(|| anyhow!("no matching firmware target to clone"))
                        .and_then(|state| {
                            next_profile_copy_name(&active_label)
                                .and_then(|clone| create_profile(&clone, &state).map(|_| (clone, state)))
                        });
                    match result {
                        Ok((clone, state)) => {
                            self.activate_profile_target(Some(clone), Some(state));
                        }
                        Err(e) => {
                            self.profile_error = Some(format!("could not clone firmware profile: {e:#}"));
                        }
                    }
                    ui.close();
                }
                if active.is_some()
                    && ui.button(format!("🗑 Delete '{active_label}'")).clicked()
                {
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
                                self.profile_error =
                                    Some(format!("could not delete {deleted}: {e:#}"));
                            }
                        }
                    }
                    ui.close();
                }
            });
        });
        if self.active_profile.is_some() {
            ui.label(
                RichText::new("profile = firmware target · flash to apply")
                    .size(9.5)
                    .color(pal::TEXT_DIM),
            );
        }
        if let Some(e) = &self.profile_error {
            ui.colored_label(pal::RED, RichText::new(e).size(10.5));
        }
        if self.prof_new_open {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.profile_draft)
                        .hint_text("name…")
                        .desired_width(96.0),
                );
                let draft = self.profile_draft.trim();
                let ok = profile_file_name(draft).is_ok()
                    && !saved_profiles
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(draft));
                if ui
                    .add_enabled(ok, egui::Button::new("✓"))
                    .on_disabled_hover_text("Use 1-64 letters, numbers, spaces, '-' or '_'.")
                    .clicked()
                {
                    let name = self.profile_draft.trim().to_string();
                    let result = self
                        .desired_firmware_state()
                        .ok_or_else(|| anyhow!("no matching firmware target to save"))
                        .and_then(|state| create_profile(&name, &state).map(|_| state));
                    match result {
                        Ok(state) => {
                            if self.activate_profile_target(Some(name), Some(state)) {
                                self.prof_new_open = false;
                                self.profile_draft.clear();
                            }
                        }
                        Err(e) => {
                            self.profile_error =
                                Some(format!("could not create firmware profile: {e:#}"));
                        }
                    }
                }
                if ui.button("✕").clicked() {
                    self.prof_new_open = false;
                }
            });
        }
    }

    /// Sub-items rendered in the sidebar under the ACTIVE tab: layers for
    /// Live/Heatmap/Peek, library categories for FX Studio.
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
                    // Custom layers get a ★ marker (only meaningful with a layout).
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
                // Authoring (add/rename/delete) lives in the Layers tab.
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
                ui.add_space(2.0);
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
                for (n, name) in names.iter().enumerate() {
                    let n = n as u8;
                    if sub_item(ui, self.peek_layer == n, n == active, name) {
                        self.peek_layer = n;
                        let c = self.minimap_settings(n);
                        self.arm_preview(&c, 2000);
                    }
                }
            }
            Tab::Fx => {
                for (lib, label) in [
                    (FxLib::Const, "constant"),
                    (FxLib::Press, "on press"),
                    (FxLib::Custom, "custom"),
                    (FxLib::Apply, "▶ board RGB"),
                ] {
                    if sub_item(ui, self.fx_lib == lib, false, label) {
                        self.fx_lib = lib;
                    }
                }
            }
            Tab::Layers | Tab::Perf | Tab::Auto | Tab::Tools => {}
        }
        ui.add_space(4.0);
    }

    pub(super) fn save_custom_shortcuts(&mut self) {
        let shortcuts = self.custom_shortcuts.clone();
        self.persist_config("saving shortcuts", move |cfg| {
            cfg.custom_shortcuts = shortcuts;
        });
    }

    pub(super) fn import_terminal_shortcuts_into_library(&mut self) {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            self.shortcut_import_status = Some("HOME is unavailable".into());
            return;
        };
        let found = crate::shortcuts::import_terminal_shortcuts(&home);
        let mut added = 0usize;
        for d in found {
            let duplicate = self
                .custom_shortcuts
                .iter()
                .any(|c| c.category == d.category && c.keys == d.keys && c.desc == d.desc);
            if !duplicate {
                self.custom_shortcuts.push(config::CustomShortcut {
                    category: d.category,
                    keys: d.keys,
                    desc: d.desc,
                    high: d.high,
                });
                added += 1;
            }
        }
        if added > 0 {
            self.save_custom_shortcuts();
            self.shortcut_import_status = Some(format!("Imported {added} terminal shortcuts"));
        } else {
            self.shortcut_import_status =
                Some("No new Ghostty/Kitty keybinds found in default config paths".into());
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
                    .button("⇩ import terminal keybinds")
                    .on_hover_text(
                        "Import simple keybinds from Ghostty or Kitty default config paths",
                    )
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
        if let Some(status) = &self.shortcut_import_status {
            ui.label(RichText::new(status).size(11.0).color(pal::TEXT_DIM));
        }
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
