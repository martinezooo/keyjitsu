//! Codec for the ZSA "Oryx" raw-HID protocol (v4).
//!
//! Wire format (both directions): 32-byte reports over QMK raw HID
//! (usage page 0xFF60, usage 0x61). `bytes[0]` is the command/event id,
//! followed by parameters, padded to the end with `0xFE` (stop byte).
//!
//! Source of truth: `oryx.c` / `oryx.h` in <https://github.com/zsa/qmk_modules>.

/// Report size of the QMK raw HID endpoint.
pub const REPORT_SIZE: usize = 32;
/// Stop/padding byte.
pub const STOP: u8 = 0xFE;
/// Protocol version this crate implements.
pub const PROTOCOL_VERSION: u8 = 4;

/// Host → keyboard command ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandId {
    GetFwVersion = 0x00,
    PairingInit = 0x01,
    #[allow(dead_code)] // legacy no-op, kept for protocol completeness
    PairingValidate = 0x02,
    Disconnect = 0x03,
    SetLayer = 0x04,
    RgbControl = 0x05,
    SetRgbLed = 0x06,
    SetStatusLed = 0x07,
    UpdateBrightness = 0x08,
    SetRgbLedAll = 0x09,
    StatusLedControl = 0x0A,
    // Encoded as a fully 0xFE-padded packet (see `Command::encode`), so the
    // variant itself is never constructed; kept to document the wire id.
    #[allow(dead_code)]
    GetProtocolVersion = 0xFE,
}

/// A 32-byte packet ready to be written to the device.
pub type Packet = [u8; REPORT_SIZE];

fn packet(id: CommandId, params: &[u8]) -> Packet {
    debug_assert!(params.len() < REPORT_SIZE);
    let mut buf = [STOP; REPORT_SIZE];
    buf[0] = id as u8;
    buf[1..=params.len()].copy_from_slice(params);
    buf
}

/// Commands understood by the keyboard. Encoded per the `oryx.c` handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    GetFwVersion,
    PairingInit,
    Disconnect,
    /// Activate (`on = true`, firmware calls `layer_move`) or release
    /// (`on = false`, `layer_off`) a layer.
    SetLayer { on: bool, layer: u8 },
    /// Take over (`true`) or release (`false`) RGB control. Acked by the
    /// firmware with an `Event::RgbControl`.
    RgbControl(bool),
    SetRgbLed { led: u8, r: u8, g: u8, b: u8 },
    SetRgbLedAll { r: u8, g: u8, b: u8 },
    /// Status LEDs are indexed 0..=5.
    SetStatusLed { led: u8, on: bool },
    /// Take over (`true`) or release (`false`) status-LED control.
    StatusLedControl(bool),
    /// `up = true` increases brightness, `false` decreases.
    UpdateBrightness { up: bool },
    GetProtocolVersion,
}

impl Command {
    pub fn encode(self) -> Packet {
        use CommandId as C;
        match self {
            Command::GetFwVersion => packet(C::GetFwVersion, &[]),
            Command::PairingInit => packet(C::PairingInit, &[]),
            Command::Disconnect => packet(C::Disconnect, &[]),
            Command::SetLayer { on, layer } => packet(C::SetLayer, &[on as u8, layer]),
            Command::RgbControl(on) => packet(C::RgbControl, &[on as u8]),
            Command::SetRgbLed { led, r, g, b } => packet(C::SetRgbLed, &[led, r, g, b]),
            Command::SetRgbLedAll { r, g, b } => packet(C::SetRgbLedAll, &[r, g, b]),
            Command::SetStatusLed { led, on } => packet(C::SetStatusLed, &[led, on as u8]),
            Command::StatusLedControl(on) => packet(C::StatusLedControl, &[on as u8]),
            Command::UpdateBrightness { up } => packet(C::UpdateBrightness, &[up as u8]),
            // A fully 0xFE-padded packet doubles as the version probe.
            Command::GetProtocolVersion => [STOP; REPORT_SIZE],
        }
    }
}

/// Keyboard → host events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Response to `GetFwVersion`: the firmware's `SERIAL_NUMBER` string,
    /// which for Oryx-built firmware embeds the layout id (e.g. `hash/rev`).
    FwVersion(String),
    PairingInput(Vec<u8>),
    PairingKeyInput(Vec<u8>),
    PairingFailed,
    PairingSuccess,
    /// Active layer changed (also emitted right after pairing).
    Layer(u8),
    KeyDown { col: u8, row: u8 },
    KeyUp { col: u8, row: u8 },
    /// Ack for `RgbControl`; payload is the current takeover state.
    RgbControl(bool),
    ToggleSmartLayer(u8),
    TriggerSmartLayer(u8),
    StatusLedControl(bool),
    /// Response to `GetProtocolVersion`.
    ProtocolVersion(u8),
    Error(Vec<u8>),
    Unknown { id: u8, params: Vec<u8> },
}

/// Event ids (see `Oryx_Event_Code` in `oryx.h`).
mod event_id {
    pub const FW_VERSION: u8 = 0x00;
    pub const PAIRING_INPUT: u8 = 0x01;
    pub const PAIRING_KEY_INPUT: u8 = 0x02;
    pub const PAIRING_FAILED: u8 = 0x03;
    pub const PAIRING_SUCCESS: u8 = 0x04;
    pub const LAYER: u8 = 0x05;
    pub const KEYDOWN: u8 = 0x06;
    pub const KEYUP: u8 = 0x07;
    pub const RGB_CONTROL: u8 = 0x08;
    pub const TOGGLE_SMART_LAYER: u8 = 0x09;
    pub const TRIGGER_SMART_LAYER: u8 = 0x0A;
    pub const STATUS_LED_CONTROL: u8 = 0x0B;
    pub const PROTOCOL_VERSION: u8 = 0xFE;
    pub const ERROR: u8 = 0xFF;
}

impl Event {
    /// Decode a single report. Returns `None` for empty/truncated reads.
    pub fn decode(report: &[u8]) -> Option<Event> {
        let (&id, rest) = report.split_first()?;
        let params: &[u8] = rest
            .split(|&b| b == STOP)
            .next()
            .unwrap_or(&[]);
        use event_id as E;
        Some(match id {
            E::FW_VERSION => Event::FwVersion(String::from_utf8_lossy(params).into_owned()),
            E::PAIRING_INPUT => Event::PairingInput(params.to_vec()),
            E::PAIRING_KEY_INPUT => Event::PairingKeyInput(params.to_vec()),
            E::PAIRING_FAILED => Event::PairingFailed,
            E::PAIRING_SUCCESS => Event::PairingSuccess,
            E::LAYER => Event::Layer(*params.first()?),
            E::KEYDOWN => Event::KeyDown { col: *params.first()?, row: *params.get(1)? },
            E::KEYUP => Event::KeyUp { col: *params.first()?, row: *params.get(1)? },
            E::RGB_CONTROL => Event::RgbControl(params.first().copied().unwrap_or(0) != 0),
            E::TOGGLE_SMART_LAYER => Event::ToggleSmartLayer(params.first().copied().unwrap_or(0)),
            E::TRIGGER_SMART_LAYER => Event::TriggerSmartLayer(params.first().copied().unwrap_or(0)),
            E::STATUS_LED_CONTROL => {
                Event::StatusLedControl(params.first().copied().unwrap_or(0) != 0)
            }
            E::PROTOCOL_VERSION => Event::ProtocolVersion(params.first().copied().unwrap_or(0)),
            E::ERROR => Event::Error(params.to_vec()),
            other => Event::Unknown { id: other, params: params.to_vec() },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: u8, params: &[u8]) -> Vec<u8> {
        let mut r = vec![STOP; REPORT_SIZE];
        r[0] = id;
        r[1..=params.len()].copy_from_slice(params);
        r
    }

    #[test]
    fn encode_set_layer_on() {
        let p = Command::SetLayer { on: true, layer: 3 }.encode();
        assert_eq!(&p[..3], &[0x04, 0x01, 0x03]);
        assert!(p[3..].iter().all(|&b| b == STOP));
    }

    #[test]
    fn encode_set_layer_off() {
        let p = Command::SetLayer { on: false, layer: 2 }.encode();
        assert_eq!(&p[..3], &[0x04, 0x00, 0x02]);
    }

    #[test]
    fn encode_rgb_led() {
        let p = Command::SetRgbLed { led: 12, r: 255, g: 128, b: 0 }.encode();
        assert_eq!(&p[..5], &[0x06, 12, 255, 128, 0]);
    }

    #[test]
    fn encode_protocol_version_probe_is_all_stop_bytes() {
        assert_eq!(Command::GetProtocolVersion.encode(), [STOP; REPORT_SIZE]);
    }

    #[test]
    fn decode_layer_event() {
        assert_eq!(Event::decode(&report(0x05, &[2])), Some(Event::Layer(2)));
    }

    #[test]
    fn decode_keydown_col_row_order() {
        // oryx.c sends (col, row) in that order.
        assert_eq!(
            Event::decode(&report(0x06, &[4, 1])),
            Some(Event::KeyDown { col: 4, row: 1 })
        );
    }

    #[test]
    fn decode_fw_version_string() {
        assert_eq!(
            Event::decode(&report(0x00, b"AbCd3/latest")),
            Some(Event::FwVersion("AbCd3/latest".into()))
        );
    }

    #[test]
    fn decode_protocol_version() {
        assert_eq!(Event::decode(&report(0xFE, &[4])), Some(Event::ProtocolVersion(4)));
    }

    #[test]
    fn decode_empty_report() {
        assert_eq!(Event::decode(&[]), None);
    }

    #[test]
    fn decode_pairing_success() {
        assert_eq!(Event::decode(&report(0x04, &[])), Some(Event::PairingSuccess));
    }
}
