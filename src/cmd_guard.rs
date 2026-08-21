//! `keyjitsu guard` - Karabiner-style: the Mac's built-in keyboard is
//! disabled while a ZSA keyboard is connected, and restored the moment it is
//! unplugged or the command exits. No daemon: it runs in the foreground.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::device;
use crate::macos_kb::{self, BuiltinKeyboardGuard};

pub fn run(serial: Option<&str>) -> Result<()> {
    macos_kb::ensure_input_monitoring()?;

    // If a previous run died (SIGKILL/crash) with the guard on, the built-in
    // keyboard is still disabled - restore it before we do anything else.
    if macos_kb::heal_stale_guard() {
        println!("guard: restored the built-in keyboard from a previous session.");
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    // Ctrl+C / SIGTERM: restore the keyboard directly in the handler too, in
    // case the graceful-drop path below doesn't get to run.
    ctrlc::set_handler(move || {
        macos_kb::force_restore_if_active();
        r.store(false, Ordering::SeqCst);
    })?;

    println!("guard: built-in keyboard is disabled while a ZSA keyboard is connected.");
    println!("guard: Ctrl+C restores it and exits.");

    let mut api = device::hid_api()?;
    let mut guard: Option<BuiltinKeyboardGuard> = None;
    let mut announced_waiting = false;

    while running.load(Ordering::SeqCst) {
        api.refresh_devices().context("refreshing USB device list")?;
        let present = device::enumerate(&api).iter().any(|k| match serial {
            Some(f) => k.serial.as_deref().is_some_and(|s| s.contains(f)),
            None => true,
        });

        match (present, guard.is_some()) {
            (true, false) => match macos_kb::seize_builtin() {
                Ok(g) => {
                    println!("🔒 disabled: {}", g.describe());
                    guard = Some(g);
                    announced_waiting = false;
                }
                Err(e) => {
                    // Permission / exclusivity problems won't fix themselves;
                    // stop instead of spamming retries.
                    return Err(e);
                }
            },
            (false, true) => {
                guard = None; // drop → devices closed
                println!("🔓 restored: ZSA keyboard disconnected");
            }
            (false, false) if !announced_waiting => {
                println!("waiting for a ZSA keyboard…");
                announced_waiting = true;
            }
            _ => {}
        }

        std::thread::sleep(Duration::from_millis(700));
    }

    if guard.take().is_some() {
        println!("🔓 restored: exiting");
    }
    Ok(())
}
