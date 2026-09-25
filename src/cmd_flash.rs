//! `keyjitsu flash` - flash firmware from a file, an Oryx URL, or straight to
//! the newest revision of the layout currently on the keyboard.
//!
//! The USB/bootloader/DFU heavy lifting is ZSA's own open-source `zapp-core`
//! (MIT + Commons Clause), used here as a library.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use nusb::MaybeFuture;
use zapp_core::device;
use zapp_core::firmware::{self, Firmware};
use zapp_core::flash::{self, FlashProgress};

use crate::oryx_api::LayoutId;

const ORYX_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_FIRMWARE_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve a firmware image from `--latest` / URL / file path. Shared with
/// the GUI flash tab.
pub fn acquire_firmware(target: Option<&str>, latest: bool) -> Result<Firmware> {
    match (latest, target) {
        (true, _) => download_latest_for_connected(),
        (false, Some(t)) if t.starts_with("http://") || t.starts_with("https://") => {
            download_from_url(t)
        }
        (false, Some(path)) => {
            firmware::load_firmware(Path::new(path)).context("failed to load firmware file")
        }
        (false, None) => {
            bail!("pass a firmware file or an Oryx URL, or use --latest to update in place")
        }
    }
}

pub fn wait_for_bootloader(
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    on_found: impl Fn(&'static str),
) -> Result<device::BootloaderDevice> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            bail!("canceled");
        }

        for info in nusb::list_devices().wait().context("listing USB devices")? {
            let vid = info.vendor_id();
            let pid = info.product_id();
            let Some(kind) = device::ids::identify_bootloader(vid, pid) else {
                continue;
            };

            std::thread::sleep(Duration::from_millis(500));
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                bail!("canceled");
            }

            let usb = info.open().wait().context("opening bootloader device")?;
            let name = device::ids::friendly_name(vid, pid);
            on_found(name);
            return Ok(device::BootloaderDevice {
                device: usb,
                vid,
                pid,
                kind,
                keyboard: device::ids::keyboard_for_bootloader(vid, pid),
            });
        }

        if Instant::now() >= deadline {
            bail!("no bootloader appeared within {}s - was the reset button pressed?", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn run(target: Option<&str>, latest: bool, timeout_secs: u64) -> Result<()> {
    let fw = acquire_firmware(target, latest)?;
    println!("{}", firmware_summary(&fw));

    println!();
    println!("Put the keyboard into bootloader mode now - press its RESET button");
    println!("(Voyager: the tiny button on the left half, see https://www.zsa.io/flash).");
    println!("Waiting up to {timeout_secs}s for the bootloader…");
    std::io::stdout().flush().ok();

    let dev = wait_for_bootloader(Duration::from_secs(timeout_secs), None, |name| {
        println!("Bootloader detected: {name}");
    })
    .context("bootloader detection failed")?;

    flash::flash_device(&dev, &fw, &|p| match p {
        FlashProgress::Erasing { bytes_erased, total_bytes } => {
            eprint!("\rErasing… {:>3}%", pct(bytes_erased, total_bytes));
        }
        FlashProgress::Writing { bytes_written, total_bytes } => {
            eprint!("\rWriting… {:>3}%", pct(bytes_written, total_bytes));
        }
        FlashProgress::Resetting => eprint!("\rRestarting keyboard…      "),
        FlashProgress::Complete => eprintln!("\r✓ Flash complete.         "),
    })
    .context("flashing failed")?;

    println!("The keyboard should be back in a couple of seconds (try `keyjitsu status`).");
    Ok(())
}

fn pct(done: usize, total: usize) -> usize {
    if total == 0 {
        100
    } else {
        done * 100 / total
    }
}

/// `keyjitsu flash --latest`: read the layout id off the connected keyboard,
/// ask Oryx for its newest revision, download and flash it.
fn download_latest_for_connected() -> Result<Firmware> {
    let kb = device::find_keyboard()
        .context("no ZSA keyboard on USB (plug it in, in normal mode, not bootloader)")?;
    let id = LayoutId::from_serial(&kb.serial)?;
    let newest = fetch_latest_revision(&id.hash)?;
    if newest == id.revision {
        println!(
            "Keyboard already runs the newest revision of layout {} ({newest}) - flashing it again.",
            id.hash
        );
    } else {
        println!("Updating layout {}: {} → {newest}", id.hash, id.revision);
    }
    download_firmware(&newest, false)
}

fn download_from_url(url: &str) -> Result<Firmware> {
    let id = LayoutId::from_url(url)?;
    let revision = if id.revision == "latest" {
        fetch_latest_revision(&id.hash)?
    } else {
        id.revision.clone()
    };
    println!("Layout {} · revision {revision}", id.hash);
    // Moonlander firmware needs the collated (revA+revB) image.
    let collate = url.contains("/moonlander/");
    download_firmware(&revision, collate)
}

fn fetch_latest_revision(layout_id: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Latest {
        latest: String,
    }
    let url = format!("https://oryx.zsa.io/firmware/latest/{layout_id}");
    let latest: Latest = ureq::get(&url)
        .timeout(ORYX_REQUEST_TIMEOUT)
        .call()
        .with_context(|| format!("asking Oryx for the latest revision of {layout_id}"))?
        .into_json()
        .context("malformed response from Oryx")?;
    Ok(latest.latest)
}

fn download_firmware(revision_id: &str, collate: bool) -> Result<Firmware> {
    let mut url = format!("https://oryx.zsa.io/firmware/{revision_id}");
    if collate {
        url.push_str("?collate=true");
    }
    println!("Downloading firmware…");
    let resp = ureq::get(&url)
        .timeout(ORYX_REQUEST_TIMEOUT)
        .call()
        .context("firmware download failed")?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_FIRMWARE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("reading firmware download")?;
    if bytes.len() as u64 > MAX_FIRMWARE_BYTES {
        bail!(
            "firmware download is larger than {MAX_FIRMWARE_BYTES} bytes; refusing a truncated image"
        );
    }
    firmware::load_firmware_from_bytes(&bytes).context("downloaded file is not valid firmware")
}

pub fn firmware_summary(fw: &Firmware) -> String {
    match fw {
        Firmware::DfuBinary { data, vid, pid } => format!(
            "Firmware: DFU binary, {} KiB, target {}",
            data.len() / 1024,
            zapp_core::device::ids::friendly_name(*vid, *pid)
        ),
        Firmware::IgnitionDual { primary, alternate } => format!(
            "Firmware: dual image, {} ({} KiB) + {} ({} KiB). The right one is picked automatically",
            zapp_core::device::ids::friendly_name(primary.vid, primary.pid),
            primary.data.len() / 1024,
            zapp_core::device::ids::friendly_name(alternate.vid, alternate.pid),
            alternate.data.len() / 1024,
        ),
        Firmware::IntelHex { data } => {
            format!("Firmware: Intel HEX (HalfKay), {} KiB", data.len() / 1024)
        }
    }
}
