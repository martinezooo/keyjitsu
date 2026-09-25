//! Client for the Oryx GraphQL API (layout definitions) with a disk cache.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://oryx.zsa.io/graphql";

const LAYOUT_QUERY: &str = r#"query Layout($hashId: String!, $geometry: String!, $revisionId: String!) {
  layout(hashId: $hashId, geometry: $geometry, revisionId: $revisionId) {
    hashId title geometry
    revision { hashId title model layers { title position color keys } }
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
pub struct KeyAction {
    /// QMK keycode (`KC_A`) or a layer-switch family (`TO`, `MO`, `LT`, …).
    pub code: Option<String>,
    /// Target layer for layer-switch codes.
    pub layer: Option<u8>,
    pub description: Option<String>,
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

    pub fn from_serial(serial: &str) -> Result<LayoutId> {
        match serial.split_once('/') {
            Some((h, r)) if !h.is_empty() && !r.is_empty() => {
                let revision = r.split_once(crate::firmware_state::SERIAL_MARKER).map(|(rev, _)| rev).unwrap_or(r);
                if revision.is_empty() {
                    bail!("keyboard serial {serial:?} has an empty revision before the Keyjitsu state marker");
                }
                Self::validate_part("layout hash", h)?;
                Self::validate_part("revision", revision)?;
                Ok(LayoutId {
                    hash: h.to_string(),
                    revision: revision.to_string(),
                })
            },
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
        Self::validate_part("layout hash", hash)?;
        Self::validate_part("revision", revision)?;
        Ok(LayoutId {
            hash: hash.to_string(),
            revision: revision.to_string(),
        })
    }
}

pub fn cache_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "keyjitsu")
        .context("cannot determine a cache directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn cache_path(id: &LayoutId, geometry: &str) -> Result<PathBuf> {
    // v2: layer `color` added to the query - old caches lack it.
    Ok(cache_dir()?.join(format!("layout-{geometry}-{}-{}-v2.json", id.hash, id.revision)))
}

/// Read a layout from the on-disk cache only, never touching the network.
/// Returns `None` if it isn't cached yet (or the cache can't be read). Used to
/// show the last-seen layout when no keyboard is plugged in.
pub fn cached_layout(id: &LayoutId, geometry: &str) -> Option<Layout> {
    let cache = cache_path(id, geometry).ok()?;
    let bytes = fs::read(cache).ok()?;
    parse_layout(&bytes).ok()
}

/// Find any layout already in the cache (newest first). Lets the GUI show a
/// real layout with no keyboard attached and no remembered serial yet (e.g.
/// after upgrading). Cache-only, never networks.
pub fn any_cached_layout(geometry: &str) -> Option<(LayoutId, Layout)> {
    let dir = cache_dir().ok()?;
    let prefix = format!("layout-{geometry}-");
    let mut hits: Vec<(std::time::SystemTime, LayoutId)> = fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            // layout-<geometry>-<hash>-<revision>-v2.json
            let rest = name.strip_prefix(&prefix)?.strip_suffix("-v2.json")?;
            let (hash, revision) = rest.rsplit_once('-')?;
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, LayoutId { hash: hash.to_string(), revision: revision.to_string() }))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    hits.into_iter().find_map(|(_, id)| cached_layout(&id, geometry).map(|l| (id, l)))
}

/// Fetch a layout, using the on-disk cache unless `refresh` is set.
/// `revision = "latest"` always goes to the network.
pub fn fetch_layout(id: &LayoutId, geometry: &str, refresh: bool) -> Result<Layout> {
    let cache = cache_path(id, geometry)?;
    let cacheable = id.revision != "latest";
    if cacheable && !refresh {
        if let Ok(bytes) = fs::read(&cache) {
            if let Ok(layout) = parse_layout(&bytes) {
                return Ok(layout);
            }
        }
    }

    let body = serde_json::json!({
        "query": LAYOUT_QUERY,
        "variables": { "hashId": id.hash, "geometry": geometry, "revisionId": id.revision },
    });
    let resp: serde_json::Value = ureq::post(ENDPOINT)
        .timeout(Duration::from_secs(8))
        .set("Content-Type", "application/json")
        .set("User-Agent", concat!("keyjitsu/", env!("CARGO_PKG_VERSION")))
        .send_json(body)
        .context("Oryx API request failed (offline? cached layouts still work)")?
        .into_json()
        .context("Oryx API returned malformed JSON")?;

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
        assert!(LayoutId::from_url(
            "https://configure.zsa.io/voyager/layouts/../../escape"
        )
        .is_err());
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
              {"tap": {"code": "TO", "layer": 2}}
            ]}]
          }
        }"##;
        let l: Layout = serde_json::from_str(json).unwrap();
        assert_eq!(l.revision.layers[0].keys.len(), 2);
        assert_eq!(
            l.revision.layers[0].keys[0].tap.as_ref().unwrap().code.as_deref(),
            Some("KC_ESCAPE")
        );
        assert_eq!(l.revision.layers[0].keys[1].tap.as_ref().unwrap().layer, Some(2));
    }
}
