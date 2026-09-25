//! HID transport: finding, opening and talking to ZSA keyboards.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use hidapi::{HidApi, HidDevice};

use crate::protocol::{Command, Event, REPORT_SIZE};

pub const ZSA_VID: u16 = 0x3297;
/// QMK raw-HID endpoint identifiers - matching on these (not on interface
/// numbers) selects the vendor channel and avoids the keyboard interface.
pub const RAW_USAGE_PAGE: u16 = 0xFF60;
pub const RAW_USAGE: u16 = 0x61;

pub fn model_name(pid: u16) -> &'static str {
    match pid {
        0x1977 | 0x1978 => "Voyager",
        0x1969 => "Moonlander",
        _ => "ZSA keyboard",
    }
}

/// A ZSA keyboard found during enumeration (its raw-HID interface).
#[derive(Debug, Clone)]
pub struct Found {
    pub pid: u16,
    pub serial: Option<String>,
    pub product: Option<String>,
    path: std::ffi::CString,
}

impl Found {
    pub fn model(&self) -> &'static str {
        model_name(self.pid)
    }
}

pub fn hid_api() -> Result<HidApi> {
    HidApi::new().context("initializing HID API")
}

/// All ZSA raw-HID interfaces currently attached.
pub fn enumerate(api: &HidApi) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for d in api.device_list() {
        if d.vendor_id() != ZSA_VID || d.usage_page() != RAW_USAGE_PAGE || d.usage() != RAW_USAGE {
            continue;
        }
        if found.iter().any(|f| f.path.as_c_str() == d.path()) {
            continue;
        }
        found.push(Found {
            pid: d.product_id(),
            serial: d.serial_number().map(str::to_owned),
            product: d.product_string().map(str::to_owned),
            path: d.path().to_owned(),
        });
    }
    found
}

/// An open, usable keyboard connection.
pub struct Keyboard {
    dev: HidDevice,
    pub info: Found,
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        let _ = self.send(Command::Disconnect);
    }
}

impl Keyboard {
    /// Open the first ZSA keyboard, or the one whose USB serial contains
    /// `serial_filter`.
    pub fn open(serial_filter: Option<&str>) -> Result<Keyboard> {
        let api = hid_api()?;
        let all = enumerate(&api);
        if all.is_empty() {
            bail!(
                "no ZSA keyboard found. Is it plugged in? \
                 (If Keymapp is running, quit it - the raw HID channel is exclusive.)"
            );
        }
        let info = match serial_filter {
            None => all
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("keyboard disappeared during enumeration"))?,
            Some(f) => all
                .into_iter()
                .find(|k| k.serial.as_deref().is_some_and(|s| s.contains(f)))
                .ok_or_else(|| anyhow!("no ZSA keyboard with serial containing {f:?}"))?,
        };
        let dev = api
            .open_path(info.path.as_c_str())
            .with_context(|| format!("opening {} (is another app using it?)", info.model()))?;
        Ok(Keyboard { dev, info })
    }

    /// Write one 32-byte command packet (prefixed with report id 0, as QMK
    /// raw HID has no report ids).
    pub fn send(&self, cmd: Command) -> Result<()> {
        let mut buf = [0u8; REPORT_SIZE + 1];
        buf[1..].copy_from_slice(&cmd.encode());
        let n = self.dev.write(&buf).context("HID write failed")?;
        if n != buf.len() {
            bail!("short HID write ({n}/{} bytes)", buf.len());
        }
        Ok(())
    }

    /// Read a single event, waiting up to `timeout`. `Ok(None)` on timeout.
    pub fn read_event(&self, timeout: Duration) -> Result<Option<Event>> {
        let mut buf = [0u8; REPORT_SIZE];
        let n = self
            .dev
            .read_timeout(&mut buf, timeout.as_millis() as i32)
            .context("HID read failed")?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Event::decode(&buf[..n]))
    }

    /// Send `cmd`, then read events until one matches `pred` (returning it)
    /// or `timeout` elapses. Non-matching events are passed to `on_other`.
    pub fn request(
        &self,
        cmd: Command,
        timeout: Duration,
        pred: impl Fn(&Event) -> bool,
        mut on_other: impl FnMut(Event),
    ) -> Result<Event> {
        self.send(cmd)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| anyhow!("keyboard did not answer {cmd:?} within {timeout:?}"))?;
            if let Some(ev) = self.read_event(left.min(Duration::from_millis(100)))? {
                if pred(&ev) {
                    return Ok(ev);
                }
                on_other(ev);
            }
        }
    }

    /// Probe the Oryx protocol version (an all-0xFE packet).
    pub fn protocol_version(&self) -> Result<u8> {
        match self.request(
            Command::GetProtocolVersion,
            Duration::from_secs(2),
            |e| matches!(e, Event::ProtocolVersion(_)),
            |_| {},
        )? {
            Event::ProtocolVersion(v) => Ok(v),
            other => bail!("unexpected protocol-version response: {other:?}"),
        }
    }

    pub fn pair_with_events(
        &self,
        mut on_other: impl FnMut(Event),
    ) -> Result<Option<u8>> {
        self.request(
            Command::PairingInit,
            Duration::from_secs(2),
            |e| matches!(e, Event::PairingSuccess | Event::PairingFailed),
            &mut on_other,
        )
        .and_then(|ev| match ev {
            Event::PairingSuccess => Ok(()),
            _ => bail!("keyboard refused pairing"),
        })?;

        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Some(ev) = self.read_event(Duration::from_millis(50))? {
                if let Event::Layer(n) = ev {
                    on_other(Event::Layer(n));
                    return Ok(Some(n));
                }
                on_other(ev);
            }
        }
        Ok(None)
    }

    /// Pair with the keyboard (required before it streams events).
    pub fn pair(&self) -> Result<Option<u8>> {
        self.pair_with_events(|_| {})
    }

    pub fn fw_version_with_events(
        &self,
        on_other: impl FnMut(Event),
    ) -> Result<String> {
        match self.request(
            Command::GetFwVersion,
            Duration::from_secs(2),
            |e| matches!(e, Event::FwVersion(_)),
            on_other,
        )? {
            Event::FwVersion(s) => Ok(s),
            other => bail!("unexpected firmware-version response: {other:?}"),
        }
    }

    /// Firmware "version" string (`SERIAL_NUMBER`), which for Oryx-built
    /// firmware embeds the layout id.
    pub fn fw_version(&self) -> Result<String> {
        self.fw_version_with_events(|_| {})
    }

    /// Politely tell the firmware to stop streaming (clears paired state).
    pub fn disconnect(&self) {
        let _ = self.send(Command::Disconnect);
    }
}
