//! Fully-local firmware builds: fetch Oryx's *generated* QMK source (anonymous,
//! no login), apply the user's key remaps to `keymap.c`, then compile with a
//! local `qmk` + `qmk_firmware` checkout. The resulting `.bin` is flashed by
//! the normal flash path.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use eframe::egui;

use crate::keymap;
use crate::oryx_api::cache_dir;

/// Refuse to cache an Oryx source download bigger than this (it's a small
/// keymap zip; anything larger is a truncated/garbage response).
const MAX_SOURCE_ZIP_BYTES: u64 = 8 * 1024 * 1024;
const SOURCE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often to poll the `qmk compile` child while streaming its output.
const COMPILE_POLL: Duration = Duration::from_millis(120);

/// A remap coming from the GUI: layer + `LAYOUT` position + new keycode.
pub struct KeyEdit {
    pub layer: u8,
    pub position: usize,
    pub keycode: String,
}

/// A brand-new layer to append: its index + (LAYOUT position → keycode) list.
pub struct NewLayer {
    pub position: u8,
    pub keys: Vec<(usize, String)>,
}

/// What the local toolchain looks like right now.
pub struct BuildEnv {
    pub qmk_cli: bool,
    pub arm_gcc: bool,
    pub firmware_dir: Option<PathBuf>,
}

impl BuildEnv {
    pub fn is_ready(&self) -> bool {
        self.qmk_cli && self.arm_gcc && self.firmware_dir.is_some()
    }
}

fn which(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Where `qmk setup` clones the firmware by default, plus a couple of common
/// spots. Honors a saved override in the config.
pub fn detect_env() -> BuildEnv {
    let cfg = crate::config::load();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = cfg.qmk_firmware_dir {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        candidates.push(home.join("qmk_firmware"));
        candidates.push(home.join("Documents/qmk_firmware"));
        candidates.push(home.join("src/qmk_firmware"));
    }
    let firmware_dir = candidates.into_iter().find(|p| is_qmk_tree(p));

    BuildEnv {
        qmk_cli: which("qmk"),
        arm_gcc: which("arm-none-eabi-gcc"),
        firmware_dir,
    }
}

fn is_qmk_tree(p: &Path) -> bool {
    // A real checkout has the Voyager keyboard directory.
    p.join("keyboards/zsa/voyager").is_dir()
}

// ---------------------------------------------------------------------------
// Background build job

pub enum BuildMsg {
    Log(String),
    Built(PathBuf),
    Failed(String),
}

pub fn spawn_build(
    revision: String,
    edits: Vec<KeyEdit>,
    dances: Vec<crate::keymap::DanceSpec>,
    new_layers: Vec<NewLayer>,
    firmware_serial: Option<String>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) -> Receiver<BuildMsg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let log = |s: String| {
            let _ = tx.send(BuildMsg::Log(s));
            ctx.request_repaint();
        };
        match build(&revision, &edits, &dances, &new_layers, firmware_serial.as_deref(), &cancel, &log) {
            Ok(bin) => {
                let _ = tx.send(BuildMsg::Built(bin));
            }
            Err(e) => {
                let _ = tx.send(BuildMsg::Failed(format!("{e:#}")));
            }
        }
        ctx.request_repaint();
    });
    rx
}

pub fn build(
    revision: &str,
    edits: &[KeyEdit],
    dances: &[crate::keymap::DanceSpec],
    new_layers: &[NewLayer],
    firmware_serial: Option<&str>,
    cancel: &Arc<AtomicBool>,
    log: &dyn Fn(String),
) -> Result<PathBuf> {
    let env = detect_env();
    let firmware = env
        .firmware_dir
        .ok_or_else(|| anyhow!("no qmk_firmware checkout found - run `qmk setup` (see Tools)"))?;
    if !env.qmk_cli {
        bail!("the `qmk` CLI isn't installed - see Tools for setup");
    }
    if !env.arm_gcc {
        bail!("the ARM GCC toolchain isn't installed - see Tools for setup");
    }

    log(format!("Fetching generated source for revision {revision}…"));
    let mut files = fetch_source_files(revision)?;

    let keymap_c = take_file(&mut files, "keymap.c")
        .ok_or_else(|| anyhow!("no keymap.c in the generated source"))?;
    let rules_mk = take_file(&mut files, "rules.mk").unwrap_or_default();

    // A multi-step slot ("KC_A\nKC_B", staged from more than one "then press
    // another key") going into a PLAIN LAYOUT position - not a tap-dance,
    // which taps its own multi-step slots directly in the generated case
    // body - needs a custom keycode plus generated process_record_user to
    // fire the taps: collect those here, substituting the position's
    // keycode with the generated MACRO_KJ_{id} name.
    let mut macros: Vec<keymap::MacroSpec> = Vec::new();
    let mut macro_for = |code: &str| -> String {
        if !code.contains('\n') {
            return code.to_string();
        }
        let id = macros.len();
        let steps: Vec<String> = code.split('\n').map(str::to_string).collect();
        macros.push(keymap::MacroSpec { id, steps });
        format!("MACRO_KJ_{id}")
    };

    log(format!("Applying {} key change(s) to keymap.c…", edits.len()));
    let km_edits: Vec<keymap::Edit> = edits
        .iter()
        .map(|e| keymap::Edit {
            layer: e.layer,
            position: e.position,
            keycode: macro_for(&e.keycode),
        })
        .collect();
    let mut patched = keymap::apply_edits(&keymap_c, &km_edits)?;
    for nl in new_layers {
        log(format!("Adding layer [{}] ({} keys)…", nl.position, nl.keys.len()));
        let keys: Vec<(usize, String)> = nl.keys.iter().map(|(pos, code)| (*pos, macro_for(code))).collect();
        patched = keymap::add_layer(&patched, nl.position, &keys)?;
    }
    if !dances.is_empty() {
        log(format!("Generating {} tap dance(s)…", dances.len()));
        patched = keymap::apply_dances(&patched, dances)?;
    }
    if let Some(serial) = firmware_serial {
        // Oryx returns SERIAL_NUMBER inside a fixed 32-byte raw-HID report:
        // byte 0 is the event id and one byte is the stop marker, leaving at
        // most 30 visible serial bytes.
        if serial.len() > 30 || serial.contains(['"','\n','\r']) {
            bail!("invalid firmware serial (must be <= 30 ASCII-safe bytes)");
        }
        let cfg = files.iter_mut().find(|(n, _)| n == "config.h")
            .ok_or_else(|| anyhow!("generated source has no config.h for firmware identity"))?;
        let mut text = String::from_utf8_lossy(&cfg.1).into_owned();
        text.push_str("\n// Keyjitsu: identify the exact authored state running on the keyboard.\n");
        text.push_str("#undef SERIAL_NUMBER\n");
        text.push_str(&format!("#define SERIAL_NUMBER \"{}\"\n", serial));
        cfg.1 = text.into_bytes();
        log(format!("Embedded firmware state identity: {serial}"));
    }
    if !macros.is_empty() {
        log(format!("Generating {} multi-key macro(s)…", macros.len()));
        patched = keymap::apply_macros(&patched, &macros)?;
    }

    // Enable any QMK features the new keycodes rely on (Oryx often ships these
    // off to save space), otherwise they'd compile but silently do nothing.
    // Scan EVERY source of new keycodes - edits, tap-dance sub-actions, and
    // new-layer keys - not just `edits` (a mouse/media key on a new layer or in
    // a dance was previously built inert).
    let all_codes: Vec<&str> = edits
        .iter()
        .map(|e| e.keycode.as_str())
        .chain(dances.iter().flat_map(|d| {
            [&d.tap, &d.hold, &d.double_tap, &d.tap_hold].into_iter().filter_map(|s| s.as_deref())
        }))
        .chain(new_layers.iter().flat_map(|l| l.keys.iter().map(|(_, c)| c.as_str())))
        .collect();
    let mut rules = rules_mk;
    if all_codes.iter().any(|c| crate::keycodes::needs_mousekey(c)) {
        rules = set_rule(&rules, "MOUSEKEY_ENABLE", "yes");
        log("Enabled MOUSEKEY_ENABLE (mouse keycode used)".into());
    }
    if all_codes.iter().any(|c| crate::keycodes::needs_extrakey(c)) {
        rules = set_rule(&rules, "EXTRAKEY_ENABLE", "yes");
        log("Enabled EXTRAKEY_ENABLE (media/system keycode used)".into());
    }
    if !dances.is_empty() {
        rules = set_rule(&rules, "TAP_DANCE_ENABLE", "yes");
        log("Enabled TAP_DANCE_ENABLE (tap dance generated)".into());
        // Our generated dances set a comfortable per-key tapping term via
        // get_tapping_term - which only fires when TAPPING_TERM_PER_KEY is
        // defined. Oryx sources with dances already define it; add it if not.
        if let Some(cfg) = files.iter_mut().find(|(n, _)| n == "config.h") {
            let text = String::from_utf8_lossy(&cfg.1);
            if !text.contains("TAPPING_TERM_PER_KEY") {
                let mut new = text.into_owned();
                new.push_str("\n#define TAPPING_TERM_PER_KEY\n");
                cfg.1 = new.into_bytes();
                log("Enabled TAPPING_TERM_PER_KEY (comfortable dance timing)".into());
            }
        }
    }

    // Drop the layout into a dedicated keymap so we never touch `default`.
    // Write every generated file (keymap.c pulls in i18n.h, config.h, …), then
    // overwrite keymap.c / rules.mk with our edited versions.
    let km_dir = firmware.join("keyboards/zsa/voyager/keymaps/keyjitsu");
    std::fs::create_dir_all(&km_dir)
        .with_context(|| format!("creating {}", km_dir.display()))?;
    for (name, bytes) in &files {
        std::fs::write(km_dir.join(name), bytes)
            .with_context(|| format!("writing {}", km_dir.join(name).display()))?;
    }
    std::fs::write(km_dir.join("keymap.c"), patched)
        .with_context(|| format!("writing {}", km_dir.join("keymap.c").display()))?;
    std::fs::write(km_dir.join("rules.mk"), rules)
        .with_context(|| format!("writing {}", km_dir.join("rules.mk").display()))?;
    log(format!("Wrote {} source file(s) to {}", files.len() + 2, km_dir.display()));

    if cancel.load(Ordering::SeqCst) {
        bail!("canceled");
    }
    log("Compiling with qmk (this can take a minute)…".into());
    run_streamed(
        Command::new("qmk")
            .current_dir(&firmware)
            .args(["compile", "-kb", "zsa/voyager", "-km", "keyjitsu"]),
        cancel,
        log,
    )?;

    // qmk writes `zsa_voyager_keyjitsu.bin` into the firmware root.
    let bin = firmware.join("zsa_voyager_keyjitsu.bin");
    if !bin.is_file() {
        bail!("compile finished but {} was not produced", bin.display());
    }
    log(format!("Built {}", bin.display()));
    Ok(bin)
}

/// Pull a source file out of the list by basename, as UTF-8.
fn take_file(files: &mut Vec<(String, Vec<u8>)>, name: &str) -> Option<String> {
    let idx = files.iter().position(|(n, _)| n == name)?;
    let (_, bytes) = files.remove(idx);
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Set `KEY = value` in a `rules.mk`, replacing the line if present (any value)
/// or appending it. Matches `KEY = ...` allowing arbitrary spacing.
fn set_rule(rules: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut out: Vec<String> = rules
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
            {
                found = true;
                format!("{key} = {value}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        out.push(format!("{key} = {value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}



/// All files inside the generated `*_source/` directory of Oryx's zip, as
/// `(basename, bytes)`. Non-source extras (the prebuilt .bin, build.log,
/// README) are skipped.
fn fetch_source_files(revision: &str) -> Result<Vec<(String, Vec<u8>)>> {
    // Keep the revision from escaping the cache dir / URL path.
    if revision.is_empty() || revision.contains(['/', '\\', '.']) {
        bail!("invalid revision id {revision:?}");
    }
    // Cache the zip so rebuilds are offline - but only once it's validated as a
    // real zip, so a truncated download / captive-portal HTML page can't poison
    // the cache and make every future build of this revision fail.
    let cache = cache_dir()?.join(format!("source-{revision}.zip"));
    let bytes = if let Ok(b) = std::fs::read(&cache) {
        b
    } else {
        let url = format!("https://oryx.zsa.io/source/{revision}");
        let mut b = Vec::new();
        ureq::get(&url)
            .timeout(SOURCE_REQUEST_TIMEOUT)
            .call()
            .with_context(|| format!("downloading generated source from {url}"))?
            .into_reader()
            // +1 byte: if we read exactly the cap the body was truncated.
            .take(MAX_SOURCE_ZIP_BYTES + 1)
            .read_to_end(&mut b)?;
        if b.len() as u64 > MAX_SOURCE_ZIP_BYTES {
            bail!("generated source from {url} is larger than {MAX_SOURCE_ZIP_BYTES} bytes - refusing to cache a truncated download");
        }
        // Validate BEFORE caching.
        zip::ZipArchive::new(std::io::Cursor::new(&b[..]))
            .with_context(|| format!("downloaded source from {url} is not a valid zip (not caching)"))?;
        if let Err(e) = crate::config::write_atomic(&cache, &b) {
            eprintln!("keyjitsu: could not cache generated source {revision}: {e:#}");
        }
        b
    };

    let mut zip = match zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
        Ok(z) => z,
        Err(e) => {
            // A cached file that won't open is stale/corrupt - drop it so the
            // next build re-downloads instead of failing forever.
            if let Err(remove) = std::fs::remove_file(&cache) {
                if remove.kind() != std::io::ErrorKind::NotFound {
                    return Err(anyhow!(
                        "cached source is invalid ({e}) and could not be removed: {remove}"
                    ));
                }
            }
            return Err(anyhow!("cached source is not a valid zip ({e}). Removed it, retry the build"));
        }
    };
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).context("reading source zip")?;
        if !f.is_file() {
            continue;
        }
        let name = f.name().to_string();
        // Only the compilable source, not build.log / README / the prebuilt bin.
        let is_source = name.contains("_source/")
            && (name.ends_with(".c") || name.ends_with(".h") || name.ends_with(".mk")
                || name.ends_with(".json"));
        if !is_source {
            continue;
        }
        let base = name.rsplit('/').next().unwrap_or(&name).to_string();
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        out.push((base, bytes));
    }
    if out.is_empty() {
        bail!("no source files found in the generated zip");
    }
    Ok(out)
}

/// Run a command, streaming combined stdout+stderr to `log`, killable via
/// `cancel`. Output is read on helper threads so the main loop can poll both
/// process exit and the cancel flag.
fn run_streamed(cmd: &mut Command, cancel: &Arc<AtomicBool>, log: &dyn Fn(String)) -> Result<()> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start qmk (is it on PATH?)")?;

    let (line_tx, line_rx) = channel::<String>();
    let readers: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
        child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>),
    ];
    for reader in readers.into_iter().flatten() {
        let tx = line_tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(line_tx);

    loop {
        while let Ok(line) = line_rx.try_recv() {
            log(line);
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("canceled");
        }
        match child.try_wait().context("waiting for qmk")? {
            Some(status) => {
                // Drain any remaining buffered lines.
                while let Ok(line) = line_rx.try_recv() {
                    log(line);
                }
                if !status.success() {
                    bail!("qmk compile failed (exit {:?})", status.code());
                }
                return Ok(());
            }
            None => std::thread::sleep(COMPILE_POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::set_rule;

    #[test]
    fn replaces_existing_rule_regardless_of_value() {
        let r = "MOUSEKEY_ENABLE = no\nORYX_ENABLE = yes\n";
        let out = set_rule(r, "MOUSEKEY_ENABLE", "yes");
        assert!(out.contains("MOUSEKEY_ENABLE = yes"));
        assert!(!out.contains("MOUSEKEY_ENABLE = no"));
        assert!(out.contains("ORYX_ENABLE = yes"));
    }

    #[test]
    fn appends_missing_rule() {
        let out = set_rule("ORYX_ENABLE = yes\n", "EXTRAKEY_ENABLE", "yes");
        assert!(out.contains("ORYX_ENABLE = yes"));
        assert!(out.contains("EXTRAKEY_ENABLE = yes"));
    }
}
