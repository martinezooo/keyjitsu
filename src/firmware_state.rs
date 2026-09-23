//! Automatic cache of the exact Keyjitsu-authored state compiled into firmware.
//!
//! This is deliberately separate from user profiles/config snapshots. The keyboard
//! identifies the running build via a short marker embedded in its USB serial;
//! this cache maps that marker back to the exact state that produced the binary.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::CustomLayer;
use crate::oryx_api::cache_dir;

pub const STATE_ID_HEX_LEN: usize = 10;
pub const SERIAL_MARKER: &str = "~kj";

/// Extract the Keyjitsu firmware-state marker from the USB serial, if present.
pub fn state_id_from_serial(serial: &str) -> Option<&str> {
    let (_, tail) = serial.rsplit_once(SERIAL_MARKER)?;
    (tail.len() == STATE_ID_HEX_LEN && tail.bytes().all(|b| b.is_ascii_hexdigit())).then_some(tail)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareEdit {
    pub layer: u8,
    pub key: u16,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareDance {
    pub layer: u8,
    pub key: u16,
    pub slots: [Option<String>; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirmwareState {
    pub layout_hash: String,
    pub revision: String,
    pub edits: Vec<FirmwareEdit>,
    pub dances: Vec<FirmwareDance>,
    pub custom_layers: Vec<CustomLayer>,
}

impl FirmwareState {
    pub fn new(
        layout_hash: String,
        revision: String,
        mut edits: Vec<FirmwareEdit>,
        mut dances: Vec<FirmwareDance>,
        mut custom_layers: Vec<CustomLayer>,
    ) -> Self {
        edits.sort_by_key(|e| (e.layer, e.key));
        dances.sort_by_key(|d| (d.layer, d.key));
        for layer in &mut custom_layers {
            layer.keys.sort_by_key(|k| k.key);
        }
        Self {
            layout_hash,
            revision,
            edits,
            dances,
            custom_layers,
        }
    }

    /// Stable, compact identity for the complete authored firmware state.
    /// FNV-1a is used as an identity checksum, not for security.
    pub fn state_id(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("FirmwareState is serializable");
        let mut hash = 0xcbf29ce484222325u64;
        for b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{:010x}", hash & 0xffffffffff)
    }

    pub fn save(&self) -> Result<String> {
        let id = self.state_id();
        let dir = cache_dir()?.join("firmware-states");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("{id}.json"));
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("installing {}", path.display()))?;
        Ok(id)
    }

    pub fn load(id: &str) -> Option<Self> {
        if id.len() != STATE_ID_HEX_LEN || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let path = cache_dir().ok()?.join("firmware-states").join(format!("{id}.json"));
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_is_order_independent_for_key_entries() {
        let a = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![
                FirmwareEdit { layer: 1, key: 4, code: "KC_B".into() },
                FirmwareEdit { layer: 0, key: 2, code: "KC_A".into() },
            ],
            Vec::new(),
            Vec::new(),
        );
        let b = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![
                FirmwareEdit { layer: 0, key: 2, code: "KC_A".into() },
                FirmwareEdit { layer: 1, key: 4, code: "KC_B".into() },
            ],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(a.state_id(), b.state_id());
        assert_eq!(a.state_id().len(), STATE_ID_HEX_LEN);
    }
}
