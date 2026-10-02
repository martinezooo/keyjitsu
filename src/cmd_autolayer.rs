//! `keyjitsu autolayer` - Keymapp's "smart layers" as a foreground command:
//! when the frontmost macOS app changes, switch the keyboard to the layer
//! mapped for it. No daemon; Ctrl+C restores the base layer and exits.

#![cfg(target_os = "macos")]

use std::process::Command as Proc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::AutolayerRule;
use crate::device::Keyboard;
use crate::protocol::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrontmostApp {
    pub bundle: String,
    pub command: Option<String>,
}

pub(crate) fn rule_matches(bundle: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    !pattern.is_empty() && bundle.contains(pattern)
}

pub(crate) fn rule_matches_app(app: &FrontmostApp, rule: &AutolayerRule) -> bool {
    if !rule_matches(&app.bundle, &rule.bundle) {
        return false;
    }
    let Some(pattern) = rule
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return true;
    };
    app.command
        .as_deref()
        .is_some_and(|command| command.contains(pattern))
}

pub(crate) fn layer_transition(
    current: Option<u8>,
    target: Option<u8>,
) -> (Option<u8>, Option<u8>) {
    if current == target {
        (None, None)
    } else {
        (current, target)
    }
}

/// `--rule com.apple.Terminal=2` (substring match on the bundle id).
pub fn parse_rule(s: &str) -> Result<(String, u8), String> {
    let (bundle, layer) = s
        .split_once('=')
        .ok_or_else(|| format!("expected BUNDLE_ID=LAYER, got {s:?}"))?;
    let bundle = bundle.trim();
    let layer = layer.trim();
    let layer: u8 = layer
        .parse()
        .map_err(|_| format!("{layer:?} is not a layer number"))?;
    if bundle.is_empty() {
        return Err("empty bundle id".into());
    }
    Ok((bundle.to_string(), layer))
}

pub fn run(serial: Option<&str>, rules: &[(String, u8)], poll_ms: u64) -> Result<()> {
    let kb = Keyboard::open(serial)?;
    kb.pair()?;

    println!(
        "autolayer: watching the frontmost app ({} rules). Ctrl+C to stop.",
        rules.len()
    );
    for (bundle, layer) in rules {
        println!("  {bundle} → layer {layer}");
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;

    let mut last_bundle: Option<String> = None;
    let mut active_rule_layer: Option<u8> = None;

    while running.load(Ordering::SeqCst) {
        // Consume pending events so the OS buffer doesn't fill up.
        while let Ok(Some(_)) = kb.read_event(Duration::ZERO) {}

        let observed = frontmost_bundle_id();
        if observed != last_bundle {
            let target = observed.as_deref().and_then(|bundle| {
                rules
                    .iter()
                    .find(|(pat, _)| rule_matches(bundle, pat))
                    .map(|(_, layer)| *layer)
            });
            let (release, enable) = layer_transition(active_rule_layer, target);
            if let Some(prev) = release {
                kb.send(Command::SetLayer {
                    on: false,
                    layer: prev,
                })
                .context("releasing previous autolayer (keyboard unplugged?)")?;
                println!("→ layer {prev} released");
            }
            if let Some(layer) = enable {
                kb.send(Command::SetLayer { on: true, layer })
                    .context("switching layer (keyboard unplugged?)")?;
                println!("→ layer {layer}");
            }
            active_rule_layer = target;
            last_bundle = observed;
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }

    if let Some(prev) = active_rule_layer {
        kb.send(Command::SetLayer {
            on: false,
            layer: prev,
        })
        .context("releasing autolayer on exit (keyboard unplugged?)")?;
        println!("→ layer {prev} released (exit)");
    }
    kb.disconnect();
    Ok(())
}

/// Frontmost app identity via `lsappinfo` (ships with macOS). The process
/// command is best-effort and lets a rule distinguish dedicated instances of
/// the same app without requiring Accessibility or Screen Recording access.
pub(crate) fn frontmost_app() -> Option<FrontmostApp> {
    let out = Proc::new("/bin/sh")
        .args(["-c", "lsappinfo info -only bundleid,pid $(lsappinfo front)"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut bundle: Option<String> = None;
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim();
        if key.contains("CFBundleIdentifier") && !value.is_empty() && !value.starts_with('[') {
            bundle = Some(value.to_string());
        } else if key.contains("pid") {
            pid = value.parse().ok();
        }
    }
    let bundle = bundle?;
    let command = pid.and_then(|pid| {
        Proc::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    Some(FrontmostApp { bundle, command })
}

/// Bundle-only compatibility helper for the CLI and simple UI callers.
pub fn frontmost_bundle_id() -> Option<String> {
    frontmost_app().map(|app| app.bundle)
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

#[cfg(test)]
mod tests {
    use super::{layer_transition, parse_rule, rule_matches, rule_matches_app, FrontmostApp};
    use crate::config::AutolayerRule;

    #[test]
    fn empty_or_whitespace_rules_never_match_every_app() {
        assert!(rule_matches("com.apple.Terminal", "apple.Term"));
        assert!(!rule_matches("com.apple.Terminal", ""));
        assert!(!rule_matches("com.apple.Terminal", "   "));
        assert_eq!(
            parse_rule(" com.apple.Terminal = 2").unwrap(),
            ("com.apple.Terminal".into(), 2)
        );
    }

    #[test]
    fn optional_process_command_qualifies_same_bundle() {
        let app = FrontmostApp {
            bundle: "net.kovidgoyal.kitty".into(),
            command: Some(
                "/Applications/kitty.app/Contents/MacOS/kitty --title homelab-tui (HA Green)"
                    .into(),
            ),
        };
        let dedicated = AutolayerRule {
            bundle: "net.kovidgoyal.kitty".into(),
            command: Some("homelab-tui (HA Green)".into()),
            layer: 3,
        };
        let other = AutolayerRule {
            bundle: "net.kovidgoyal.kitty".into(),
            command: Some("other-terminal".into()),
            layer: 4,
        };
        let generic = AutolayerRule {
            bundle: "net.kovidgoyal.kitty".into(),
            command: None,
            layer: 2,
        };
        assert!(rule_matches_app(&app, &dedicated));
        assert!(!rule_matches_app(&app, &other));
        assert!(rule_matches_app(&app, &generic));
    }

    #[test]
    fn switching_rules_releases_previous_layer_before_enabling_next() {
        assert_eq!(layer_transition(None, Some(3)), (None, Some(3)));
        assert_eq!(layer_transition(Some(3), Some(1)), (Some(3), Some(1)));
        assert_eq!(layer_transition(Some(1), None), (Some(1), None));
        assert_eq!(layer_transition(Some(2), Some(2)), (None, None));
    }
}
