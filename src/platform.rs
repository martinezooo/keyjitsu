use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub fn reveal_path(path: &Path) -> Result<()> {
    let target = if path.is_dir() { path } else { path.parent().unwrap_or(path) };

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("explorer.exe");
        c.arg(target);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };

    cmd.spawn()
        .with_context(|| format!("opening {}", target.display()))?;
    Ok(())
}

pub fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };

    cmd.spawn().with_context(|| format!("opening {url}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn autostart_location() -> Option<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Library/LaunchAgents/com.keyjitsu.gui.plist"))
}

#[cfg(not(target_os = "macos"))]
pub fn autostart_location() -> Option<PathBuf> {
    None
}

pub fn autostart_enabled() -> bool {
    autostart_location().is_some_and(|p| p.exists())
}

#[cfg(target_os = "macos")]
pub fn set_autostart(on: bool) -> Result<()> {
    let path = autostart_location().context("cannot determine home directory")?;
    if !on {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    let exe = std::env::current_exe()?;
    let exe = xml_escape(&exe.to_string_lossy());
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
    <key>Label</key><string>com.keyjitsu.gui</string>\n\
    <key>ProgramArguments</key>\n    <array>\n\
        <string>{exe}</string>\n        <string>gui</string>\n    </array>\n\
    <key>RunAtLoad</key><true/>\n\
    <key>LimitLoadToSessionType</key><string>Aqua</string>\n\
</dict>\n</plist>\n"
    );
    crate::config::write_atomic(&path, plist.as_bytes())
}

#[cfg(not(target_os = "macos"))]
pub fn set_autostart(_on: bool) -> Result<()> {
    bail!("start-at-login is not implemented on this platform yet")
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
