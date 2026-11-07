//! macOS updater: stage a release archive, verify Apple's Developer ID and
//! notarization, then replace the app from a detached signed helper after quit.
//! Never overwrites a running app or installs an unsigned downloaded archive.
#![cfg(target_os = "macos")]

use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TEAM: &str = "TVQDVS8HK4";
const AUTHORITY: &str = "Authority=Developer ID Application: Marcin Bortkiewicz (TVQDVS8HK4)";
const BUNDLE_ID: &str = "Identifier=com.keyjitsu.gui";
const MAX_ARCHIVE_BYTES: u64 = 90 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PreparedUpdate {
    pub staged_app: PathBuf,
    pub target_app: PathBuf,
    pub release_tag: String,
}

fn run_checked(program: &str, args: &[&str], path: &Path) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .arg(path)
        .output()
        .with_context(|| format!("running {program}"))?;
    let output_text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(output.status.success(), "{program} failed: {output_text}");
    Ok(output_text)
}

fn check_bundle(path: &Path) -> Result<()> {
    ensure!(
        path.is_dir() && !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "update is not a regular app bundle"
    );
    run_checked(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict"],
        path,
    )
    .context("the candidate's code signature is invalid")?;
    let details = run_checked("/usr/bin/codesign", &["-d", "-vvv"], path)?;
    ensure!(
        details.lines().any(|v| v == BUNDLE_ID),
        "unexpected bundle identifier"
    );
    ensure!(
        details
            .lines()
            .any(|v| v == format!("TeamIdentifier={TEAM}")),
        "unexpected signing team"
    );
    ensure!(
        details.lines().any(|v| v == AUTHORITY),
        "candidate is not signed by the expected Developer ID"
    );
    run_checked("/usr/bin/xcrun", &["stapler", "validate"], path)
        .context("the candidate has no valid Apple notarization ticket")?;
    run_checked("/usr/sbin/spctl", &["--assess", "--type", "execute"], path)
        .context("macOS Gatekeeper rejected the candidate")?;
    Ok(())
}

fn existing_app_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating running Keyjitsu")?;
    ensure!(
        exe.file_name().is_some_and(|n| n == "keyjitsu"),
        "unexpected executable name"
    );
    let macos = exe.parent().context("missing MacOS bundle directory")?;
    ensure!(
        macos.file_name().is_some_and(|n| n == "MacOS"),
        "not running inside an app"
    );
    let contents = macos.parent().context("missing Contents directory")?;
    ensure!(
        contents.file_name().is_some_and(|n| n == "Contents"),
        "not running inside an app"
    );
    let app = contents
        .parent()
        .context("missing Keyjitsu.app directory")?;
    ensure!(
        app.file_name().is_some_and(|n| n == "Keyjitsu.app"),
        "unexpected app name"
    );
    ensure!(
        app.is_dir() && !fs::symlink_metadata(app)?.file_type().is_symlink(),
        "app is a symlink"
    );
    Ok(app.to_path_buf())
}

fn signed_version(tag: &str) -> Result<String> {
    let suffix = tag
        .strip_prefix("signed-macos-v")
        .context("not a signed macOS release")?;
    let (version, short_hash) = suffix.rsplit_once('-').context("missing release commit")?;
    ensure!(
        short_hash.len() >= 7 && short_hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid release commit"
    );
    ensure!(version.split('.').count() >= 3, "invalid version");
    Ok(version.to_string())
}

/// Bound the expanded ZIP and reject unexpected entries before invoking ditto.
/// A matching SHA-256 alone does not protect against release-account compromise.
fn inspect_archive(path: &Path) -> Result<()> {
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).context("reading update ZIP directory")?;
    ensure!(
        !archive.is_empty() && archive.len() <= 500,
        "unexpected ZIP entry count"
    );
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name()?.into_owned();
        ensure!(
            name.starts_with("Keyjitsu.app/")
                && !name.contains('\\')
                && entry.enclosed_name().is_some(),
            "unsafe or unexpected ZIP entry"
        );
        let file_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        ensure!(
            file_type == 0 || file_type == 0o100000 || file_type == 0o040000,
            "unexpected archive symlink or special file"
        );
        ensure!(entry.size() <= 150 * 1024 * 1024, "oversized ZIP entry");
        total = total
            .checked_add(entry.size())
            .context("ZIP size overflow")?;
        ensure!(
            total <= 300 * 1024 * 1024,
            "expanded update exceeds safety limit"
        );
    }
    Ok(())
}

fn download_release(url: &str, digest: &str, tag: &str) -> Result<PreparedUpdate> {
    let version = signed_version(tag)?;
    let prefix = format!("https://github.com/martinezooo/keyjitsu/releases/download/{tag}/");
    ensure!(
        url.starts_with(&prefix) && url.ends_with("-macos-arm64.zip"),
        "unrecognized GitHub update URL"
    );
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid GitHub asset digest"
    );
    let target_app = existing_app_path()?;
    let stage = tempfile::Builder::new()
        .prefix("keyjitsu-update-")
        .tempdir()
        .context("creating private update staging directory")?;
    let zip = stage.path().join("download.zip");

    let response = ureq::get(url)
        .set(
            "User-Agent",
            concat!("keyjitsu/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(50))
        .call()
        .context("downloading GitHub release")?;
    let mut reader = response.into_reader().take(MAX_ARCHIVE_BYTES + 1);
    let mut file = fs::File::create(&zip)?;
    let received = std::io::copy(&mut reader, &mut file)?;
    ensure!(
        received > 0 && received <= MAX_ARCHIVE_BYTES,
        "update archive exceeds safety limit"
    );
    file.flush()?;
    drop(file);

    let digest_output = run_checked("/usr/bin/shasum", &["-a", "256"], &zip)?;
    let computed = digest_output.split_whitespace().next().unwrap_or("");
    ensure!(
        computed.eq_ignore_ascii_case(digest),
        "download SHA-256 mismatch"
    );
    inspect_archive(&zip)?;
    let output = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(stage.path())
        .output()
        .context("extracting update")?;
    ensure!(output.status.success(), "invalid update ZIP archive");
    let candidate = stage.path().join("Keyjitsu.app");
    check_bundle(&candidate)?;
    let version_check = Command::new(candidate.join("Contents/MacOS/keyjitsu"))
        .arg("--version")
        .output()
        .context("checking signed candidate version")?;
    ensure!(
        version_check.status.success(),
        "candidate version command failed"
    );
    let reported = String::from_utf8_lossy(&version_check.stdout);
    ensure!(
        reported.trim() == format!("keyjitsu {version}"),
        "candidate version does not match the release"
    );
    fs::remove_file(zip)?;
    let staged_app = stage.keep().join("Keyjitsu.app");
    Ok(PreparedUpdate {
        staged_app,
        target_app,
        release_tag: tag.to_string(),
    })
}

pub fn spawn_download(
    download_url: String,
    sha256: String,
    release_tag: String,
) -> Receiver<std::result::Result<PreparedUpdate, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result =
            download_release(&download_url, &sha256, &release_tag).map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
    rx
}

pub fn begin_install(ready: &PreparedUpdate) -> Result<()> {
    ensure!(
        ready.target_app == existing_app_path()?,
        "the current app path changed"
    );
    ensure!(
        ready.release_tag.starts_with("signed-macos-v"),
        "invalid release channel"
    );
    let parent = ready
        .target_app
        .parent()
        .context("installed app has no parent")?;
    tempfile::Builder::new()
        .prefix(".keyjitsu-access-")
        .tempdir_in(parent)
        .context("cannot update this app location; install the DMG manually")?;
    check_bundle(&ready.staged_app)?;
    let executable = ready.staged_app.join("Contents/MacOS/keyjitsu");
    Command::new(&executable)
        .arg("__apply-update")
        .arg(&ready.staged_app)
        .arg(&ready.target_app)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting signed update helper")?;
    Ok(())
}

fn wait_for_original_process(pid: u32) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(35) {
        // kill(pid, 0) is a liveness probe; no signal is delivered.
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        if !alive {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("the old Keyjitsu process did not exit; update was not installed")
}

fn install_prepared_app(source: &Path, destination: &Path, relaunch: bool) -> Result<()> {
    ensure!(
        source.file_name().is_some_and(|n| n == "Keyjitsu.app"),
        "unexpected staged bundle name"
    );
    ensure!(
        destination.file_name().is_some_and(|n| n == "Keyjitsu.app"),
        "unexpected destination bundle name"
    );
    ensure!(
        destination.is_dir() && !fs::symlink_metadata(destination)?.file_type().is_symlink(),
        "the installed app is missing or is a symlink"
    );
    check_bundle(source)?;

    // Copy to the destination filesystem first, so the final replace uses
    // same-volume renames rather than an interrupted cross-volume copy.
    let parent = destination
        .parent()
        .context("installed app has no parent")?;
    let staging = tempfile::Builder::new()
        .prefix(".keyjitsu-install-")
        .tempdir_in(parent)
        .context("cannot write into the app installation folder")?;
    let next_app = staging.path().join("Keyjitsu.app");
    let result = Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(&next_app)
        .output()
        .context("copying signed app to target volume")?;
    ensure!(
        result.status.success(),
        "failed to stage the signed app: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    check_bundle(&next_app)?;

    let backup = staging.path().join("Keyjitsu.previous.app");
    fs::rename(destination, &backup).context("moving installed app to rollback directory")?;
    if let Err(error) = fs::rename(&next_app, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(error).context("activating update; rollback attempted");
    }
    if !relaunch {
        return Ok(());
    }
    let open = Command::new("/usr/bin/open").arg(destination).status();
    match open {
        Ok(status) if status.success() => Ok(()),
        other => {
            let _ = fs::rename(destination, &next_app);
            let restored = fs::rename(&backup, destination);
            if let Err(e) = restored {
                bail!("new app failed to launch ({other:?}); rollback failed: {e}");
            }
            bail!("new app failed to launch ({other:?}); previous version was restored")
        }
    }
}

/// Called only by the already-verified candidate executable, after the GUI
/// exits. The helper never asks for privilege escalation or touches user data.
pub fn run_helper(args: &[String]) -> Result<()> {
    ensure!(args.len() == 3, "invalid update helper arguments");
    let source = PathBuf::from(&args[0]);
    let destination = PathBuf::from(&args[1]);
    let parent_pid: u32 = args[2].parse().context("invalid parent process id")?;
    ensure!(
        parent_pid > 1 && parent_pid != std::process::id(),
        "invalid parent process id"
    );
    wait_for_original_process(parent_pid)?;
    install_prepared_app(&source, &destination, true)
}

pub fn log_helper_error(error: &anyhow::Error, args: &[String]) {
    let log = directories::UserDirs::new()
        .map(|d| d.home_dir().join("Library/Logs/Keyjitsu-updater.log"));
    if let Some(path) = log {
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "update failed: {error:#}");
        }
    }
    // The original app remains in place (or is rolled back) on failure.
    if let Some(path) = args.get(1) {
        if Path::new(path).is_dir() {
            let _ = Command::new("/usr/bin/open").arg(path).spawn();
        }
    }
    // Keep the diagnostic user-visible after the original GUI has closed.
    let script = "on run argv\n display alert \"Keyjitsu update failed\" message (item 1 of argv) as critical\nend run";
    let message = format!("Update could not be completed.\n{error:#}");
    let _ = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg(message)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_signed_release_tags_are_accepted() {
        assert_eq!(
            signed_version("signed-macos-v0.10.0-beta.5-abcdef1").unwrap(),
            "0.10.0-beta.5"
        );
        assert!(signed_version("v0.10.0-beta.5").is_err());
        assert!(signed_version("signed-macos-v0.10.0-beta.5-bad").is_err());
    }

    /// Run after producing a notarized bundle:
    /// KEYJITSU_SIGNED_APP=/absolute/path/to/Keyjitsu.app cargo test
    /// signed_install_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn signed_install_smoke() {
        let candidate =
            PathBuf::from(std::env::var("KEYJITSU_SIGNED_APP").expect("set KEYJITSU_SIGNED_APP"));
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("Keyjitsu.app");
        let old_app =
            PathBuf::from(std::env::var("KEYJITSU_OLD_APP").expect("set KEYJITSU_OLD_APP"));
        let copy = Command::new("/usr/bin/ditto")
            .arg(&old_app)
            .arg(&destination)
            .status()
            .unwrap();
        assert!(copy.success(), "could not copy old app into test directory");
        install_prepared_app(&candidate, &destination, false).unwrap();
        check_bundle(&destination).unwrap();
        let version = Command::new(destination.join("Contents/MacOS/keyjitsu"))
            .arg("--version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn updater_rejects_untrusted_installation_paths() {
        let stage = tempfile::tempdir().unwrap();
        let other_app = stage.path().join("Foreign.app");
        fs::create_dir(&other_app).unwrap();
        assert!(
            install_prepared_app(&other_app, &stage.path().join("Keyjitsu.app"), false).is_err()
        );
    }
}
