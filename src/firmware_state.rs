//! Automatic cache of the exact Keyjitsu-authored state compiled into firmware.
//!
//! This is deliberately separate from user profiles/config snapshots. The keyboard
//! identifies the running build via a short marker embedded in its USB serial;
//! this cache maps that marker back to the exact state that produced the binary.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{self, CustomLayer};
use crate::key_action::canonicalize_qmk_code;
use crate::oryx_api::{cache_dir, LayoutId};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareGlow {
    pub layer: u8,
    pub key: u16,
    pub rgb: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirmwareState {
    pub layout_hash: String,
    pub revision: String,
    pub edits: Vec<FirmwareEdit>,
    pub dances: Vec<FirmwareDance>,
    pub custom_layers: Vec<CustomLayer>,
    #[serde(default)]
    pub glow: Vec<FirmwareGlow>,
}

impl FirmwareState {
    pub fn new(
        layout_hash: String,
        revision: String,
        mut edits: Vec<FirmwareEdit>,
        mut dances: Vec<FirmwareDance>,
        mut custom_layers: Vec<CustomLayer>,
        mut glow: Vec<FirmwareGlow>,
    ) -> Self {
        for edit in &mut edits {
            edit.code = canonicalize_qmk_code(&edit.code);
        }
        for dance in &mut dances {
            for slot in &mut dance.slots {
                if let Some(code) = slot.as_mut() {
                    *code = canonicalize_qmk_code(code);
                }
            }
        }
        for layer in &mut custom_layers {
            for key in &mut layer.keys {
                key.code = canonicalize_qmk_code(&key.code);
            }
        }
        edits.sort_by_key(|e| (e.layer, e.key));
        dances.sort_by_key(|d| (d.layer, d.key));
        for layer in &mut custom_layers {
            layer.keys.sort_by_key(|k| k.key);
        }
        glow.sort_by_key(|g| (g.layer, g.key));
        Self {
            layout_hash,
            revision,
            edits,
            dances,
            custom_layers,
            glow,
        }
    }

    pub fn needs_action_normalization(&self) -> bool {
        self.edits
            .iter()
            .any(|edit| canonicalize_qmk_code(&edit.code) != edit.code)
            || self.dances.iter().any(|dance| {
                dance
                    .slots
                    .iter()
                    .flatten()
                    .any(|code| canonicalize_qmk_code(code) != *code)
            })
            || self.custom_layers.iter().any(|layer| {
                layer
                    .keys
                    .iter()
                    .any(|key| canonicalize_qmk_code(&key.code) != key.code)
            })
    }

    /// Stable, compact identity for the complete authored firmware state.
    /// FNV-1a is used as an identity checksum, not for security.
    pub fn state_id(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).context("serializing firmware state identity")?;
        let mut hash = 0xcbf29ce484222325u64;
        for b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Ok(format!("{:010x}", hash & 0xffffffffff))
    }

    pub fn save(&self) -> Result<String> {
        let id = self.state_id()?;
        let dir = cache_dir()?.join("firmware-states");
        let path = dir.join(format!("{id}.json"));

        match std::fs::read(&path) {
            Ok(bytes) => {
                let existing: FirmwareState =
                    serde_json::from_slice(&bytes).with_context(|| {
                        format!("existing firmware state {} is unreadable", path.display())
                    })?;
                if existing != *self {
                    bail!("firmware state identity collision for {id}; refusing to overwrite");
                }
                return Ok(id);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }

        let bytes = serde_json::to_vec_pretty(self)?;
        config::write_atomic(&path, &bytes)
            .with_context(|| format!("persisting {}", path.display()))?;
        Ok(id)
    }

    pub fn load_checked(id: &str) -> Result<Option<Self>> {
        if id.len() != STATE_ID_HEX_LEN || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let path = cache_dir()?
            .join("firmware-states")
            .join(format!("{id}.json"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        match serde_json::from_slice(&bytes) {
            Ok(state) => Ok(Some(state)),
            Err(e) => {
                let backup = config::preserve_corrupt_bytes(&path, &bytes).with_context(|| {
                    format!(
                        "firmware state is unreadable ({e}); failed to preserve the original bytes"
                    )
                })?;
                Err(e).with_context(|| {
                    format!(
                        "firmware state is unreadable; preserved the original bytes in {}",
                        backup.display()
                    )
                })
            }
        }
    }

    pub fn load(id: &str) -> Option<Self> {
        match Self::load_checked(id) {
            Ok(state) => state,
            Err(e) => {
                eprintln!("keyjitsu: {e:#}");
                None
            }
        }
    }

    pub fn load_legacy_for_serial(serial: &str) -> Option<Self> {
        match Self::load_legacy_checked(serial) {
            Ok(state) => state,
            Err(e) => {
                eprintln!("keyjitsu: {e:#}");
                None
            }
        }
    }

    fn load_legacy_checked(serial: &str) -> Result<Option<Self>> {
        if state_id_from_serial(serial).is_some() {
            return Ok(None);
        }
        let id = LayoutId::from_serial(serial)?;
        let path = legacy_state_path(serial)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let state: FirmwareState = serde_json::from_slice(&bytes)
            .with_context(|| format!("reading legacy firmware state {}", path.display()))?;
        if state.layout_hash != id.hash || state.revision != id.revision {
            bail!(
                "legacy firmware state {} does not match connected layout {}/{}",
                path.display(),
                id.hash,
                id.revision
            );
        }
        Ok(Some(state))
    }
}

fn legacy_state_path(serial: &str) -> Result<std::path::PathBuf> {
    let id = LayoutId::from_serial(serial)?;
    if state_id_from_serial(serial).is_some() {
        bail!("marked firmware does not use legacy state");
    }
    let dir = cache_dir()?.join("legacy-firmware-states");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join(format!("{}-{}.json", id.hash, id.revision)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_marker_only_when_complete() {
        assert_eq!(
            state_id_from_serial("abc/rev~kj0123456789"),
            Some("0123456789")
        );
        assert_eq!(state_id_from_serial("abc/rev"), None);
        assert_eq!(state_id_from_serial("abc/rev~kj123"), None);
        assert_eq!(state_id_from_serial("abc/rev~kj012345678z"), None);
    }

    #[test]
    fn canonicalizes_custom_layer_key_order() {
        let a = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![],
            vec![],
            vec![CustomLayer {
                layout: "layout".into(),
                name: "Extra".into(),
                keys: vec![
                    crate::config::CustomKey {
                        key: 4,
                        code: "KC_B".into(),
                    },
                    crate::config::CustomKey {
                        key: 1,
                        code: "KC_A".into(),
                    },
                ],
            }],
            vec![],
        );
        let b = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![],
            vec![],
            vec![CustomLayer {
                layout: "layout".into(),
                name: "Extra".into(),
                keys: vec![
                    crate::config::CustomKey {
                        key: 1,
                        code: "KC_A".into(),
                    },
                    crate::config::CustomKey {
                        key: 4,
                        code: "KC_B".into(),
                    },
                ],
            }],
            vec![],
        );
        assert_eq!(a.state_id().unwrap(), b.state_id().unwrap());
    }

    #[test]
    fn state_id_is_order_independent_for_key_entries() {
        let a = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![
                FirmwareEdit {
                    layer: 1,
                    key: 4,
                    code: "KC_B".into(),
                },
                FirmwareEdit {
                    layer: 0,
                    key: 2,
                    code: "KC_A".into(),
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let b = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![
                FirmwareEdit {
                    layer: 0,
                    key: 2,
                    code: "KC_A".into(),
                },
                FirmwareEdit {
                    layer: 1,
                    key: 4,
                    code: "KC_B".into(),
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(a.state_id().unwrap(), b.state_id().unwrap());
        assert_eq!(a.state_id().unwrap().len(), STATE_ID_HEX_LEN);
    }

    #[test]
    fn state_id_is_order_independent_for_glow_entries() {
        let mk =
            |glow| FirmwareState::new("layout".into(), "rev".into(), vec![], vec![], vec![], glow);
        let a = mk(vec![
            FirmwareGlow {
                layer: 1,
                key: 9,
                rgb: [1, 2, 3],
            },
            FirmwareGlow {
                layer: 0,
                key: 2,
                rgb: [4, 5, 6],
            },
        ]);
        let b = mk(vec![
            FirmwareGlow {
                layer: 0,
                key: 2,
                rgb: [4, 5, 6],
            },
            FirmwareGlow {
                layer: 1,
                key: 9,
                rgb: [1, 2, 3],
            },
        ]);
        assert_eq!(a.state_id().unwrap(), b.state_id().unwrap());
    }

    #[test]
    fn canonicalizes_redundant_modifier_wrappers() {
        let state = FirmwareState::new(
            "layout".into(),
            "rev".into(),
            vec![FirmwareEdit {
                layer: 1,
                key: 6,
                code: "LALT(LALT(KC_TAB))".into(),
            }],
            vec![FirmwareDance {
                layer: 0,
                key: 2,
                slots: [Some("LGUI(LGUI(KC_A))".into()), None, None, None],
            }],
            vec![CustomLayer {
                layout: "layout".into(),
                name: "Extra".into(),
                keys: vec![crate::config::CustomKey {
                    key: 1,
                    code: "LSFT(LSFT(KC_B))".into(),
                }],
            }],
            vec![],
        );
        assert_eq!(state.edits[0].code, "LALT(KC_TAB)");
        assert_eq!(state.dances[0].slots[0].as_deref(), Some("LGUI(KC_A)"));
        assert_eq!(state.custom_layers[0].keys[0].code, "LSFT(KC_B)");
        assert!(!state.needs_action_normalization());
    }

    #[test]
    fn detects_noncanonical_loaded_state_without_rewriting_marker_identity() {
        let state = FirmwareState {
            layout_hash: "layout".into(),
            revision: "rev".into(),
            edits: vec![FirmwareEdit {
                layer: 1,
                key: 6,
                code: "LALT(LALT(KC_TAB))".into(),
            }],
            dances: vec![],
            custom_layers: vec![],
            glow: vec![],
        };
        assert!(state.needs_action_normalization());
    }
}
