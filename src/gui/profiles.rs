use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use crate::config;

fn profiles_dir() -> Result<PathBuf> {
    Ok(crate::oryx_api::cache_dir()?.join("profiles"))
}

pub(super) fn profile_file_name(name: &str) -> Result<String> {
    let name = name.trim();
    let valid = !name.is_empty()
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_');
    let upper = name.to_ascii_uppercase();
    let windows_reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0');
    if !valid || windows_reserved {
        return Err(anyhow!("invalid profile name"));
    }
    Ok(format!("{name}.json"))
}

pub(super) fn profile_path(name: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(profile_file_name(name)?))
}

/// Sorted names of saved profiles. A missing directory is a valid empty
/// profile set; other filesystem failures are surfaced instead of pretending
/// that all profiles disappeared.
pub(super) fn list_profiles() -> Result<Vec<String>> {
    let dir = profiles_dir()?;
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.with_context(|| format!("reading an entry in {}", dir.display()))?;
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) == Some("json") {
            if let Some(name) = p.file_stem().and_then(|x| x.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

pub(super) fn snapshot_profile(name: &str) -> Result<()> {
    let path = profile_path(name)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let cfg = config::load_checked()?;
    let profile = config::Profile::from_config(&cfg);
    let json = serde_json::to_vec_pretty(&profile)?;
    config::write_atomic(&path, &json)?;
    Ok(())
}

pub(super) fn load_profile(name: &str) -> Result<config::Profile> {
    let bytes = std::fs::read(profile_path(name)?)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn create_profile(name: &str) -> Result<()> {
    if list_profiles()?
        .iter()
        .any(|saved| saved.eq_ignore_ascii_case(name.trim()))
    {
        return Err(anyhow!("profile {name:?} already exists"));
    }
    snapshot_profile(name)
}

pub(super) fn next_profile_copy_name(source: &str) -> Result<String> {
    let saved = list_profiles()?;
    for n in 1..=999 {
        let candidate = if n == 1 {
            format!("{source} copy")
        } else {
            format!("{source} copy {n}")
        };
        if profile_file_name(&candidate).is_ok()
            && !saved
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&candidate))
        {
            return Ok(candidate);
        }
    }
    Ok(format!("profile copy {}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::profile_file_name;

    #[test]
    fn profile_names_do_not_collapse_to_the_same_file() {
        assert_eq!(profile_file_name("work").unwrap(), "work.json");
        assert_eq!(profile_file_name("work copy").unwrap(), "work copy.json");
        assert!(profile_file_name("work/dev").is_err());
        assert!(profile_file_name("work?dev").is_err());
        assert!(profile_file_name("").is_err());
        assert!(profile_file_name("CON").is_err());
        assert!(profile_file_name("com1").is_err());
    }
}
