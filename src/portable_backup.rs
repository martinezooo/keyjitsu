//! Portable, bounded backup/restore for user-authored Keyjitsu data.
//!
//! The archive deliberately excludes arbitrary files from the data directory
//! (logs, recordings, signing experiments, and recovery copies). Restore is a
//! merge: differing existing files are preserved before replacement and no
//! user file is deleted.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;

use crate::config;
use crate::firmware_state::FirmwareState;

const FORMAT: &str = "keyjitsu-portable-backup";
const FORMAT_VERSION: u32 = 1;
const MANIFEST_PATH: &str = "keyjitsu-backup.json";
const DATA_PREFIX: &str = "data/";
const PENDING_NAME: &str = ".pending-import.keyjitsu-backup.zip";
const REPORT_NAME: &str = ".last-import-report.json";
const MAX_ENTRIES: usize = 2048;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    format: String,
    version: u32,
    app_version: String,
    created_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportReport {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Default)]
struct ApplySummary {
    installed: usize,
    unchanged: usize,
    preserved: usize,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn suggested_file_name() -> String {
    format!("keyjitsu-backup-{}.zip", unix_now())
}

fn is_root_data_file(name: &str) -> bool {
    name == "config.json"
        || (name.starts_with("layout-") && name.ends_with(".json"))
        || (name.starts_with("source-") && name.ends_with(".zip"))
        || (name.starts_with("heatmap-") && name.ends_with(".json"))
}

fn allowed_relative_path(path: &Path) -> bool {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return false;
    }
    let parts: Vec<_> = path.components().collect();
    match parts.as_slice() {
        [Component::Normal(name)] => is_root_data_file(&name.to_string_lossy()),
        [Component::Normal(dir), Component::Normal(name)] => {
            matches!(dir.to_str(), Some("profiles" | "firmware-states"))
                && name.to_string_lossy().ends_with(".json")
        }
        _ => false,
    }
}

fn collect_files(data_dir: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut relative = Vec::new();
    if !data_dir.exists() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(data_dir)
        .with_context(|| format!("reading Keyjitsu data folder {}", data_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let rel = PathBuf::from(&name);
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_file() && allowed_relative_path(&rel) {
            relative.push(rel);
        } else if meta.is_dir() && matches!(name.to_str(), Some("profiles" | "firmware-states")) {
            for child in fs::read_dir(entry.path())? {
                let child = child?;
                let child_meta = fs::symlink_metadata(child.path())?;
                let child_rel = rel.join(child.file_name());
                if child_meta.is_file()
                    && !child_meta.file_type().is_symlink()
                    && allowed_relative_path(&child_rel)
                {
                    relative.push(child_rel);
                }
            }
        }
    }
    relative.sort();
    if relative.len() > MAX_ENTRIES {
        bail!("backup contains too many files");
    }

    let mut total = 0usize;
    let mut out = Vec::with_capacity(relative.len());
    for rel in relative {
        let bytes = fs::read(data_dir.join(&rel))
            .with_context(|| format!("reading backup item {}", rel.display()))?;
        if bytes.len() > MAX_FILE_BYTES {
            bail!("backup item {} is larger than 64 MiB", rel.display());
        }
        total = total
            .checked_add(bytes.len())
            .context("backup size overflow")?;
        if total > MAX_TOTAL_BYTES {
            bail!("portable backup is larger than 256 MiB");
        }
        out.push((rel, bytes));
    }
    Ok(out)
}

pub fn export_to(destination: &Path, data_dir: &Path) -> Result<usize> {
    let files = collect_files(data_dir)?;
    if files.is_empty() {
        bail!("there is no Keyjitsu data to back up yet");
    }
    let cursor = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let manifest = BackupManifest {
        format: FORMAT.to_string(),
        version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created_unix: unix_now(),
    };
    zip.start_file(MANIFEST_PATH, options)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for (rel, bytes) in &files {
        let archive_name = format!("{DATA_PREFIX}{}", rel.to_string_lossy().replace('\\', "/"));
        zip.start_file(archive_name, options)?;
        zip.write_all(bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    config::write_atomic(destination, &bytes)
        .with_context(|| format!("writing backup {}", destination.display()))?;
    Ok(files.len())
}

fn read_archive(bytes: &[u8]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    if bytes.len() > MAX_TOTAL_BYTES {
        bail!("backup archive is larger than 256 MiB");
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("opening backup archive")?;
    if zip.len() > MAX_ENTRIES + 1 {
        bail!("backup archive contains too many entries");
    }

    let manifest: BackupManifest = {
        let mut entry = zip
            .by_name(MANIFEST_PATH)
            .context("backup manifest is missing")?;
        if entry.size() as usize > MAX_FILE_BYTES {
            bail!("backup manifest is too large");
        }
        let mut raw = Vec::new();
        entry.read_to_end(&mut raw)?;
        serde_json::from_slice(&raw).context("reading backup manifest")?
    };
    if manifest.format != FORMAT || manifest.version != FORMAT_VERSION {
        bail!("this is not a supported Keyjitsu portable backup");
    }

    let mut total = 0usize;
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let entry_name = entry.name()?.into_owned();
        if entry_name == MANIFEST_PATH || entry.is_dir() {
            continue;
        }
        let Some(name) = entry_name.strip_prefix(DATA_PREFIX) else {
            bail!("unexpected archive entry {entry_name}");
        };
        let rel = PathBuf::from(name);
        if !allowed_relative_path(&rel) || entry.enclosed_name().is_none() {
            bail!("unsafe or unsupported backup entry {entry_name}");
        }
        let size = usize::try_from(entry.size()).context("backup item is too large")?;
        if size > MAX_FILE_BYTES {
            bail!("backup item {entry_name} is larger than 64 MiB");
        }
        total = total.checked_add(size).context("backup size overflow")?;
        if total > MAX_TOTAL_BYTES {
            bail!("expanded backup is larger than 256 MiB");
        }
        let mut raw = Vec::with_capacity(size);
        entry.read_to_end(&mut raw)?;
        validate_item(&rel, &raw)?;
        files.push((rel, raw));
    }
    if files.is_empty() {
        bail!("backup contains no portable Keyjitsu data");
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("backup contains duplicate data paths");
    }
    Ok(files)
}

fn validate_item(rel: &Path, bytes: &[u8]) -> Result<()> {
    if rel == Path::new("config.json") {
        return config::validate_portable_bytes(bytes)
            .context("backup contains an unsupported config");
    }
    let parent = rel.parent().and_then(Path::to_str);
    if matches!(parent, Some("profiles" | "firmware-states")) {
        let state: FirmwareState = serde_json::from_slice(bytes)
            .with_context(|| format!("backup item {} is not firmware state", rel.display()))?;
        if parent == Some("firmware-states") {
            let expected = rel.file_stem().and_then(|name| name.to_str()).unwrap_or("");
            let actual = state.state_id()?;
            if expected != actual {
                bail!(
                    "firmware-state file {} has identity {actual}, expected {expected}",
                    rel.display()
                );
            }
        }
        return Ok(());
    }
    if rel.extension().and_then(|ext| ext.to_str()) == Some("json") {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .with_context(|| format!("backup item {} is not valid JSON", rel.display()))?;
    } else if rel.extension().and_then(|ext| ext.to_str()) == Some("zip") {
        zip::ZipArchive::new(Cursor::new(bytes))
            .with_context(|| format!("backup item {} is not a valid ZIP", rel.display()))?;
    }
    Ok(())
}

fn unique_timestamped_path(dir: &Path, prefix: &str, extension: &str) -> PathBuf {
    let stamp = unix_now();
    let mut suffix = 0u32;
    loop {
        let name = if suffix == 0 {
            format!("{prefix}-{stamp}.{extension}")
        } else {
            format!("{prefix}-{stamp}-{suffix}.{extension}")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn archive_existing_pending(data_dir: &Path) -> Result<()> {
    let pending = data_dir.join(PENDING_NAME);
    if !pending.exists() {
        return Ok(());
    }
    let imports = data_dir.join("imports");
    fs::create_dir_all(&imports)?;
    let destination = unique_timestamped_path(&imports, "replaced-pending", "zip");
    fs::rename(&pending, &destination).with_context(|| {
        format!(
            "preserving previous pending import in {}",
            destination.display()
        )
    })?;
    Ok(())
}

pub fn stage_import(source: &Path, data_dir: &Path) -> Result<usize> {
    let bytes = fs::read(source).with_context(|| format!("reading backup {}", source.display()))?;
    let count = read_archive(&bytes)?.len();
    fs::create_dir_all(data_dir)?;
    archive_existing_pending(data_dir)?;
    config::write_atomic(&data_dir.join(PENDING_NAME), &bytes)?;
    Ok(count)
}

pub fn has_pending_import(data_dir: &Path) -> bool {
    data_dir.join(PENDING_NAME).is_file()
}

fn next_preserved_path(path: &Path, stamp: u64) -> PathBuf {
    let mut suffix = 0u32;
    loop {
        let ending = if suffix == 0 {
            format!("before-import-{stamp}")
        } else {
            format!("before-import-{stamp}-{suffix}")
        };
        let candidate = PathBuf::from(format!("{}.{}", path.display(), ending));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn mirror_firmware_state(rel: &Path, bytes: &[u8]) -> Result<()> {
    let components = rel.components().collect::<Vec<_>>();
    let [Component::Normal(dir), Component::Normal(name)] = components.as_slice() else {
        return Ok(());
    };
    if dir.to_str() != Some("firmware-states") {
        return Ok(());
    }
    let home = directories::UserDirs::new().context("cannot determine home directory")?;
    let destination = home.home_dir().join(".keyjitsu/firmware-states").join(name);
    config::write_atomic(&destination, bytes)
}

fn write_report(data_dir: &Path, report: &ImportReport) -> Result<()> {
    config::write_atomic(
        &data_dir.join(REPORT_NAME),
        &serde_json::to_vec_pretty(report)?,
    )
}

pub fn apply_pending_import(data_dir: &Path) -> Result<Option<ImportReport>> {
    let pending = data_dir.join(PENDING_NAME);
    let bytes = match fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading pending portable backup"),
    };
    let result = (|| -> Result<ApplySummary> {
        let files = read_archive(&bytes)?;
        let stamp = unix_now();
        let mut summary = ApplySummary::default();
        for (rel, incoming) in files {
            let destination = data_dir.join(&rel);
            match fs::read(&destination) {
                Ok(existing) if existing == incoming => {
                    summary.unchanged += 1;
                    mirror_firmware_state(&rel, &incoming)?;
                    continue;
                }
                Ok(existing) => {
                    let preserved = next_preserved_path(&destination, stamp);
                    config::write_atomic(&preserved, &existing).with_context(|| {
                        format!("preserving existing {}", destination.display())
                    })?;
                    summary.preserved += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("reading existing {}", destination.display()));
                }
            }
            config::write_atomic(&destination, &incoming)
                .with_context(|| format!("restoring {}", destination.display()))?;
            mirror_firmware_state(&rel, &incoming)?;
            summary.installed += 1;
        }
        Ok(summary)
    })();

    let report = match result {
        Ok(summary) => ImportReport {
            ok: true,
            message: format!(
                "Backup restored: {} installed, {} unchanged, {} previous version{} preserved.",
                summary.installed,
                summary.unchanged,
                summary.preserved,
                if summary.preserved == 1 { "" } else { "s" }
            ),
        },
        Err(error) => ImportReport {
            ok: false,
            message: format!("Backup restore failed: {error:#}"),
        },
    };
    write_report(data_dir, &report)?;

    if report.ok {
        let imports = data_dir.join("imports");
        fs::create_dir_all(&imports)?;
        let destination = unique_timestamped_path(&imports, "applied", "zip");
        fs::rename(&pending, &destination)
            .with_context(|| format!("archiving applied backup in {}", destination.display()))?;
    }
    Ok(Some(report))
}

pub fn take_last_report(data_dir: &Path) -> Result<Option<ImportReport>> {
    let path = data_dir.join(REPORT_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading last import report"),
    };
    let report: ImportReport = serde_json::from_slice(&bytes)?;
    let reports = data_dir.join("imports/reports");
    fs::create_dir_all(&reports)?;
    let destination = unique_timestamped_path(&reports, "report", "json");
    fs::rename(&path, &destination)
        .with_context(|| format!("archiving import report in {}", destination.display()))?;
    Ok(Some(report))
}

pub fn record_failure(data_dir: &Path, message: String) {
    let _ = write_report(data_dir, &ImportReport { ok: false, message });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_restore_preserves_conflicts_and_excludes_unrelated_files() {
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("config.json"),
            br#"{"schema_version":1,"show_cpu_header":true}"#,
        )
        .unwrap();
        fs::create_dir_all(source.path().join("profiles")).unwrap();
        fs::write(
            source.path().join("profiles/work.json"),
            br#"{"layout_hash":"x","revision":"y","edits":[],"dances":[],"custom_layers":[]}"#,
        )
        .unwrap();
        fs::create_dir_all(source.path().join("voice-notes")).unwrap();
        fs::write(source.path().join("voice-notes/private.pem"), b"secret").unwrap();

        let archive_dir = tempfile::tempdir().unwrap();
        let archive = archive_dir.path().join("backup.zip");
        assert_eq!(export_to(&archive, source.path()).unwrap(), 2);

        let target = tempfile::tempdir().unwrap();
        fs::write(
            target.path().join("config.json"),
            br#"{"schema_version":1,"show_cpu_header":false}"#,
        )
        .unwrap();
        assert_eq!(stage_import(&archive, target.path()).unwrap(), 2);
        let report = apply_pending_import(target.path()).unwrap().unwrap();
        assert!(report.ok);
        assert_eq!(
            fs::read(target.path().join("config.json")).unwrap(),
            br#"{"schema_version":1,"show_cpu_header":true}"#
        );
        assert!(fs::read_dir(target.path())
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("before-import")));
        assert!(!target.path().join("voice-notes/private.pem").exists());
        assert!(target.path().join("profiles/work.json").exists());
    }

    #[test]
    fn rejects_archive_path_traversal() {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();
        let manifest = BackupManifest {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
            app_version: "test".into(),
            created_unix: 0,
        };
        zip.start_file(MANIFEST_PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.start_file("data/../config.json", options).unwrap();
        zip.write_all(b"{}").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(read_archive(&bytes).is_err());
    }

    #[test]
    fn supported_path_allowlist_is_narrow() {
        assert!(allowed_relative_path(Path::new("config.json")));
        assert!(allowed_relative_path(Path::new("profiles/work.json")));
        assert!(allowed_relative_path(Path::new("firmware-states/abc.json")));
        assert!(allowed_relative_path(Path::new(
            "layout-voyager-x-y-v3.json"
        )));
        assert!(allowed_relative_path(Path::new("source-rev.zip")));
        assert!(!allowed_relative_path(Path::new("voice-notes/key.pem")));
        assert!(!allowed_relative_path(Path::new("../config.json")));
        assert!(!allowed_relative_path(Path::new(
            "profiles/nested/work.json"
        )));
    }
}
