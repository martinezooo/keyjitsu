//! Built-in shortcut cheatsheet: a curated CSV of ready-made shortcuts
//! (macOS, editors, terminals, pentest tools, QMK patterns…) shown in the
//! GUI's Shortcuts tab. User-added entries live in the config instead.

use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutDef {
    pub category: String,
    pub keys: String,
    pub desc: String,
    /// true = "High" usefulness, false = "Medium".
    pub high: bool,
}

/// Parse one CSV line. The Shortcut column may itself contain commas
/// ("Cmd + ,"), so: category = first field, usefulness = last field,
/// description = second-to-last, keys = everything in between re-joined.
fn parse_line(line: &str) -> Option<ShortcutDef> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() < 4 {
        return None;
    }
    let category = parts[0].trim().to_string();
    let high = parts[parts.len() - 1].trim().eq_ignore_ascii_case("high");
    let desc = parts[parts.len() - 2].trim().to_string();
    let keys = parts[1..parts.len() - 2].join(",").trim().to_string();
    if category.is_empty() || keys.is_empty() {
        return None;
    }
    Some(ShortcutDef { category, keys, desc, high })
}

/// The embedded library, parsed once.
pub fn builtin() -> &'static [ShortcutDef] {
    static CACHE: OnceLock<Vec<ShortcutDef>> = OnceLock::new();
    CACHE.get_or_init(|| {
        include_str!("../resources/shortcuts.csv")
            .lines()
            .skip(1) // header
            .filter_map(parse_line)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_parses_completely() {
        let all = builtin();
        assert!(all.len() > 140, "expected the full library, got {}", all.len());
        // Every entry has a category and keys.
        assert!(all.iter().all(|s| !s.category.is_empty() && !s.keys.is_empty()));
    }

    #[test]
    fn comma_inside_shortcut_survives() {
        // "Cmd + ," (app preferences) must keep its comma.
        let hit = builtin().iter().find(|s| s.desc.contains("settings/preferences")).unwrap();
        assert_eq!(hit.keys, "Cmd + ,");
        assert!(hit.high);
    }

    #[test]
    fn categories_are_grouped() {
        let cats: Vec<&str> = {
            let mut v: Vec<&str> = builtin().iter().map(|s| s.category.as_str()).collect();
            v.dedup();
            v
        };
        // Dedup on the ordered list = one run per category (no interleaving).
        let mut sorted = cats.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(cats.len(), sorted.len(), "categories should be contiguous blocks");
    }
}
