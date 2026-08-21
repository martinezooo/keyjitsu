//! Disabling the Mac's built-in keyboard.
//!
//! IOKit seizing (`kIOHIDOptionsTypeSeizeDevice`) of the *internal* keyboard is
//! blocked for unprivileged apps on modern macOS - `IOHIDDeviceOpen` returns
//! `kIOReturnNotPrivileged` even with Input Monitoring granted. Instead we remap
//! every key on the internal keyboard to a no-op via `hidutil`, which needs no
//! special permission and only affects the matched device (not the ZSA board).
//!
//! **Safety net.** The mapping does not survive a reboot, but it DOES survive
//! the process dying (crash, `kill`, force-quit) - in which case `Drop` never
//! runs and the built-in keyboard stays disabled. To make that impossible:
//!   1. a marker file is written while the guard is active, removed on clean
//!      release; [`heal_stale_guard`] on startup restores + clears it if a
//!      previous run left it behind, and
//!   2. [`force_restore_if_active`] lets a signal handler restore on SIGTERM/INT.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};

/// Matches the internal keyboard/trackpad. UserKeyMapping only touches keyboard
/// usages, so the trackpad is unaffected.
const MATCH: &str = r#"{"Product":"Apple Internal Keyboard / Trackpad"}"#;
const EMPTY: &str = r#"{"UserKeyMapping":[]}"#;
/// HID keyboard usage page base. Remapping every key's usage to this page's
/// usage 0 makes the OS see no keycode - that's how the built-in is silenced.
const HID_KB_USAGE_PAGE: u64 = 0x7_0000_0000;
/// Absolute path so a Finder-launched `.app` with a sanitized PATH still finds
/// it (a failed spawn here would leave the keyboard disabled).
const HIDUTIL: &str = "/usr/bin/hidutil";

/// True while a guard mapping is applied - read by the signal handler so it can
/// restore before the process dies.
static GUARD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True while the guard *logically* wants the built-in disabled. Unlike
/// GUARD_ACTIVE, this stays set across a lock-screen suspend (see
/// [`suspend_for_lock`]) so we know to re-disable on unlock. Cleared only when
/// the guard is fully turned off (Drop / force restore / heal / failed seize).
static GUARD_WANTED: AtomicBool = AtomicBool::new(false);

/// Marker file: present iff the guard is engaged. If it survives to the next
/// launch, the app didn't release cleanly and we heal on startup.
fn marker_path() -> Option<PathBuf> {
    crate::oryx_api::cache_dir().ok().map(|d| d.join("guard-active.lock"))
}

/// RAII token: while it's alive the built-in keyboard is disabled; dropping it
/// restores it. The private field keeps it constructible only via `seize_builtin`.
pub struct BuiltinKeyboardGuard(());

impl BuiltinKeyboardGuard {
    pub fn describe(&self) -> &'static str {
        "Apple Internal Keyboard / Trackpad"
    }
}

impl Drop for BuiltinKeyboardGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Restore the built-in keyboard, clearing the marker/flag ONLY if hidutil
/// actually succeeded - otherwise the marker stays so the next launch's
/// `heal_stale_guard` retries instead of erasing the safety net.
fn restore() -> bool {
    match hidutil_set(EMPTY) {
        Ok(()) => {
            clear_marker();
            true
        }
        Err(_) => {
            // Leave GUARD_ACTIVE / marker intact so recovery can retry.
            false
        }
    }
}

fn hidutil_set(json: &str) -> Result<()> {
    let ok = Command::new(HIDUTIL)
        .args(["property", "--matching", MATCH, "--set", json])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        bail!("hidutil could not update the built-in keyboard mapping");
    }
    Ok(())
}

/// Write the marker; returns whether it persisted. The marker is the ONLY
/// SIGKILL/crash recovery path, so the caller warns if it couldn't be written.
fn write_marker() -> bool {
    let Some(p) = marker_path() else { return false };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&p, b"1").is_ok()
}

fn clear_marker() {
    GUARD_ACTIVE.store(false, Ordering::SeqCst);
    GUARD_WANTED.store(false, Ordering::SeqCst);
    if let Some(p) = marker_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Screen locked: re-enable the built-in keyboard so the login window is usable
/// even if the Voyager is unavailable, WITHOUT forgetting that the guard should
/// snap back on at unlock (GUARD_WANTED stays set, marker stays). No-op unless a
/// guard is currently engaged. Called from the lock-screen observer.
pub fn suspend_for_lock() {
    if GUARD_WANTED.load(Ordering::SeqCst)
        && GUARD_ACTIVE.load(Ordering::SeqCst)
        && hidutil_set(EMPTY).is_ok()
    {
        GUARD_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Screen unlocked: if the guard still wants to be on, disable the built-in
/// again. No-op unless [`suspend_for_lock`] left a guard pending.
pub fn resume_after_unlock() {
    if GUARD_WANTED.load(Ordering::SeqCst)
        && !GUARD_ACTIVE.load(Ordering::SeqCst)
        && hidutil_set(&disable_mapping()).is_ok()
    {
        GUARD_ACTIVE.store(true, Ordering::SeqCst);
    }
}

/// Remap the whole keyboard usage range (0x04..=0xE7) to usage 0 (no event).
fn disable_mapping() -> String {
    let entries: Vec<String> = (0x04u64..=0xE7)
        .map(|u| {
            format!(
                r#"{{"HIDKeyboardModifierMappingSrc":0x{:X},"HIDKeyboardModifierMappingDst":0x{:X}}}"#,
                HID_KB_USAGE_PAGE + u,
                HID_KB_USAGE_PAGE
            )
        })
        .collect();
    format!(r#"{{"UserKeyMapping":[{}]}}"#, entries.join(","))
}

/// hidutil needs no TCC permission, so this is a no-op kept for call-site
/// compatibility with the old IOKit backend.
pub fn ensure_input_monitoring() -> Result<()> {
    Ok(())
}

pub fn seize_builtin() -> Result<BuiltinKeyboardGuard> {
    // Arm the safety net BEFORE the dangerous call: mark active + write the
    // marker first, so any crash/kill in the disabling window is still
    // recoverable (Drop / signal handler / next-launch heal all see it).
    GUARD_ACTIVE.store(true, Ordering::SeqCst);
    GUARD_WANTED.store(true, Ordering::SeqCst);
    let marked = write_marker();
    match hidutil_set(&disable_mapping()) {
        Ok(()) => {
            if !marked {
                eprintln!(
                    "keyjitsu: warning - couldn't write the guard marker; a hard kill (SIGKILL) \
                     would leave the built-in keyboard disabled until you re-run keyjitsu"
                );
            }
            Ok(BuiltinKeyboardGuard(()))
        }
        Err(e) => {
            // Disabling failed - unwind the armed state so we don't leave a
            // stale marker for a guard that never engaged.
            clear_marker();
            Err(e)
        }
    }
}

/// Restore the built-in keyboard immediately if a guard is active. Safe to call
/// from a signal handler thread (ctrlc): it just runs hidutil + clears state.
/// Returns true if it actually restored (hidutil succeeded).
pub fn force_restore_if_active() -> bool {
    let active = GUARD_ACTIVE.load(Ordering::SeqCst) || marker_path().is_some_and(|p| p.exists());
    if active {
        restore()
    } else {
        false
    }
}

/// On startup: if a previous run left the guard engaged (marker present), the
/// built-in keyboard is still disabled - restore it and clear the marker.
/// Returns true if it healed a stale guard (hidutil succeeded).
pub fn heal_stale_guard() -> bool {
    if marker_path().is_some_and(|p| p.exists()) {
        restore()
    } else {
        false
    }
}

/// The command to restore the keyboard by hand - shown in the UI as a last-
/// resort safety net.
pub fn restore_command() -> String {
    format!("hidutil property --matching '{MATCH}' --set '{EMPTY}'")
}
