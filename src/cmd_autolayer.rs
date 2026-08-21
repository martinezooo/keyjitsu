//! `keyjitsu autolayer` - Keymapp's "smart layers" as a foreground command:
//! when the frontmost macOS app changes, switch the keyboard to the layer
//! mapped for it. No daemon; Ctrl+C restores the base layer and exits.

#![cfg(target_os = "macos")]

use std::process::Command as Proc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::device::Keyboard;
use crate::protocol::Command;

/// `--rule com.apple.Terminal=2` (substring match on the bundle id).
pub fn parse_rule(s: &str) -> Result<(String, u8), String> {
    let (bundle, layer) = s
        .split_once('=')
        .ok_or_else(|| format!("expected BUNDLE_ID=LAYER, got {s:?}"))?;
    let layer: u8 = layer.parse().map_err(|_| format!("{layer:?} is not a layer number"))?;
    if bundle.is_empty() {
        return Err("empty bundle id".into());
    }
    Ok((bundle.to_string(), layer))
}

pub fn run(serial: Option<&str>, rules: &[(String, u8)], poll_ms: u64) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    kb.pair()?;

    println!("autolayer: watching the frontmost app ({} rules). Ctrl+C to stop.", rules.len());
    for (bundle, layer) in rules {
        println!("  {bundle} → layer {layer}");
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;

    let mut last_bundle = String::new();
    let mut active_rule_layer: Option<u8> = None;

    while running.load(Ordering::SeqCst) {
        // Consume pending events so the OS buffer doesn't fill up.
        while let Ok(Some(_)) = kb.read_event(Duration::ZERO) {}

        if let Some(bundle) = frontmost_bundle_id() {
            if bundle != last_bundle {
                let target = rules
                    .iter()
                    .find(|(pat, _)| bundle.contains(pat.as_str()))
                    .map(|(_, layer)| *layer);
                match (target, active_rule_layer) {
                    (Some(layer), current) if current != Some(layer) => {
                        kb.send(Command::SetLayer { on: true, layer })
                            .context("switching layer (keyboard unplugged?)")?;
                        println!("→ layer {layer}  ({bundle})");
                        active_rule_layer = Some(layer);
                    }
                    (None, Some(prev)) => {
                        kb.send(Command::SetLayer { on: false, layer: prev })
                            .context("releasing layer (keyboard unplugged?)")?;
                        println!("→ layer {prev} released  ({bundle})");
                        active_rule_layer = None;
                    }
                    _ => {}
                }
                last_bundle = bundle;
            }
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }

    if let Some(prev) = active_rule_layer {
        let _ = kb.send(Command::SetLayer { on: false, layer: prev });
        println!("→ layer {prev} released (exit)");
    }
    kb.disconnect();
    Ok(())
}

/// Bundle id of the frontmost app, via `lsappinfo` (ships with macOS).
pub fn frontmost_bundle_id() -> Option<String> {
    let out = Proc::new("/bin/sh")
        .args(["-c", "lsappinfo info -only bundleid $(lsappinfo front)"])
        .output()
        .ok()?;
    // Output looks like: "CFBundleIdentifier"="com.apple.Terminal"
    // Apps without a bundle id (e.g. a bare binary) yield `=[ NULL ]`.
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text.split('=').nth(1)?;
    let bundle = value.trim().trim_matches('"').trim();
    if bundle.is_empty() || bundle.starts_with('[') {
        None
    } else {
        Some(bundle.to_string())
    }
}

/// Currently-running apps as `(name, bundle_id)`, for the rule picker.
/// Parses `lsappinfo list` blocks: `N) "Name" ASN…` then `bundleID="…"`.
pub fn running_apps() -> Vec<(String, String)> {
    let Ok(out) = Proc::new("/bin/sh").args(["-c", "lsappinfo list"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut apps: Vec<(String, String)> = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.split_once(") \"").map(|(_, r)| r) {
            // Header line: N) "Name" ASN:…
            pending_name = rest.split('"').next().map(str::to_string);
        } else if let Some(idx) = t.find("bundleID=\"") {
            let b = &t[idx + 10..];
            if let Some(bundle) = b.split('"').next() {
                if !bundle.is_empty() {
                    let name = pending_name.take().unwrap_or_else(|| bundle.to_string());
                    apps.push((name, bundle.to_string()));
                }
            }
        }
    }
    apps.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    apps.dedup_by(|a, b| a.1 == b.1);
    apps
}
