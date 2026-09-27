//! Sidebar navigation, profiles and shortcut library.

use super::*;

impl App {
    pub(super) fn switch_profile(&mut self, target: Option<String>) {
        let current = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".into());
        if let Err(e) = snapshot_profile(&current) {
            self.profile_error = Some(format!("could not save {current}: {e:#}"));
            return;
        }

        let target_name = target.clone().unwrap_or_else(|| "default".into());
        let profile = match load_profile(&target_name) {
            Ok(profile) => profile,
            Err(e) => {
                self.profile_error = Some(format!("could not load {target_name}: {e:#}"));
                return;
            }
        };

        let profile_for_config = profile.clone();
        let active_profile = target.clone();
        if let Err(e) = config::update(move |cfg| {
            profile_for_config.apply_to(cfg);
            cfg.active_profile = active_profile;
        }) {
            self.profile_error = Some(format!("could not activate {target_name}: {e:#}"));
            return;
        }

        self.active_profile = target;
        self.profile_error = None;
        self.apply_profile(&profile);
    }

    /// Sidebar profile switcher: default + saved profiles, with new/clone/
    /// delete actions. Switching snapshots the active profile first.
    pub(super) fn profile_bar(&mut self, ui: &mut egui::Ui) {
        let active = self.active_profile.clone();
        let active_label = active.clone().unwrap_or_else(|| "default".into());
        let saved_profiles = match list_profiles() {
            Ok(saved) => saved,
            Err(e) => {
                self.profile_error = Some(format!("could not list profiles: {e:#}"));
                Vec::new()
            }
        };
        ui.horizontal(|ui| {
            let mut switch: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("profile_sel")
                .width(112.0)
                .selected_text(RichText::new(format!("💾 {active_label}")).size(11.5))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(active.is_none(), "default").clicked()
                        && active.is_some()
                    {
                        switch = Some(None);
                    }
                    for name in &saved_profiles {
                        if name == "default" {
                            continue;
                        }
                        let is = active.as_deref() == Some(name.as_str());
                        if ui.selectable_label(is, name).clicked() && !is {
                            switch = Some(Some(name.clone()));
                        }
                    }
                });
            if let Some(t) = switch {
                self.switch_profile(t);
            }
            ui.menu_button(RichText::new("＋").size(12.0), |ui| {
                if ui.button("New profile from current…").clicked() {
                    self.prof_new_open = true;
                    self.profile_draft.clear();
                    ui.close();
                }
                if ui.button(format!("Clone '{active_label}'")).clicked() {
                    let current = self
                        .active_profile
                        .clone()
                        .unwrap_or_else(|| "default".into());
                    let result = next_profile_copy_name(&active_label).and_then(|clone| {
                        snapshot_profile(&current).and_then(|_| create_profile(&clone))
                    });
                    self.profile_error = result
                        .err()
                        .map(|e| format!("could not clone profile: {e:#}"));
                    ui.close();
                }
                if active.is_some() && ui.button(format!("🗑 Delete '{active_label}'")).clicked()
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
                let ok = !draft.eq_ignore_ascii_case("default")
                    && profile_file_name(draft).is_ok()
                    && !saved_profiles
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(draft));
                if ui
                    .add_enabled(ok, egui::Button::new("✓"))
                    .on_disabled_hover_text(
                        "Use 1-64 letters, numbers, spaces, '-' or '_'; 'default' is reserved.",
                    )
                    .clicked()
                {
                    let name = self.profile_draft.trim().to_string();
                    let current = self
                        .active_profile
                        .clone()
                        .unwrap_or_else(|| "default".into());
                    let result = snapshot_profile(&current)
                        .and_then(|_| create_profile(&name))
                        .and_then(|_| {
                            let active = name.clone();
                            config::update(move |cfg| cfg.active_profile = Some(active))
                        });
                    match result {
                        Ok(()) => {
                            self.active_profile = Some(name);
                            self.profile_error = None;
                            self.prof_new_open = false;
                            self.profile_draft.clear();
                        }
                        Err(e) => {
                            self.profile_error = Some(format!("could not create profile: {e:#}"))
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
                        let c = self.peek.clone();
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
