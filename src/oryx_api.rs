//! Client for the Oryx GraphQL API (layout definitions) with a disk cache.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://oryx.zsa.io/graphql";
const MAX_LAYOUT_BYTES: u64 = 4 * 1024 * 1024;

const LAYOUT_QUERY: &str = r#"query Layout($hashId: String!, $geometry: String!, $revisionId: String!) {
  layout(hashId: $hashId, geometry: $geometry, revisionId: $revisionId) {
    hashId title geometry
    revision {
      hashId title model
      layers { title position color keys }
      combos { keyIndices layerIdx trigger }
    }
  }
}"#;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub hash_id: String,
    pub title: String,
    pub geometry: String,
    pub revision: Revision,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub hash_id: String,
    #[allow(dead_code)]
    pub title: Option<String>,
    pub layers: Vec<Layer>,
    /// Oryx combos are revision-level chords, not properties of an individual
    /// key. Keep them in the canonical layout model so every UI surface sees
    /// the same relation between physical key positions and the emitted action.
    #[serde(default)]
    pub combos: Vec<OryxCombo>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OryxCombo {
    /// Physical key positions (same index space as Layer::keys / Voyager LAYOUT).
    pub key_indices: Vec<usize>,
    /// Layer index on which the chord is active.
    pub layer_idx: u8,
    /// Action emitted when all combo keys are pressed.
    pub trigger: Option<KeyAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub title: Option<String>,
    pub position: u8,
    /// The layer's default color - keys without their own `glowColor` light up
    /// in this color on the physical board.
    #[serde(default)]
    pub color: Option<String>,
    pub keys: Vec<OryxKey>,
}

/// One key of one layer, as Oryx models it.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OryxKey {
    pub tap: Option<KeyAction>,
    pub hold: Option<KeyAction>,
    pub tap_hold: Option<KeyAction>,
    pub double_tap: Option<KeyAction>,
    pub custom_label: Option<String>,
    pub emoji: Option<String>,
    pub glow_color: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyModifiers {
    pub left_alt: bool,
    pub left_ctrl: bool,
    pub left_gui: bool,
    pub left_shift: bool,
    pub right_alt: bool,
    pub right_ctrl: bool,
    pub right_gui: bool,
    pub right_shift: bool,
}

impl KeyModifiers {
    pub fn is_empty(&self) -> bool {
        !self.left_alt
            && !self.left_ctrl
            && !self.left_gui
            && !self.left_shift
            && !self.right_alt
            && !self.right_ctrl
            && !self.right_gui
            && !self.right_shift
    }

    /// Oryx has used both a singular `modifier: "LALT"` field and the
    /// newer `modifiers: { leftAlt: true }` mask. Normalize both into this
    /// one mask before anything reaches the UI/editor.
    pub fn apply_token(&mut self, token: &str) -> bool {
        match token.trim().to_ascii_uppercase().as_str() {
            "LALT" | "LEFT_ALT" => self.left_alt = true,
            "RALT" | "RIGHT_ALT" => self.right_alt = true,
            "LCTL" | "LCTRL" | "LEFT_CTRL" => self.left_ctrl = true,
            "RCTL" | "RCTRL" | "RIGHT_CTRL" => self.right_ctrl = true,
            "LGUI" | "LEFT_GUI" => self.left_gui = true,
            "RGUI" | "RIGHT_GUI" => self.right_gui = true,
            "LSFT" | "LSHIFT" | "LEFT_SHIFT" => self.left_shift = true,
            "RSFT" | "RSHIFT" | "RIGHT_SHIFT" => self.right_shift = true,
            _ => return false,
        }
        true
    }

    /// Convert normalized Oryx modifier flags into the QMK wrapper form used
    /// everywhere else in Keyjitsu. Modifier order does not change the chord.
    pub fn wrap_qmk(&self, base: &str) -> String {
        let mut code = base.to_string();
        for (enabled, wrapper) in [
            (self.left_gui, "LGUI"),
            (self.right_gui, "RGUI"),
            (self.left_alt, "LALT"),
            (self.right_alt, "RALT"),
            (self.left_shift, "LSFT"),
            (self.right_shift, "RSFT"),
            (self.left_ctrl, "LCTL"),
            (self.right_ctrl, "RCTL"),
        ] {
            if enabled {
                code = format!("{wrapper}({code})");
            }
        }
        code
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyAction {
    /// QMK keycode (`KC_A`) or a layer-switch family (`TO`, `MO`, `LT`, …).
    pub code: Option<String>,
    /// Target layer for layer-switch codes.
    pub layer: Option<u8>,
    pub description: Option<String>,
    /// Oryx stores precomposed shortcuts (for example Option+Tab) as a base
    /// keycode plus modifier flags instead of a wrapped QMK code.
    pub modifiers: Option<KeyModifiers>,
    /// Older/current Oryx payloads may carry one modifier separately from
    /// the boolean modifier mask. It is part of the action, not metadata.
    pub modifier: Option<String>,
    #[serde(rename = "macro")]
    pub macro_action: Option<serde_json::Value>,
    pub color: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl KeyAction {
    /// Canonical editable QMK representation of this action.
    pub fn qmk_code(&self) -> Option<String> {
        if let Some(layer) = self.layer {
            let family = self
                .code
                .as_deref()
                .filter(|code| !code.trim().is_empty())
                .unwrap_or("MO");
            return Some(format!("{family}({layer})"));
        }

        let base = self.code.as_deref()?.trim();
        if base.is_empty() {
            return None;
        }
        let mut mods = self.modifiers.clone().unwrap_or_default();
        if let Some(modifier) = self.modifier.as_deref() {
            let _ = mods.apply_token(modifier);
        }
        Some(if mods.is_empty() {
            base.to_string()
        } else {
            mods.wrap_qmk(base)
        })
    }

    /// False means editing this action as a plain QMK string could lose Oryx
    /// semantics that Keyjitsu does not model yet.
    pub fn roundtrip_safe(&self) -> bool {
        self.macro_action.is_none() && self.color.is_none() && self.extra.is_empty()
    }

    pub fn fallback_kind(&self) -> Option<&'static str> {
        if self.macro_action.is_some() {
            Some("Macro")
        } else if self.color.is_some() {
            Some("RGB action")
        } else if !self.extra.is_empty() {
            Some("Oryx action")
        } else {
            None
        }
    }
}

impl OryxKey {
    pub fn assignment_roundtrip_safe(&self) -> bool {
        [
            self.tap.as_ref(),
            self.hold.as_ref(),
            self.double_tap.as_ref(),
            self.tap_hold.as_ref(),
        ]
        .into_iter()
        .flatten()
        .all(KeyAction::roundtrip_safe)
    }
}

/// `hashId` / `revisionId` pair identifying a layout revision. The firmware's
/// serial string is exactly `"<hashId>/<revisionId>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutId {
    pub hash: String,
    pub revision: String,
}

impl LayoutId {
    fn validate_part(kind: &str, value: &str) -> Result<()> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("invalid Oryx {kind} {value:?}");
        }
        Ok(())
    }

    pub fn new(hash: String, revision: String) -> Result<LayoutId> {
        Self::validate_part("layout hash", &hash)?;
        Self::validate_part("revision", &revision)?;
        Ok(LayoutId { hash, revision })
    }

    pub fn from_serial(serial: &str) -> Result<LayoutId> {
        match serial.split_once('/') {
            Some((h, r)) if !h.is_empty() && !r.is_empty() => {
                let revision = r
                    .split_once(crate::firmware_state::SERIAL_MARKER)
                    .map(|(rev, _)| rev)
                    .unwrap_or(r);
                if revision.is_empty() {
                    bail!("keyboard serial {serial:?} has an empty revision before the Keyjitsu state marker");
                }
                Self::new(h.to_string(), revision.to_string())
            }
            _ => bail!(
                "keyboard serial {serial:?} does not look like an Oryx layout id \
                 (expected \"hash/revision\"). Pass --url or --hash instead"
            ),
        }
    }

    /// Accepts `https://configure.zsa.io/<geometry>/layouts/<hash>[/<rev>[/...]]`.
    pub fn from_url(url: &str) -> Result<LayoutId> {
        let parts: Vec<&str> = url.trim_end_matches('/').split('/').collect();
        let i = parts
            .iter()
            .position(|p| *p == "layouts")
            .ok_or_else(|| anyhow!("no \"/layouts/\" segment in {url:?}"))?;
        let hash = parts
            .get(i + 1)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| anyhow!("no layout hash after /layouts/ in {url:?}"))?;
        let revision = parts.get(i + 2).copied().unwrap_or("latest");
        Self::new(hash.to_string(), revision.to_string())
    }
}

pub fn cache_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "keyjitsu")
        .context("cannot determine a cache directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn cache_path(id: &LayoutId, geometry: &str) -> Result<PathBuf> {
    // Defend the persistence boundary even if a future caller constructs
    // LayoutId directly instead of going through the parsers.
    LayoutId::validate_part("layout hash", &id.hash)?;
    LayoutId::validate_part("revision", &id.revision)?;
    LayoutId::validate_part("geometry", geometry)?;
    // v3: revision-level Oryx combos added. Do not reuse v2 while connected:
    // a v2 cache silently makes every combo disappear from Live/Peek.
    Ok(cache_dir()?.join(format!(
        "layout-{geometry}-{}-{}-v3.json",
        id.hash, id.revision
    )))
}

fn read_layout_bytes(reader: impl Read, source: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_LAYOUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {source}"))?;
    if bytes.len() as u64 > MAX_LAYOUT_BYTES {
        bail!("{source} is larger than {MAX_LAYOUT_BYTES} bytes");
    }
    Ok(bytes)
}

fn read_layout_cache(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    read_layout_bytes(file, &format!("layout cache {}", path.display()))
}

/// Read a layout from the on-disk cache only, never touching the network.
/// Returns `None` if it isn't cached yet (or the cache can't be read). Used to
/// show the last-seen layout when no keyboard is plugged in.
pub fn cached_layout(id: &LayoutId, geometry: &str) -> Option<Layout> {
    let cache = cache_path(id, geometry).ok()?;
    let bytes = read_layout_cache(&cache).ok()?;
    parse_layout(&bytes).ok()
}

/// Find any layout already in the cache (newest first). Lets the GUI show a
/// real layout with no keyboard attached and no remembered serial yet (e.g.
/// after upgrading). Cache-only, never networks.
pub fn any_cached_layout(geometry: &str) -> Option<(LayoutId, Layout)> {
    LayoutId::validate_part("geometry", geometry).ok()?;
    let dir = cache_dir().ok()?;
    let prefix = format!("layout-{geometry}-");
    let mut hits: Vec<(std::time::SystemTime, std::path::PathBuf)> = fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with(&prefix)
                || !(name.ends_with("-v3.json") || name.ends_with("-v2.json"))
            {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    hits.into_iter().find_map(|(_, path)| {
        let bytes = read_layout_cache(&path).ok()?;
        let layout = parse_layout(&bytes).ok()?;
        if layout.geometry != geometry {
            return None;
        }
        let id = LayoutId::new(layout.hash_id.clone(), layout.revision.hash_id.clone()).ok()?;
        Some((id, layout))
    })
}

/// Fetch a layout, using the on-disk cache unless `refresh` is set.
/// `revision = "latest"` always goes to the network.
pub fn fetch_layout(id: &LayoutId, geometry: &str, refresh: bool) -> Result<Layout> {
    let cache = cache_path(id, geometry)?;
    let cacheable = id.revision != "latest";
    if cacheable && !refresh {
        if let Ok(bytes) = read_layout_cache(&cache) {
            if let Ok(layout) = parse_layout(&bytes) {
                return Ok(layout);
            }
        }
    }

    let body = serde_json::json!({
        "query": LAYOUT_QUERY,
        "variables": { "hashId": id.hash, "geometry": geometry, "revisionId": id.revision },
    });
    let response = ureq::post(ENDPOINT)
        .timeout(Duration::from_secs(8))
        .set("Content-Type", "application/json")
        .set(
            "User-Agent",
            concat!("keyjitsu/", env!("CARGO_PKG_VERSION")),
        )
        .send_json(body)
        .context("Oryx API request failed (offline? cached layouts still work)")?;
    let bytes = read_layout_bytes(response.into_reader(), "Oryx API response")?;
    let resp: serde_json::Value =
        serde_json::from_slice(&bytes).context("Oryx API returned malformed JSON")?;

    if let Some(errs) = resp.get("errors").and_then(|e| e.as_array()) {
        let msgs: Vec<String> = errs
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()).map(String::from))
            .collect();
        bail!("Oryx API error for layout {}: {}", id.hash, msgs.join("; "));
    }

    let raw = resp
        .get("data")
        .and_then(|d| d.get("layout"))
        .filter(|l| !l.is_null())
        .ok_or_else(|| anyhow!("layout {} not found on Oryx (is it private?)", id.hash))?
        .clone();

    let bytes = serde_json::to_vec(&raw)?;
    let layout = parse_layout(&bytes)?;
    if cacheable {
        if let Err(e) = crate::config::write_atomic(&cache, &bytes) {
            eprintln!("keyjitsu: could not cache Oryx layout {}: {e:#}", id.hash);
        }
    }
    Ok(layout)
}

fn parse_layout(bytes: &[u8]) -> Result<Layout> {
    serde_json::from_slice(bytes).context("unexpected layout JSON shape")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_id_from_serial() {
        let id = LayoutId::from_serial("xBrnx/wODgzD").unwrap();
        assert_eq!(id.hash, "xBrnx");
        assert_eq!(id.revision, "wODgzD");
        let keyed = LayoutId::from_serial("xBrnx/wODgzD~kj0123456789").unwrap();
        assert_eq!(keyed.hash, "xBrnx");
        assert_eq!(keyed.revision, "wODgzD");
        assert!(LayoutId::from_serial("garbage").is_err());
        assert!(LayoutId::from_serial("layout/rev/../../escape").is_err());
        assert!(LayoutId::from_serial("layout/..").is_err());
        assert!(LayoutId::from_serial("layout/rev with spaces").is_err());
    }

    #[test]
    fn layout_id_from_url() {
        let id =
            LayoutId::from_url("https://configure.zsa.io/voyager/layouts/xBrnx/latest/0").unwrap();
        assert_eq!(id.hash, "xBrnx");
        assert_eq!(id.revision, "latest");
        let id2 = LayoutId::from_url("https://configure.zsa.io/voyager/layouts/AbCdE").unwrap();
        assert_eq!(id2.revision, "latest");
        assert!(
            LayoutId::from_url("https://configure.zsa.io/voyager/layouts/../../escape").is_err()
        );
    }

    #[test]
    fn rejects_oversized_layout_payloads() {
        let bytes = vec![b'x'; MAX_LAYOUT_BYTES as usize + 1];
        assert!(read_layout_bytes(std::io::Cursor::new(bytes), "test layout").is_err());
    }

    #[test]
    fn modifier_chords_have_one_canonical_qmk_form() {
        let mask: KeyAction =
            serde_json::from_str(r#"{"code":"KC_TAB","modifiers":{"leftAlt":true}}"#).unwrap();
        assert_eq!(mask.qmk_code().as_deref(), Some("LALT(KC_TAB)"));
        assert!(mask.roundtrip_safe());

        let singular: KeyAction =
            serde_json::from_str(r#"{"code":"KC_TAB","modifier":"LALT"}"#).unwrap();
        assert_eq!(singular.qmk_code().as_deref(), Some("LALT(KC_TAB)"));
        assert!(singular.roundtrip_safe());

        let long_form: KeyAction =
            serde_json::from_str(r#"{"code":"KC_TAB","modifier":"left_alt"}"#).unwrap();
        assert_eq!(long_form.qmk_code().as_deref(), Some("LALT(KC_TAB)"));
    }

    #[test]
    fn unknown_oryx_action_fields_are_preserved_and_not_editable() {
        let action: KeyAction =
            serde_json::from_str(r#"{"code":"KC_A","futureBehavior":{"kind":"new"}}"#).unwrap();
        assert!(action.extra.contains_key("futureBehavior"));
        assert!(!action.roundtrip_safe());

        let encoded = serde_json::to_value(&action).unwrap();
        assert!(encoded.get("futureBehavior").is_some());
    }

    #[test]
    fn parses_real_layout_shape() {
        let json = r##"{
          "hashId": "xBrnx", "title": "Workhorse", "geometry": "voyager",
          "revision": {
            "hashId": "wODgzD", "title": "edit", "model": "v1",
            "layers": [{ "title": "Main", "position": 0, "keys": [
              {"tap": {"code": "KC_ESCAPE", "layer": null}, "hold": {"code": "KC_GRAVE"},
               "glowColor": "#C30CFF", "customLabel": null},
              {"tap": {"code": "KC_TAB", "modifiers": {"leftAlt": true}}},
              {"tap": {"code": "TO", "layer": 2}}
            ]}],
            "combos": [
              {"keyIndices": [1, 2], "layerIdx": 0,
               "trigger": {"code": "KC_TAB", "modifier": "LALT"}}
            ]
          }
        }"##;
        let l: Layout = serde_json::from_str(json).unwrap();
        assert_eq!(l.revision.layers[0].keys.len(), 3);
        assert_eq!(
            l.revision.layers[0].keys[0]
                .tap
                .as_ref()
                .unwrap()
                .code
                .as_deref(),
            Some("KC_ESCAPE")
        );
        assert_eq!(
            l.revision.layers[0].keys[2].tap.as_ref().unwrap().layer,
            Some(2)
        );
        assert_eq!(
            l.revision.layers[0].keys[1]
                .tap
                .as_ref()
                .and_then(|a| a.modifiers.as_ref())
                .map(|m| m.left_alt),
            Some(true)
        );
        assert_eq!(l.revision.combos.len(), 1);
        assert_eq!(l.revision.combos[0].key_indices, vec![1, 2]);
        assert_eq!(l.revision.combos[0].layer_idx, 0);
        assert_eq!(
            l.revision.combos[0]
                .trigger
                .as_ref()
                .and_then(KeyAction::qmk_code)
                .as_deref(),
            Some("LALT(KC_TAB)")
        );
    }
}
