//! Key-press counters per layout/layer/key, persisted as JSON in the app's
//! data directory. Populated while `live` (or `watch --heatmap`) runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::oryx_api::cache_dir;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HeatmapStore {
    /// layer → per-key counts (index = Oryx/LAYOUT key index).
    pub layers: BTreeMap<u8, Vec<u64>>,
    #[serde(skip)]
    dirty: u32,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl HeatmapStore {
    fn path_for(layout_hash: &str) -> Result<PathBuf> {
        Ok(cache_dir()?.join(format!("heatmap-{layout_hash}.json")))
    }

    pub fn load(layout_hash: &str, key_count: usize) -> Result<HeatmapStore> {
        let path = Self::path_for(layout_hash)?;
        let mut store: HeatmapStore = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HeatmapStore::default(),
        };
        for counts in store.layers.values_mut() {
            counts.resize(key_count, 0);
        }
        store.path = Some(path);
        Ok(store)
    }

    pub fn record(&mut self, layer: u8, key_idx: usize, key_count: usize) {
        let counts = self.layers.entry(layer).or_insert_with(|| vec![0; key_count]);
        if key_idx < counts.len() {
            counts[key_idx] += 1;
            self.dirty += 1;
        }
    }

    /// Persist now.
    pub fn save(&mut self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let bytes = serde_json::to_vec(self)?;
        fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        self.dirty = 0;
        Ok(())
    }

    /// Persist if enough new presses accumulated (call from hot loops).
    pub fn autosave(&mut self) -> Result<()> {
        if self.dirty >= 200 {
            self.save()?;
        }
        Ok(())
    }

    pub fn reset(layout_hash: &str) -> Result<bool> {
        let path = Self::path_for(layout_hash)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// Counts for one layer, or summed across layers if `layer` is `None`.
    pub fn counts(&self, layer: Option<u8>, key_count: usize) -> Vec<u64> {
        match layer {
            Some(l) => self
                .layers
                .get(&l)
                .cloned()
                .unwrap_or_else(|| vec![0; key_count]),
            None => {
                let mut total = vec![0u64; key_count];
                for counts in self.layers.values() {
                    for (t, c) in total.iter_mut().zip(counts) {
                        *t += c;
                    }
                }
                total
            }
        }
    }

    pub fn total_presses(&self) -> u64 {
        self.layers.values().flatten().sum()
    }
}

/// Normalize raw counts to 0.0..=1.0 against the max.
pub fn normalize(counts: &[u64]) -> Vec<f64> {
    let max = counts.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return vec![0.0; counts.len()];
    }
    counts.iter().map(|&c| c as f64 / max as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_sum() {
        let mut s = HeatmapStore::default();
        s.record(0, 3, 52);
        s.record(0, 3, 52);
        s.record(1, 3, 52);
        assert_eq!(s.counts(Some(0), 52)[3], 2);
        assert_eq!(s.counts(None, 52)[3], 3);
        assert_eq!(s.total_presses(), 3);
    }

    #[test]
    fn normalize_handles_zero() {
        assert_eq!(normalize(&[0, 0]), vec![0.0, 0.0]);
        assert_eq!(normalize(&[1, 2]), vec![0.5, 1.0]);
    }
}
