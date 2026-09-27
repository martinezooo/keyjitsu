//! Non-rendering application state and runtime behavior.
//!
//! This module owns transformations between Oryx baseline data, confirmed
//! firmware state, pending edits, custom layers, persisted glow/FX state and
//! runtime gesture state. UI modules should ask this layer for state instead
//! of rebuilding their own interpretation of the keyboard.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeviceStateKind {
    /// No keyboard is attached; the UI can only show the last cached snapshot.
    OfflineSnapshot,
    /// The connected serial identifies an Oryx revision, but there is no
    /// Keyjitsu state marker proving that local/custom changes are represented.
    OryxBaseline,
    /// The connected firmware reports a Keyjitsu state id and the exact state
    /// is available locally.
    VerifiedFirmware,
    /// The firmware reports a Keyjitsu state id, but its exact state is missing
    /// locally. Never substitute the Oryx baseline for this case.
    MissingFirmwareState,
}

impl App {
    pub(super) fn connected_state_marker(&self) -> Option<&str> {
        self.connected
            .as_ref()
            .and_then(|(_, serial)| firmware_state::state_id_from_serial(serial))
    }

    pub(super) fn device_state_kind(&self) -> DeviceStateKind {
        if self.connected.is_none() {
            return DeviceStateKind::OfflineSnapshot;
        }
        match (self.connected_state_marker(), self.firmware_state.as_ref()) {
            (Some(_), Some(_)) if !self.firmware_state_unknown => {
                DeviceStateKind::VerifiedFirmware
            }
            (Some(_), _) => DeviceStateKind::MissingFirmwareState,
            (None, _) => DeviceStateKind::OryxBaseline,
        }
    }

    pub(super) fn firmware_state_verified(&self) -> bool {
        self.device_state_kind() == DeviceStateKind::VerifiedFirmware
    }

    pub(super) fn geometry(&self) -> &'static Geometry {
        geometry::voyager()
    }

    /// Number of layers that come from the Oryx source (not counting the
    /// user's own custom layers).
    pub(super) fn oryx_layer_count(&self) -> u8 {
        self.layout
            .as_ref()
            .and_then(|l| l.revision.layers.iter().map(|layer| layer.position).max())
            .map(|max| max.saturating_add(1))
            .unwrap_or(0)
    }

    pub(super) fn layer_def(&self, n: u8) -> Option<&Layer> {
        let oryx = self.oryx_layer_count();
        if n < oryx {
            self.layout
                .as_ref()
                .and_then(|l| l.revision.layers.iter().find(|la| la.position == n))
        } else {
            self.synth_layers.get((n - oryx) as usize)
        }
    }

    /// Best available runtime projection for a physical key.
    ///
    /// VerifiedFirmware applies the exact Keyjitsu-authored overlay. An
    /// unmarked keyboard can only use its Oryx revision as an explicitly
    /// unverified baseline. MissingFirmwareState returns "?" rather than
    /// silently substituting Oryx.
    pub(super) fn device_key(&self, layer: u8, key: usize) -> Option<OryxKey> {
        if self.device_state_kind() == DeviceStateKind::MissingFirmwareState {
            return Some(unknown_device_key());
        }

        let oryx = self.oryx_layer_count();
        if layer >= oryx {
            let state = self.firmware_state.as_ref()?;
            let custom = state.custom_layers.get((layer - oryx) as usize)?;
            return Some(
                custom
                    .keys
                    .iter()
                    .find(|entry| entry.key as usize == key)
                    .map(|entry| synth_key(&entry.code))
                    .unwrap_or_default(),
            );
        }

        if let Some(state) = &self.firmware_state {
            if let Some(dance) = state
                .dances
                .iter()
                .find(|dance| dance.layer == layer && dance.key as usize == key)
            {
                return Some(synth_slots(&dance.slots));
            }
            if let Some(edit) = state
                .edits
                .iter()
                .find(|edit| edit.layer == layer && edit.key as usize == key)
            {
                return Some(synth_key(&edit.code));
            }
        }
        self.layer_def(layer).and_then(|l| l.keys.get(key)).cloned()
    }

    pub(super) fn device_layer(&self, layer: u8) -> Option<Layer> {
        if self.device_state_kind() == DeviceStateKind::MissingFirmwareState {
            let mut out = self.layer_def(layer).cloned().unwrap_or_else(|| Layer {
                title: Some("Unknown device state".into()),
                position: layer,
                color: None,
                keys: vec![unknown_device_key(); self.geometry().len()],
            });
            if out.keys.len() < self.geometry().len() {
                out.keys.resize(self.geometry().len(), unknown_device_key());
            }
            for key in &mut out.keys {
                *key = unknown_device_key();
            }
            return Some(out);
        }

        let oryx = self.oryx_layer_count();
        if layer >= oryx {
            let state = self.firmware_state.as_ref()?;
            let custom = state.custom_layers.get((layer - oryx) as usize)?;
            return Some(self.synth_custom_layer(layer, custom));
        }

        let mut out = self.layer_def(layer)?.clone();
        for key in 0..out.keys.len() {
            if let Some(device) = self.device_key(layer, key) {
                out.keys[key] = device;
            }
        }
        Some(out)
    }

    /// Editing projection = best available baseline plus explicit pending edits.
    /// A missing marked firmware state is never replaced by an Oryx guess.
    pub(super) fn editing_key(&self, layer: u8, key: usize) -> Option<OryxKey> {
        if self.device_state_kind() == DeviceStateKind::MissingFirmwareState {
            return self.device_key(layer, key);
        }
        if layer >= self.oryx_layer_count() {
            return self.layer_def(layer).and_then(|l| l.keys.get(key)).cloned();
        }
        if let Some(slots) = self.key_dances.get(&(layer, key)) {
            let mut out = synth_slots(slots);
            let label = slots
                .iter()
                .flatten()
                .map(|code| self.slot_chip_label(code).replace('\n', " then "))
                .collect::<Vec<_>>()
                .join(" / ");
            if !label.is_empty() {
                out.custom_label = Some(label);
            }
            return Some(out);
        }
        if let Some(code) = self.key_edits.get(&(layer, key)) {
            let mut out = synth_key(code);
            out.custom_label = Some(self.slot_chip_label(code).replace('\n', " then "));
            return Some(out);
        }
        self.device_key(layer, key)
    }

    pub(super) fn editing_layer(&self, layer: u8) -> Option<Layer> {
        if self.device_state_kind() == DeviceStateKind::MissingFirmwareState {
            return self.device_layer(layer);
        }
        if layer >= self.oryx_layer_count() {
            return self.layer_def(layer).cloned();
        }
        let mut out = self.device_layer(layer)?;
        for key in 0..out.keys.len() {
            if let Some(editing) = self.editing_key(layer, key) {
                out.keys[key] = editing;
            }
        }
        Some(out)
    }

    pub(super) fn assignment_editable(&self, layer: u8, key: usize) -> bool {
        self.device_key(layer, key)
            .map(|key| key.assignment_roundtrip_safe())
            .unwrap_or(true)
    }

    /// Physical positions participating in configured Oryx combos on this
    /// layer. Combos are revision-level relations, not key assignments.
    pub(super) fn combo_member_mask(&self, layer: u8) -> Vec<bool> {
        let mut mask = vec![false; self.geometry().len()];
        let Some(layout) = &self.layout else {
            return mask;
        };
        for combo in layout
            .revision
            .combos
            .iter()
            .filter(|combo| combo.layer_idx == layer)
        {
            for &key in &combo.key_indices {
                if let Some(member) = mask.get_mut(key) {
                    *member = true;
                }
            }
        }
        mask
    }

    /// Human-readable configured combos involving one physical key.
    /// Uses the same centralized action/legend conversion as Live and the
    /// editor so a modifier trigger cannot silently degrade to its base key.
    pub(super) fn combo_summaries_for_key(&self, layer: u8, key: usize) -> Vec<String> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        layout
            .revision
            .combos
            .iter()
            .filter(|combo| combo.layer_idx == layer && combo.key_indices.contains(&key))
            .map(|combo| {
                let chord = combo
                    .key_indices
                    .iter()
                    .map(|&idx| {
                        self.editing_key(layer, idx)
                            .map(|key| legend::full_labels_for(&key).tap)
                            .filter(|label| !label.is_empty())
                            .unwrap_or_else(|| format!("key {idx}"))
                    })
                    .collect::<Vec<_>>()
                    .join(" + ");
                let trigger = combo
                    .trigger_action()
                    .map(|action| legend::action_label(&action))
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| "unassigned".to_string());
                format!("{chord} → {trigger}")
            })
            .collect()
    }

    pub(super) fn desired_firmware_maps(&self) -> (FirmwareEdits, FirmwareDances) {
        merge_firmware_maps(
            self.firmware_state.as_ref(),
            &self.key_edits,
            &self.key_dances,
        )
    }

    pub(super) fn layer_count(&self) -> u8 {
        (self.oryx_layer_count() + self.custom_layers.len() as u8)
            .max(1)
            .max(self.active_layer + 1)
    }

    /// True if layer `n` is one the user authored (editable in place, no Oryx
    /// source behind it). Requires a loaded layout (else there's no baseline).
    pub(super) fn is_custom_layer(&self, n: u8) -> bool {
        self.layout.is_some() && n >= self.oryx_layer_count()
    }

    /// Index into `custom_layers` for layer `n`, if it's a custom one.
    pub(super) fn custom_index(&self, n: u8) -> Option<usize> {
        let oryx = self.oryx_layer_count();
        (n >= oryx)
            .then(|| (n - oryx) as usize)
            .filter(|&i| i < self.custom_layers.len())
    }

    pub(super) fn hydrate_custom_layers(&mut self, hash: &str) {
        let cfg = config::load();
        if let Some(set) = cfg.custom_layer_sets.iter().find(|s| s.layout == hash) {
            self.custom_layers = set.layers.clone();
        } else {
            let legacy: Vec<_> = cfg
                .custom_layers
                .into_iter()
                .filter(|c| c.layout == hash)
                .collect();
            self.custom_layers = if !legacy.is_empty() {
                legacy
            } else {
                self.firmware_state
                    .as_ref()
                    .filter(|state| state.layout_hash == hash)
                    .map(|state| state.custom_layers.clone())
                    .unwrap_or_default()
            };
        }
        self.rebuild_synth_layers();
    }

    pub(super) fn save_custom_layers(&mut self) {
        let Some(hash) = self.layout_hash.clone() else {
            return;
        };
        let layers = self.custom_layers.clone();
        self.persist_config("saving custom layers", move |cfg| {
            cfg.custom_layers.retain(|c| c.layout != hash);
            cfg.custom_layer_sets.retain(|s| s.layout != hash);
            cfg.custom_layer_sets.push(config::CustomLayerSet {
                layout: hash,
                layers,
            });
        });
        self.rebuild_synth_layers();
    }

    pub(super) fn persist_layout_scoped_state(&mut self) {
        let Some(hash) = self.layout_hash.clone() else {
            return;
        };
        let state = LayoutScopedState {
            custom_layers: self.custom_layers.clone(),
            glow_overrides: self
                .glow_work
                .iter()
                .map(|(&(layer, key), &rgb)| GlowOverride {
                    layout: hash.clone(),
                    layer,
                    key: key as u16,
                    rgb,
                })
                .collect(),
            key_fx: self
                .key_fx
                .iter()
                .map(
                    |(&(layer, key), (trigger, effect, color, custom))| config::KeyFx {
                        layout: hash.clone(),
                        layer,
                        key: key as u16,
                        trigger: *trigger,
                        effect: *effect,
                        color: *color,
                        custom: custom.clone(),
                    },
                )
                .collect(),
            staged_edits: self
                .key_edits
                .iter()
                .map(|(&(layer, key), code)| StagedEdit {
                    layout: hash.clone(),
                    layer,
                    key: key as u16,
                    code: code.clone(),
                })
                .collect(),
            staged_dances: self
                .key_dances
                .iter()
                .map(|(&(layer, key), slots)| StagedDance {
                    layout: hash.clone(),
                    layer,
                    key: key as u16,
                    slots: slots.clone(),
                })
                .collect(),
        };
        let saved = self.persist_config("saving layer changes", move |cfg| {
            replace_layout_scoped_state(cfg, &hash, state);
        });
        if saved {
            self.glow_saved = self.glow_work.clone();
        }
        self.rebuild_synth_layers();
    }

    /// Append a new empty custom layer and view it.
    pub(super) fn add_custom_layer(&mut self, name: String) {
        let Some(hash) = self.layout_hash.clone() else {
            return;
        };
        self.custom_layers.push(config::CustomLayer {
            layout: hash,
            name,
            keys: Vec::new(),
        });
        self.save_custom_layers();
        self.view_layer = self.layer_count() - 1;
        self.follow = false;
    }

    /// Remove custom layer `n`, renumbering everything that referred to a
    /// higher layer down by one - so colors/effects/staged edits and
    /// layer-switch keycodes don't silently point at the wrong layer.
    pub(super) fn remove_custom_layer(&mut self, del: u8) {
        let Some(i) = self.custom_index(del) else {
            return;
        };
        self.custom_layers.remove(i);

        // 1. (layer, key)-keyed maps: drop the deleted layer, shift higher down.
        fn shift<V>(map: &mut HashMap<(u8, usize), V>, del: u8) {
            let taken = std::mem::take(map);
            *map = taken
                .into_iter()
                .filter(|((l, _), _)| *l != del)
                .map(|((l, k), v)| ((if l > del { l - 1 } else { l }, k), v))
                .collect();
        }
        shift(&mut self.glow_work, del);
        shift(&mut self.glow_saved, del);
        shift(&mut self.key_fx, del);
        shift(&mut self.key_edits, del);
        shift(&mut self.key_dances, del);

        // 2. Layer-switch keycode strings pointing above `del` shift down.
        for cl in &mut self.custom_layers {
            for k in &mut cl.keys {
                k.code = renumber_layer_ref(&k.code, del);
            }
        }
        for code in self.key_edits.values_mut() {
            *code = renumber_layer_ref(code, del);
        }
        for slots in self.key_dances.values_mut() {
            for slot in slots.iter_mut().flatten() {
                *slot = renumber_layer_ref(slot, del);
            }
        }

        if let Some(state) = &self.firmware_state {
            for edit in &state.edits {
                let pos = (edit.layer, edit.key as usize);
                let rewritten = renumber_layer_ref(&edit.code, del);
                if rewritten != edit.code
                    && !self.key_edits.contains_key(&pos)
                    && !self.key_dances.contains_key(&pos)
                {
                    self.key_edits.insert(pos, rewritten);
                }
            }
            for dance in &state.dances {
                let pos = (dance.layer, dance.key as usize);
                let mut rewritten = dance.slots.clone();
                for slot in rewritten.iter_mut().flatten() {
                    *slot = renumber_layer_ref(slot, del);
                }
                if rewritten != dance.slots
                    && !self.key_edits.contains_key(&pos)
                    && !self.key_dances.contains_key(&pos)
                {
                    self.key_dances.insert(pos, rewritten);
                }
            }
        }

        // Persist all layout-scoped state in one atomic config replacement.
        // A crash or disk error can no longer leave layer numbers renumbered in
        // only some of custom layers, glow/effects, or staged firmware changes.
        self.persist_layout_scoped_state();
        self.view_layer = self.view_layer.min(self.layer_count().saturating_sub(1));
        self.edit_synced = None; // re-hydrate the editor for the new indices
    }

    pub(super) fn rename_custom_layer(&mut self, n: u8, name: String) {
        if let Some(i) = self.custom_index(n) {
            self.custom_layers[i].name = name;
            self.save_custom_layers();
        }
    }

    /// Assign a keycode to a key on a custom layer (persisted). Empty/KC_NO
    /// clears it.
    pub(super) fn set_custom_key(&mut self, n: u8, key: usize, code: &str) {
        let Some(i) = self.custom_index(n) else {
            return;
        };
        let cl = &mut self.custom_layers[i];
        cl.keys.retain(|k| k.key != key as u16);
        if !code.is_empty() && code != "KC_TRANSPARENT" && code != "KC_TRNS" {
            cl.keys.push(config::CustomKey {
                key: key as u16,
                code: code.to_string(),
            });
        }
        self.save_custom_layers();
    }

    pub(super) fn synth_custom_layer(&self, position: u8, custom: &config::CustomLayer) -> Layer {
        let mut keys: Vec<OryxKey> = (0..self.geometry().len())
            .map(|_| OryxKey::default())
            .collect();
        for entry in &custom.keys {
            if let Some(slot) = keys.get_mut(entry.key as usize) {
                *slot = synth_key(&entry.code);
            }
        }
        Layer {
            title: Some(custom.name.clone()),
            position,
            color: None,
            keys,
        }
    }

    /// Build display layers from the desired custom-layer state.
    pub(super) fn rebuild_synth_layers(&mut self) {
        let oryx = self.oryx_layer_count();
        self.synth_layers = self
            .custom_layers
            .iter()
            .enumerate()
            .map(|(i, custom)| self.synth_custom_layer(oryx + i as u8, custom))
            .collect();
    }

    // --- glow editor ---------------------------------------------------

    /// With no keyboard attached, show the last layout we saw (from cache)
    /// instead of a grid of blank keys. Cache-only, so it never blocks on the
    /// network; a no-op on the very first run (nothing cached yet).
    pub(super) fn load_last_layout(&mut self) {
        if self.layout.is_some() {
            return;
        }
        // Prefer the last device identity; fall back to any cached Voyager layout.
        let remembered = config::load().last_layout;
        let found = remembered
            .as_deref()
            .and_then(|serial| {
                LayoutId::from_serial(serial).ok().and_then(|id| {
                    crate::oryx_api::cached_layout(&id, "voyager")
                        .map(|l| (Some(serial.to_string()), id, l))
                })
            })
            .or_else(|| crate::oryx_api::any_cached_layout("voyager").map(|(id, l)| (None, id, l)));
        let Some((serial, id, layout)) = found else {
            return;
        };

        let state_marker = serial
            .as_deref()
            .and_then(firmware_state::state_id_from_serial);
        self.firmware_state = state_marker
            .and_then(FirmwareState::load)
            .filter(|state| state.layout_hash == id.hash && state.revision == id.revision);
        self.firmware_state_unknown = state_marker.is_some() && self.firmware_state.is_none();
        self.layout = Some(layout);
        self.hydrate_glow(&id.hash); // also sets self.layout_hash
        self.hydrate_key_fx(&id.hash);
        self.hydrate_custom_layers(&id.hash);
        self.hydrate_staged(&id.hash);
        self.drop_applied_from_staged();
        self.hydrate_heatmap(&id.hash, self.geometry().len());
        self.push_anim_base();
    }

    pub(super) fn hydrate_heatmap(&mut self, hash: &str, key_count: usize) {
        match HeatmapStore::load(hash, key_count) {
            Ok(heat) => {
                self.heat = Some(heat);
                self.heat_error = None;
            }
            Err(e) => {
                self.heat = None;
                self.heat_error = Some(format!("{e:#}"));
            }
        }
    }

    /// Load saved glow overrides for a layout into the working + saved maps.
    pub(super) fn hydrate_glow(&mut self, hash: &str) {
        let cfg = config::load();
        let map: HashMap<(u8, usize), [u8; 3]> = cfg
            .glow_overrides
            .iter()
            .filter(|o| o.layout == hash)
            .map(|o| ((o.layer, o.key as usize), o.rgb))
            .collect();
        self.layout_hash = Some(hash.to_string());
        self.glow_saved = map.clone();
        self.glow_work = map;
        if !self.glow_work.is_empty() && self.sync_glow {
            self.needs_push = true;
        }
    }

    /// The layout's own glow color for a key: its explicit `glowColor`, or the
    /// layer's default color (that's how the firmware lights plain keys).
    pub(super) fn layout_glow(&self, layer: u8, key: usize) -> Option<Color32> {
        let l = self.layer_def(layer)?;
        l.keys
            .get(key)?
            .glow_color
            .as_deref()
            .and_then(parse_hex)
            .or_else(|| l.color.as_deref().and_then(parse_hex))
    }

    /// Effective glow (override wins over layout) for every key of a layer.
    pub(super) fn glow_colors(&self, layer: u8) -> Vec<Option<Color32>> {
        (0..self.geometry().len())
            .map(|i| {
                self.glow_work
                    .get(&(layer, i))
                    .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
                    .or_else(|| self.layout_glow(layer, i))
            })
            .collect()
    }

    pub(super) fn current_key_srgb(&self, layer: u8, key: usize) -> [u8; 3] {
        if let Some(c) = self.glow_work.get(&(layer, key)) {
            *c
        } else if let Some(c) = self.layout_glow(layer, key) {
            [c.r(), c.g(), c.b()]
        } else {
            [0, 0, 0]
        }
    }

    pub(super) fn set_glow(&mut self, layer: u8, key: usize, rgb: [u8; 3]) {
        self.glow_work.insert((layer, key), rgb);
        if self.sync_glow && layer == self.active_layer {
            self.needs_push = true;
        }
        self.push_anim_base();
    }

    pub(super) fn clear_glow(&mut self, layer: u8, key: usize) {
        self.glow_work.remove(&(layer, key));
        if self.sync_glow && layer == self.active_layer {
            self.needs_push = true;
        }
        self.push_anim_base();
    }

    // --- per-key press effects -----------------------------------------

    pub(super) fn hydrate_key_fx(&mut self, hash: &str) {
        let cfg = config::load();
        self.key_fx = cfg
            .key_fx
            .iter()
            .filter(|f| f.layout == hash)
            .map(|f| {
                (
                    (f.layer, f.key as usize),
                    (f.trigger, f.effect, f.color, f.custom.clone()),
                )
            })
            .collect();
    }

    /// If a reconnect proves that staged edits are already present in the
    /// running firmware, they are no longer pending. This also heals the case
    /// where the app was killed after flashing but before it could clear them.
    pub(super) fn drop_applied_from_staged(&mut self) {
        let Some(state) = self.firmware_state.clone() else {
            return;
        };

        let mut pending_edits = self.key_edits.clone();
        pending_edits
            .retain(|&(layer, key), code| staged_edit_is_pending(&state, layer, key, code));
        let mut pending_dances = self.key_dances.clone();
        pending_dances
            .retain(|&(layer, key), slots| staged_dance_is_pending(&state, layer, key, slots));

        let layout_hash = state.layout_hash.clone();
        let custom_layers = state.custom_layers.clone();
        let edits: Vec<_> = pending_edits
            .iter()
            .map(|(&(layer, key), code)| StagedEdit {
                layout: layout_hash.clone(),
                layer,
                key: key as u16,
                code: code.clone(),
            })
            .collect();
        let dances: Vec<_> = pending_dances
            .iter()
            .map(|(&(layer, key), slots)| StagedDance {
                layout: layout_hash.clone(),
                layer,
                key: key as u16,
                slots: slots.clone(),
            })
            .collect();
        if self.persist_config("confirming applied firmware state", move |cfg| {
            reconcile_confirmed_layout_config(cfg, &layout_hash, &custom_layers, edits, dances);
        }) {
            self.key_edits = pending_edits;
            self.key_dances = pending_dances;
        }
    }

    /// Load this layout's staged (not-yet-built) remaps and tap dances into the
    /// working maps, so pending changes survive a restart / profile switch.
    pub(super) fn hydrate_staged(&mut self, hash: &str) {
        let cfg = config::load();
        self.key_edits = cfg
            .staged_edits
            .iter()
            .filter(|e| e.layout == hash)
            .map(|e| ((e.layer, e.key as usize), e.code.clone()))
            .collect();
        self.key_dances = cfg
            .staged_dances
            .iter()
            .filter(|d| d.layout == hash)
            .map(|d| ((d.layer, d.key as usize), d.slots.clone()))
            .collect();
    }

    /// Persist this layout's staged remaps + tap dances. Mirrors [`Self::save_glow`]:
    /// they used to live only in memory until the next build, so a restart
    /// silently dropped them.
    pub(super) fn save_staged(&mut self) {
        let Some(hash) = self.layout_hash.clone() else {
            return;
        };
        let edits: Vec<_> = self
            .key_edits
            .iter()
            .map(|(&(layer, key), code)| StagedEdit {
                layout: hash.clone(),
                layer,
                key: key as u16,
                code: code.clone(),
            })
            .collect();
        let dances: Vec<_> = self
            .key_dances
            .iter()
            .map(|(&(layer, key), slots)| StagedDance {
                layout: hash.clone(),
                layer,
                key: key as u16,
                slots: slots.clone(),
            })
            .collect();
        self.persist_config("saving pending key changes", move |cfg| {
            replace_staged_config(cfg, &hash, edits, dances);
        });
    }

    /// Persist the current key_fx map for this layout.
    pub(super) fn save_key_fx(&mut self) {
        let Some(hash) = self.layout_hash.clone() else {
            return;
        };
        let entries: Vec<_> = self
            .key_fx
            .iter()
            .map(
                |(&(layer, key), (trigger, effect, color, custom))| config::KeyFx {
                    layout: hash.clone(),
                    layer,
                    key: key as u16,
                    trigger: *trigger,
                    effect: *effect,
                    color: *color,
                    custom: custom.clone(),
                },
            )
            .collect();
        self.persist_config("saving per-key effects", move |cfg| {
            cfg.key_fx.retain(|f| f.layout != hash);
            cfg.key_fx.extend(entries);
        });
    }

    /// Persist the user-built custom effects.
    pub(super) fn save_custom_fx(&mut self) {
        let custom_fx = self.custom_fx.clone();
        self.persist_config("saving custom effects", move |cfg| {
            cfg.custom_fx = custom_fx;
        });
    }

    /// Translate a custom-effect step by one grid unit; keys that would land
    /// off the board are dropped (a line "walks off" the edge).
    pub(super) fn shift_step(&mut self, fx: usize, step: usize, dx: f32, dy: f32) {
        let pos: Vec<(f32, f32)> = {
            let g = self.geometry();
            g.keys.iter().map(|k| (k.x, k.y)).collect()
        };
        let Some(s) = self
            .custom_fx
            .get_mut(fx)
            .and_then(|c| c.steps.get_mut(step))
        else {
            return;
        };
        let mut out: Vec<u16> = s
            .keys
            .iter()
            .filter_map(|&k| {
                let &(x, y) = pos.get(k as usize)?;
                let (tx, ty) = (x + dx, y + dy);
                pos.iter()
                    .position(|&(px, py)| (px - tx).abs() < 0.45 && (py - ty).abs() < 0.45)
                    .map(|j| j as u16)
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        s.keys = out;
        self.save_custom_fx();
    }

    /// Classify a just-released key into a gesture and append to the combo
    /// log: single/double tap, and whether the final press was a hold. A
    /// second tap of the same key within the window upgrades the last entry.
    pub(super) fn record_combo(&mut self, idx: usize) {
        let now = Instant::now();
        let down = self.combo_down.remove(&idx);
        let held = down
            .map(|d| now.duration_since(d).as_millis() > HOLD_MS)
            .unwrap_or(false);
        let down_at = down.unwrap_or(now);
        let hold_ms = down.map(|d| now.duration_since(d).as_millis()).unwrap_or(0);
        // Double-tap is measured PRESS-to-PRESS (like a double-click), so the
        // hold time of the first tap doesn't eat the window.
        let gap_to_prev = self.combo_log.back().and_then(|e| {
            down.map(|d| {
                (
                    e.key,
                    e.count,
                    d.saturating_duration_since(e.down_at).as_millis(),
                )
            })
        });
        let merged = combo_merges(gap_to_prev, idx);
        if merged {
            let gap = gap_to_prev.map(|(_, _, g)| g).unwrap_or(0);
            if let Some(last) = self.combo_log.back_mut() {
                last.count = 2;
                last.held = held;
                last.hold_ms = hold_ms;
                last.gap_ms = gap;
                last.at = now;
            }
        } else {
            self.combo_log.push_back(ComboEntry {
                key: idx,
                count: 1,
                held,
                hold_ms,
                gap_ms: 0,
                down_at,
                at: now,
            });
            while self.combo_log.len() > 10 {
                self.combo_log.pop_front();
            }
        }
        // Keep the HUD up so the just-finalized chip (⇩ / ×2) stays visible.
        if self.peek.show_combo && self.peek.enabled {
            self.peek_layer = self.active_layer;
            self.peek_until = Some(now + Duration::from_millis(1600));
        }
    }

    /// Recent combo chips (oldest→newest) with their measured timings:
    /// finalized presses (dropped after ~2.5s) plus any key currently held,
    /// shown live with a counting-up hold duration.
    pub(super) fn combo_recent(&self) -> Vec<ComboChip> {
        let now = Instant::now();
        let label = |k: usize| {
            self.device_key(self.active_layer, k)
                .map(|key| labels_for(&key).tap)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("k{k}"))
        };
        let mut out: Vec<ComboChip> = self
            .combo_log
            .iter()
            .filter(|e| now.duration_since(e.at) < Duration::from_millis(2500))
            .rev()
            .take(6)
            .map(|e| ComboChip {
                label: label(e.key),
                count: e.count,
                held: e.held,
                live: false,
                ms: e.hold_ms,
                gap_ms: e.gap_ms,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // Live "holding" chips for keys still physically down past threshold,
        // with the hold duration counting up in real time.
        let mut live: Vec<(usize, u128)> = self
            .combo_down
            .iter()
            .filter(|(_, &d)| {
                let ms = now.duration_since(d).as_millis();
                ms > HOLD_MS && ms < 8000 // ignore stale (a missed KeyUp)
            })
            .map(|(&k, &d)| (k, now.duration_since(d).as_millis()))
            .collect();
        live.sort_by_key(|&(_, ms)| ms);
        for (k, ms) in live {
            out.push(ComboChip {
                label: label(k),
                count: 1,
                held: true,
                live: true,
                ms,
                gap_ms: 0,
            });
        }
        out
    }

    /// On a key press: fire its per-key effect (honoring the double-press
    /// trigger) or fall back to the global press effect.
    pub(super) fn fire_key_fx(&mut self, idx: usize) {
        let now = Instant::now();
        let is_double = self
            .last_press_at
            .insert(idx, now)
            .is_some_and(|prev| now.duration_since(prev) < Duration::from_millis(400));

        let per_key = self.key_fx.get(&(self.active_layer, idx)).cloned();
        let fired = match per_key {
            Some((FxTrigger::Press, effect, color, custom)) => Some((effect, color, custom)),
            Some((FxTrigger::DoublePress, effect, color, custom)) if is_double => {
                Some((effect, color, custom))
            }
            _ => None,
        };

        // A per-key "★ custom" assignment plays the sequence once.
        let seq = fired.as_ref().and_then(|(_, _, custom)| {
            custom.as_deref().and_then(|name| {
                self.custom_fx
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| std::sync::Arc::new(c.steps.clone()))
            })
        });

        let Ok(mut a) = self.anim.lock() else { return };
        let (effect, color) = match &fired {
            Some((e, c, _)) => (*e, *c),
            // Global fallback (FX Studio → board RGB → on key press).
            None if a.press_effect != PressEffect::None => (a.press_effect, a.press_color),
            None => return,
        };
        if effect == PressEffect::None && seq.is_none() {
            return;
        }
        // Seed from the wall clock so sparkle/matrix patterns differ per press.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ (idx as u64) << 32)
            .unwrap_or(idx as u64);
        a.events.push(FxEvent {
            key: idx,
            effect,
            color,
            at: now,
            seed,
            seq,
        });
    }

    // --- performance sampler -------------------------------------------

    pub(super) fn perf_state(&self) -> perf::PerfState {
        let (anim, press_fx) = self
            .anim
            .lock()
            .map(|a| (a.effect != Effect::Off, !a.events.is_empty()))
            .unwrap_or((false, false));
        perf::PerfState {
            anim,
            press_fx,
            peek: self.peek_until.is_some_and(|u| Instant::now() < u),
            glow_sync: self.sync_glow,
            connected: self.connected.is_some(),
        }
    }

    pub(super) fn set_anim_effect(&self, effect: Effect) {
        if let Ok(mut a) = self.anim.lock() {
            a.effect = effect;
        }
    }

    pub(super) fn persist_config(&mut self, context: &str, mutate: impl FnOnce(&mut config::Config)) -> bool {
        match config::update(mutate) {
            Ok(()) => {
                self.persist_error = None;
                true
            }
            Err(e) => {
                self.persist_error = Some(format!("{context}: {e:#}"));
                false
            }
        }
    }
}
