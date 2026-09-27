//! Heatmap page and export logic.

use super::*;

impl App {
    pub(super) fn ui_heatmap(&mut self, ui: &mut egui::Ui, avail_h: f32) {
        let key_count = self.geometry().len();
        if self.heat.is_none() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if let Some(e) = &self.heat_error {
                    ui.colored_label(pal::RED, "Heatmap data could not be loaded.");
                    ui.label(RichText::new(e).size(11.0).color(pal::TEXT_DIM));
                } else {
                    ui.label("Connect the keyboard to collect statistics.");
                }
            });
            return;
        }
        let Some(heat) = self.heat.as_ref() else {
            return;
        };
        let (counts, total) = (
            heat.counts(self.heat_layer, key_count),
            heat.total_presses(),
        );
        let norm = normalize(&counts);
        let layer_total: u64 = counts.iter().sum();
        let mut ranked: Vec<(usize, u64)> = counts
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, c)| *c > 0)
            .collect();
        ranked.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        let layer = self.heat_layer.unwrap_or(self.view_layer);
        let top_label = ranked.first().map(|(idx, _)| {
            self.device_key(layer, *idx)
                .map(|k| labels_for(&k).tap)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("key {idx}"))
        });

        // --- Stats summary card --------------------------------------------
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(pal::CARD)
            .stroke(egui::Stroke::new(1.0, pal::BORDER))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let stat = |ui: &mut egui::Ui, value: String, label: &str| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(value).strong().size(20.0).color(pal::TEXT));
                            ui.label(RichText::new(label).size(11.0).color(pal::TEXT_DIM));
                        });
                        ui.add_space(26.0);
                    };
                    stat(ui, format_thousands(total), "total presses");
                    stat(ui, top_label.unwrap_or_else(|| "-".into()), "most used key");
                    // Scope (all layers / per layer) is picked in the sidebar.
                    stat(
                        ui,
                        match self.heat_layer {
                            None => "all layers".to_string(),
                            Some(n) => self.layer_name(n),
                        },
                        "scope",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.confirm_reset {
                            if ui
                                .button(RichText::new("Really delete?").color(pal::RED))
                                .clicked()
                            {
                                if let Some((_, serial)) = &self.connected {
                                    if let Ok(id) = LayoutId::from_serial(serial) {
                                        match HeatmapStore::reset(&id.hash) {
                                            Ok(_) => self.hydrate_heatmap(&id.hash, key_count),
                                            Err(e) => {
                                                self.heat_error =
                                                    Some(format!("resetting heatmap: {e:#}"));
                                            }
                                        }
                                    }
                                }
                                self.confirm_reset = false;
                            }
                            if ui.button("Keep").clicked() {
                                self.confirm_reset = false;
                            }
                        } else if ui.button("Reset stats").clicked() {
                            self.confirm_reset = true;
                        }
                        if ui.button("⬇ Export CSV").clicked() {
                            match self.export_heatmap_csv(&counts, layer_total) {
                                Ok(path) => {
                                    self.csv_saved = Some(path);
                                    self.heat_error = None;
                                }
                                Err(e) => {
                                    self.csv_saved = None;
                                    self.heat_error = Some(format!("exporting heatmap CSV: {e:#}"));
                                }
                            }
                        }
                    });
                });
                if let Some(p) = &self.csv_saved {
                    ui.horizontal(|ui| {
                        ui.colored_label(pal::GREEN, "✓ saved:");
                        if ui
                            .link(
                                RichText::new(p.display().to_string())
                                    .size(11.5)
                                    .monospace(),
                            )
                            .clicked()
                        {
                            if let Err(e) = crate::platform::reveal_path(p) {
                                self.heat_error =
                                    Some(format!("opening exported heatmap location: {e:#}"));
                            }
                        }
                    });
                }
            });
        ui.add_space(10.0);

        // --- Canvas (left) + ranking card (right) ---------------------------
        // Both columns share the height that's actually left below the stats
        // card, so nothing runs past the window edge.
        let budget = (avail_h - 118.0).max(200.0);
        let full = ui.available_width();
        let right_w = 250.0f32.min(full * 0.3);
        let left_w = full - right_w - 14.0;
        let (g_cols, g_rows) = self.board_units();
        // Board width capped by the height budget (34px/unit legibility floor).
        let board_cap = (((budget - 76.0) / g_rows).clamp(34.0, 62.0) * g_cols + 48.0).min(left_w);
        let rank_rows = (((budget - 84.0) / 27.0) as usize).clamp(5, 20);
        let device_layer = self.device_layer(layer);
        let layer_def = device_layer.as_ref();
        let combo_keys = self.combo_member_mask(layer);
        let glow: Vec<Option<Color32>> = norm
            .iter()
            .map(|&t| (t > 0.0).then(|| widget::heat_color(t)))
            .collect();

        ui.horizontal_top(|ui| {
            // Keyboard canvas card + legend (centered, height-budgeted).
            ui.vertical(|ui| {
                ui.set_width(left_w);
                let pad = ((left_w - board_cap) / 2.0).max(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    egui::Frame::new()
                        .fill(pal::CARD)
                        .stroke(egui::Stroke::new(1.0, pal::BORDER))
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(14, 16))
                        .show(ui, |ui| {
                            // The frame sits in a horizontal wrapper (for the
                            // centering pad) - lay its content out vertically.
                            ui.vertical(|ui| {
                                ui.set_width(board_cap - 28.0);
                                let no_press = vec![false; key_count];
                                let kb = draw_keyboard(
                                    ui,
                                    self.geometry(),
                                    layer_def,
                                    &glow,
                                    &no_press,
                                    None,
                                    Some(&combo_keys),
                                    1.0,
                                    false,
                                );
                                if let Some(i) = kb.hovered {
                                    let label = layer_def
                                        .and_then(|l| l.keys.get(i))
                                        .map(|k| labels_for(k).tap)
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or_else(|| format!("key {i}"));
                                    let c = counts.get(i).copied().unwrap_or(0);
                                    let pct = c as f64 / layer_total.max(1) as f64 * 100.0;
                                    egui::Tooltip::always_open(
                                        ui.ctx().clone(),
                                        ui.layer_id(),
                                        egui::Id::new("heat_tip"),
                                        egui::PopupAnchor::Pointer,
                                    )
                                    .show(|ui| {
                                        ui.label(RichText::new(label).strong());
                                        ui.label(format!(
                                            "{} presses · {pct:.1}%",
                                            format_thousands(c)
                                        ));
                                    });
                                }
                                ui.add_space(8.0);
                                // Legend: low → high gradient bar.
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("low").size(11.0).color(pal::TEXT_DIM));
                                    let (bar, _) = ui.allocate_exact_size(
                                        egui::vec2(160.0, 8.0),
                                        egui::Sense::hover(),
                                    );
                                    let steps = 32;
                                    for s in 0..steps {
                                        let t0 = s as f32 / steps as f32;
                                        let mut seg = bar;
                                        seg.min.x = bar.left() + bar.width() * t0;
                                        seg.max.x =
                                            bar.left() + bar.width() * (t0 + 1.0 / steps as f32);
                                        ui.painter().rect_filled(
                                            seg,
                                            egui::CornerRadius::ZERO,
                                            widget::heat_color(t0 as f64),
                                        );
                                    }
                                    ui.label(RichText::new("high").size(11.0).color(pal::TEXT_DIM));
                                });
                            });
                        });
                });
            });
            ui.add_space(10.0);
            // Ranking card.
            ui.vertical(|ui| {
                ui.set_width(right_w);
                egui::Frame::new()
                    .fill(pal::CARD)
                    .stroke(egui::Stroke::new(1.0, pal::BORDER))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("Ranking")
                                .strong()
                                .size(14.5)
                                .color(pal::TEXT),
                        );
                        ui.add_space(8.0);
                        if layer_total == 0 {
                            ui.label(
                                RichText::new("No presses recorded yet for this view.")
                                    .size(12.0)
                                    .color(pal::TEXT_DIM),
                            );
                        }
                        let max = ranked.first().map(|(_, c)| *c).unwrap_or(1) as f32;
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.spacing_mut().interact_size.y = 18.0;
                        for (rank, (idx, count)) in ranked.iter().take(rank_rows).enumerate() {
                            let label = layer_def
                                .and_then(|l| l.keys.get(*idx))
                                .map(|k| labels_for(k).tap)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| format!("key {idx}"));
                            let pct = *count as f64 / layer_total.max(1) as f64 * 100.0;
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [18.0, 16.0],
                                    egui::Label::new(
                                        RichText::new(format!("{}", rank + 1))
                                            .size(11.0)
                                            .color(pal::TEXT_DIM),
                                    ),
                                );
                                ui.add_sized(
                                    [40.0, 16.0],
                                    egui::Label::new(RichText::new(label).strong().size(13.0))
                                        .halign(egui::Align::LEFT),
                                );
                                perf_bar(
                                    ui,
                                    *count as f32 / max,
                                    widget::heat_color((*count as f32 / max) as f64),
                                );
                                ui.label(
                                    RichText::new(format_thousands(*count))
                                        .size(11.5)
                                        .color(pal::TEXT),
                                );
                                ui.label(
                                    RichText::new(format!("· {pct:.1}%"))
                                        .size(11.0)
                                        .color(pal::TEXT_DIM),
                                );
                            });
                        }
                    });
            });
        });
    }

    /// FX Studio: effect library | editor | live on-screen p a CSV.
    pub(super) fn export_heatmap_csv(
        &self,
        counts: &[u64],
        layer_total: u64,
    ) -> anyhow::Result<std::path::PathBuf> {
        use std::io::Write as _;
        let layer = self.heat_layer.unwrap_or(self.view_layer);
        let device_layer = self.device_layer(layer);
        let layer_def = device_layer.as_ref();
        let scope = match self.heat_layer {
            None => "all-layers".to_string(),
            Some(n) => format!("layer{n}"),
        };
        let dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("keyjitsu-heatmap-{scope}-{stamp}.csv"));
        let mut f = std::fs::File::create(&path)?;
        writeln!(f, "rank,key_index,label,presses,percent")?;
        let mut ranked: Vec<(usize, u64)> = counts.iter().copied().enumerate().collect();
        ranked.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (rank, (idx, count)) in ranked.iter().enumerate() {
            let label = layer_def
                .and_then(|l| l.keys.get(*idx))
                .map(|k| labels_for(k).tap)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("key {idx}"));
            let pct = *count as f64 / layer_total.max(1) as f64 * 100.0;
            // Quote labels - some are commas/quotes themselves.
            writeln!(
                f,
                "{},{},\"{}\",{},{:.2}",
                rank + 1,
                idx,
                label.replace('"', "\"\""),
                count,
                pct
            )?;
        }
        Ok(path)
    }
}
