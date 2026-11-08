//! Key action representation for the keyboard protocol.
//!
//! [`KeyAction`] maps 1:1 to the protocol's 4-byte config format
//! `[config_type, b1, b2, b3]` used in GET/SET_KEYMATRIX responses.
//!
//! # Parsing syntax
//!
//! ```text
//! A            → Key(0x04)
//! Escape       → Key(0x29)
//! Esc          → Key(0x29)       (alias)
//! 0x04         → Key(0x04)       (hex literal)
//! Ctrl+C       → Combo([LCtrl, C])
//! Shift+Alt+F3 → Combo([LShift, LAlt, F3])   (three usage slots max)
//! Mouse1       → Mouse(1)
//! Macro(0)     → Macro(index=0, repeat)
//! Macro(2,hold)→ Macro(index=2, hold-to-repeat)
//! Gamepad(1)   → Gamepad(1)
//! Fn           → Fn (layer modifier)
//! Disabled     → Disabled
//! ```

use crate::protocol::hid;
use monsgeek_transport::protocol::HidUsage;
use std::fmt;
use std::str::FromStr;

/// Usage slots in one keymatrix entry: bytes 1..=3, all pressed together.
pub const CHORD_SLOTS: usize = 3;

/// Consumer-page usages the firmware accepts in a keymatrix slot (config_type 3),
/// with the names used for both display and parsing so the two can't drift.
pub const CONSUMER_KEYS: &[(u16, &str)] = &[
    (0x00B5, "Next Track"),
    (0x00B6, "Previous Track"),
    (0x00B7, "Stop"),
    (0x00CD, "Play/Pause"),
    (0x00E2, "Mute"),
    (0x00E9, "Volume Up"),
    (0x00EA, "Volume Down"),
    (0x006F, "Brightness Up"),
    (0x0070, "Brightness Down"),
    (0x0183, "Word Processor"),
    (0x018A, "Mail"),
    (0x0192, "Calculator"),
    (0x0194, "My Computer"),
    (0x0221, "Search"),
    (0x0223, "Browser Home"),
];

/// Name for a consumer usage, if it has one.
fn consumer_name(code: u16) -> Option<&'static str> {
    CONSUMER_KEYS
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, n)| n)
}

/// Protocol config_type constants for the 4-byte key config.
mod config_type {
    pub const KEY: u8 = 0;
    pub const MOUSE: u8 = 1;
    pub const CONSUMER: u8 = 3;
    pub const PROFILE_SWITCH: u8 = 8;
    pub const MACRO: u8 = 9;
    pub const SPECIAL_FN: u8 = 10;
    pub const LED_CONTROL: u8 = 13;
    pub const CONNECTION_MODE: u8 = 14;
    pub const KNOB: u8 = 18;
    pub const GAMEPAD: u8 = 21;
}

/// Sub-function IDs for config_type SPECIAL_FN (10).
///
/// Complete map, verified against v407 `keycode_dispatch` case 10 (Ghidra @ 0x08012088).
/// Each sub toggles/sets a `g_fw_config` flag or an internal held-state bit; the
/// comments record the exact firmware effect.
///
/// Every sub NOT listed here is a firmware **no-op**: explicitly `sub 0, 4, 6, 7,
/// 0xf–0x16`, and — because the inner switch has no arm for them — everything
/// `>= 0x18` (falls through `default: return`). So e.g. `SpecialFn(0x18)` (the
/// webapp's "Fn+O") does nothing on v407.
mod special_fn {
    /// Fn held-modifier: sets bt_flags bit 1 while pressed.
    pub const FN_KEY: u8 = 1;
    /// Sets g_action_key_state bit 0x10 while held (game-mode action-key bitmap).
    pub const GAME_MODE: u8 = 2;
    /// Toggle flags1 bit 0 — guarded: only when flags1 & 6 == 0 (not in Mac/iOS). BT notify.
    pub const WIN_LOCK: u8 = 3;
    // sub 4: no-op
    /// flags1 bits[2:1] <- b2 (0=Windows, 1=Mac, 2=iOS); b2=3 cycles.
    pub const OS_MODE: u8 = 5;
    // sub 6, 7: no-op
    /// Enter pairing (GPIO + kbd_state+0x45), only when connection mode != USB(6).
    pub const BT_PAIRING: u8 = 8;
    /// Toggle flags1 bit 4 — same bit as [`FN_LOCK`] but SILENT (no bt_event_queue).
    pub const FN_TOGGLE: u8 = 9;
    /// Toggle flags1 bit 3; apply_config_changes + clear reports; BT notify.
    pub const WASD_SWAP: u8 = 0x0a;
    /// Toggle flags2 bit 0 (NKRO); apply_config_changes + clear reports; BT notify.
    ///
    /// That is the M1 V5 v407 firmware. On a FUN60 PRO (ID 2600, fw 0x0402) the
    /// factory `Fn+RAlt` carries this sub-function and pressing it flips the Fn
    /// layer instead (`GET_KBOPTION` byte 2, `fnIndex`, 1 -> 0), so the meaning is
    /// firmware-specific.
    pub const NKRO_TOGGLE: u8 = 0x0b;
    /// Toggle flags1 bit 4 (same bit as [`FN_TOGGLE`]) WITH bt_event_queue(3).
    pub const FN_LOCK: u8 = 0x0c;
    /// Toggle flags2 bit 1 — 6KRO<->NKRO report select; clears reports; BT notify.
    pub const REPORT_MODE: u8 = 0x0d;
    /// Toggle flags2 bit 2 — switches which pair of keymatrix entries the encoder's
    /// rotation drives; BT notify.
    ///
    /// `encoder_knob_dispatch` (v407 @ 0x08019fdc) reads rotation from positions
    /// 90/91 and offsets both by `+0x18` (six positions, to 96/97) while this bit is
    /// set; the knob's click is *not* offset. On the M1 V5 TMR that turns the knob
    /// from volume (consumer 0xE9/0xEA at 90/91) into LED brightness (LED_CONTROL
    /// b1=2 at 96/97), and the click — matrix position 92, which holds this very
    /// sub-function — is what flips between them.
    pub const KNOB_MODE_TOGGLE: u8 = 0x0e;
    // sub 0xf-0x16: no-op
    /// Sets g_action_key_state bit 0x80 while held (webapp labels this an "AI/DeepSeek" key;
    /// firmware just latches the held-state bit — consumer not fully traced).
    pub const RCTRL_MOD: u8 = 0x17;
}

/// What action a key performs when pressed.
///
/// Maps 1:1 to the protocol's 4-byte config format `[config_type, b1, b2, b3]`.
/// Use [`from_config_bytes`](KeyAction::from_config_bytes) to decode from wire
/// format and [`to_config_bytes`](KeyAction::to_config_bytes) to encode.
///
/// Implements [`FromStr`] for parsing human-readable syntax and [`Display`]
/// for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Disabled / no action (config_type=0, keycode=0).
    Disabled,
    /// Single HID keycode (config_type=0).
    Key(HidUsage),
    /// A chord of 2–3 HID usages pressed together (config_type=0).
    ///
    /// A keymatrix slot holds **three independent usage codes** in `b1`/`b2`/`b3`,
    /// all pressed on key-down (firmware `keycode_dispatch` case 0 calls
    /// `hid_key_press` on each). Modifiers are ordinary usages `0xE0..=0xE7`, not a
    /// bitmask — `hid_key_press` turns those into report modifier bits itself.
    /// So Ctrl+C is `[0xE0, 0x06, 0]`, matching the vendor's `configToMatrix`
    /// `[0, skey, key, key2]`.
    ///
    /// Trailing unused slots are zero; use [`KeyAction::chord`] to build one.
    Combo { keys: [HidUsage; 3] },
    /// Mouse button (config_type=1).
    Mouse(u8),
    /// Macro assignment (config_type=9).
    ///
    /// `kind`: 0=repeat by count, 1=toggle, 2=hold to repeat.
    Macro { index: u8, kind: u8 },
    /// Consumer/media key — USB HID Consumer Page usage ID (config_type=3).
    Consumer(u16),
    /// Gamepad button (config_type=21).
    Gamepad(u8),
    /// Fn layer modifier key (config_type=10, sub=1).
    Fn,
    /// Special function key (config_type=10, sub != 1).
    ///
    /// Sub-function ID in `sub`, extra data in `b2`/`b3`.
    SpecialFn { sub: u8, b2: u8, b3: u8 },
    /// Profile switch (config_type=8).
    ///
    /// `action`: 1=next, 2=prev, 3=cycle, 4=switch to specific `index`.
    ProfileSwitch { action: u8, index: u8 },
    /// Connection mode switch (config_type=14).
    ///
    /// `b1`=0: mode select (`b2`: 0=BT1, 1=BT2, 2=BT3, 5=2.4G, 6=USB).
    /// `b1`=1: pairing (`b2`: 0=2.4G pair, 1=BT pair).
    ConnectionMode { b1: u8, b2: u8, b3: u8 },
    /// LED brightness/effect control (config_type=13).
    LedControl { data: [u8; 3] },
    /// Knob/encoder action (config_type=18).
    Knob { data: [u8; 3] },
    /// Unknown/unsupported config type (preserved as raw bytes).
    Unknown { config_type: u8, data: [u8; 3] },
}

// Slots whose usage is not live (see `HidUsage::is_live`) are dropped on decode: an
// older release of this driver wrote a modifier *bitmask* into `b1`, and e.g.
// `[0, 0x01, 0x06, 0]` really does emit a bare "C" on the device. Decoding it as
// `Key(C)` reports what the key does, and the stale byte is rewritten canonically
// the next time the key is edited.
impl KeyAction {
    /// Encode to the 4-byte config format used in GET/SET_KEYMATRIX.
    pub fn to_config_bytes(self) -> [u8; 4] {
        match self {
            KeyAction::Disabled => [0, 0, 0, 0],
            // A lone usage goes in b2, matching the vendor's `hidToMatrix`.
            KeyAction::Key(code) => [0, 0, code.get(), 0],
            KeyAction::Combo { keys } => [0, keys[0].get(), keys[1].get(), keys[2].get()],
            KeyAction::Mouse(btn) => [config_type::MOUSE, 0, btn, 0],
            KeyAction::Consumer(code) => [config_type::CONSUMER, 0, code as u8, (code >> 8) as u8],
            KeyAction::Macro { index, kind } => [config_type::MACRO, kind, index, 0],
            KeyAction::Gamepad(btn) => [config_type::GAMEPAD, 0, btn, 0],
            KeyAction::Fn => [config_type::SPECIAL_FN, special_fn::FN_KEY, 0, 0],
            KeyAction::SpecialFn { sub, b2, b3 } => [config_type::SPECIAL_FN, sub, b2, b3],
            KeyAction::ProfileSwitch { action, index } => {
                [config_type::PROFILE_SWITCH, 0, action, index]
            }
            KeyAction::ConnectionMode { b1, b2, b3 } => [config_type::CONNECTION_MODE, b1, b2, b3],
            KeyAction::LedControl { data } => [config_type::LED_CONTROL, data[0], data[1], data[2]],
            KeyAction::Knob { data } => [config_type::KNOB, data[0], data[1], data[2]],
            KeyAction::Unknown { config_type, data } => [config_type, data[0], data[1], data[2]],
        }
    }

    /// Build the canonical action for a set of HID usages pressed together,
    /// ignoring zeros and firmware no-ops. Extra usages beyond three are dropped —
    /// the wire format has exactly three slots.
    pub fn chord(usages: impl IntoIterator<Item = HidUsage>) -> Self {
        let mut keys = [HidUsage::NONE; 3];
        let mut n = 0;
        for u in usages {
            if !u.is_live() || n == keys.len() {
                continue;
            }
            keys[n] = u;
            n += 1;
        }
        match n {
            0 => KeyAction::Disabled,
            1 => KeyAction::Key(keys[0]),
            _ => KeyAction::Combo { keys },
        }
    }

    /// Decode from the 4-byte config format returned by GET_KEYMATRIX.
    ///
    /// For `config_type == 0` the firmware presses `b2`, `b1`, `b3` as three
    /// independent HID usages, so decoding just collects the live ones and
    /// normalises: none → `Disabled`, one → `Key`, two or three → `Combo`.
    /// A lone usage therefore re-encodes into `b2` regardless of which slot it
    /// came from; the firmware presses every slot identically, so that is a
    /// behaviour-preserving normalisation.
    pub fn from_config_bytes(bytes: [u8; 4]) -> Self {
        match bytes[0] {
            // Slot order b1, b2, b3 mirrors the vendor's [0, skey, key, key2].
            config_type::KEY => KeyAction::chord([bytes[1], bytes[2], bytes[3]].map(HidUsage::new)),
            config_type::MOUSE => KeyAction::Mouse(bytes[2]),
            config_type::CONSUMER => {
                let code = bytes[2] as u16 | (bytes[3] as u16) << 8;
                KeyAction::Consumer(code)
            }
            config_type::MACRO => KeyAction::Macro {
                index: bytes[2],
                kind: bytes[1],
            },
            config_type::GAMEPAD => KeyAction::Gamepad(bytes[2]),
            config_type::PROFILE_SWITCH => KeyAction::ProfileSwitch {
                action: bytes[2],
                index: bytes[3],
            },
            config_type::SPECIAL_FN if bytes[1] == special_fn::FN_KEY => KeyAction::Fn,
            config_type::SPECIAL_FN => KeyAction::SpecialFn {
                sub: bytes[1],
                b2: bytes[2],
                b3: bytes[3],
            },
            config_type::LED_CONTROL => KeyAction::LedControl {
                data: [bytes[1], bytes[2], bytes[3]],
            },
            config_type::CONNECTION_MODE => KeyAction::ConnectionMode {
                b1: bytes[1],
                b2: bytes[2],
                b3: bytes[3],
            },
            config_type::KNOB => KeyAction::Knob {
                data: [bytes[1], bytes[2], bytes[3]],
            },
            ct => KeyAction::Unknown {
                config_type: ct,
                data: [bytes[1], bytes[2], bytes[3]],
            },
        }
    }
}

/// Parameterless SPECIAL_FN actions, by sub-function id.
///
/// Shared by `Display` and `FromStr` so the two cannot drift. A name that prints
/// but does not parse makes `remap` a one-way door: the binding can be read off
/// the device and shown, then never written back.
const SPECIAL_FN_NAMES: &[(u8, &str)] = &[
    (special_fn::GAME_MODE, "Game Mode"),
    (special_fn::WIN_LOCK, "Win Lock"),
    (special_fn::BT_PAIRING, "BT Pairing"),
    (special_fn::FN_TOGGLE, "Fn Toggle"),
    (special_fn::WASD_SWAP, "WASD Swap"),
    (special_fn::NKRO_TOGGLE, "NKRO Toggle"),
    (special_fn::FN_LOCK, "Fn Lock"),
    (special_fn::REPORT_MODE, "Report Mode"),
    (special_fn::KNOB_MODE_TOGGLE, "Knob Mode Toggle"),
    (special_fn::RCTRL_MOD, "RCtrl Modifier"),
];

/// LED_CONTROL actions whose three payload bytes are fixed, by those bytes.
/// Shared by `Display` and `FromStr` for the same reason.
const LED_CONTROL_NAMES: &[([u8; 3], &str)] = &[
    ([2, 1, 0], "LED Brightness Up"),
    ([2, 2, 0], "LED Brightness Down"),
    ([3, 1, 0], "LED Speed Up"),
    ([3, 2, 0], "LED Speed Down"),
];

impl fmt::Display for KeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyAction::Disabled => write!(f, "Disabled"),
            KeyAction::Key(code) => write!(f, "{}", hid::key_name(*code)),
            KeyAction::Combo { keys } => {
                let mut first = true;
                for &usage in keys.iter().filter(|&&u| u.is_live()) {
                    if !first {
                        write!(f, "+")?;
                    }
                    write!(f, "{}", hid::key_name(usage))?;
                    first = false;
                }
                Ok(())
            }
            KeyAction::Consumer(code) => match consumer_name(*code) {
                Some(name) => write!(f, "{name}"),
                None => write!(f, "Consumer(0x{code:04X})"),
            },
            KeyAction::Mouse(btn) => write!(f, "Mouse{btn}"),
            KeyAction::Macro { index, kind } => match kind {
                0 => write!(f, "Macro({index})"),
                1 => write!(f, "Macro({index},toggle)"),
                2 => write!(f, "Macro({index},hold)"),
                k => write!(f, "Macro({index},type{k})"),
            },
            KeyAction::Gamepad(btn) => write!(f, "Gamepad({btn})"),
            KeyAction::Fn => write!(f, "Fn"),
            KeyAction::SpecialFn { sub, b2, b3 } => {
                if *sub == special_fn::OS_MODE {
                    return match b2 {
                        0 => write!(f, "OS: Windows"),
                        1 => write!(f, "OS: Mac"),
                        2 => write!(f, "OS: iOS"),
                        3 => write!(f, "OS: Cycle"),
                        _ => write!(f, "OS Mode({b2})"),
                    };
                }
                match SPECIAL_FN_NAMES.iter().find(|(id, _)| id == sub) {
                    Some((_, name)) => write!(f, "{name}"),
                    None => write!(f, "SpecialFn({sub},{b2},{b3})"),
                }
            }
            KeyAction::ProfileSwitch { action, index } => match action {
                1 => write!(f, "Profile Next"),
                2 => write!(f, "Profile Prev"),
                3 => write!(f, "Profile Cycle"),
                4 => write!(f, "Profile {}", index + 1),
                _ => write!(f, "ProfileSwitch({action},{index})"),
            },
            KeyAction::ConnectionMode { b1, b2, .. } => {
                if *b1 == 1 {
                    match b2 {
                        0 => write!(f, "Pair 2.4G"),
                        1 => write!(f, "Pair BT"),
                        _ => write!(f, "Pair({b2})"),
                    }
                } else {
                    // b2 is 0-indexed BT slot; b2=3,4 are no-ops in firmware
                    match b2 {
                        0 => write!(f, "BT1"),
                        1 => write!(f, "BT2"),
                        2 => write!(f, "BT3"),
                        5 => write!(f, "2.4GHz"),
                        6 => write!(f, "USB"),
                        _ => write!(f, "Connection({b2})"),
                    }
                }
            }
            KeyAction::LedControl { data } => {
                // The b1-only actions ignore b2/b3, so they are matched on b1 alone
                // and deliberately kept out of the round-tripping table.
                let wide = match data[0] {
                    1 => Some("LED Mode Cycle"),
                    5 => Some("LED Direction"),
                    6 => Some("LED Layer Select"),
                    _ => None,
                };
                let exact = LED_CONTROL_NAMES
                    .iter()
                    .find(|(bytes, _)| bytes == data)
                    .map(|(_, name)| *name);
                match exact.or(wide) {
                    Some(name) => write!(f, "{name}"),
                    None => write!(f, "LedControl({},{},{})", data[0], data[1], data[2]),
                }
            }
            KeyAction::Knob { data } => {
                write!(f, "Knob({},{},{})", data[0], data[1], data[2])
            }
            KeyAction::Unknown {
                config_type,
                data: [b1, b2, b3],
            } => write!(
                f,
                "Unknown(type={config_type},data=[{b1:#04x},{b2:#04x},{b3:#04x}])"
            ),
        }
    }
}

/// Error type for parsing a [`KeyAction`] from a string.
#[derive(Debug, Clone)]
pub enum ParseKeyActionError {
    UnknownKey(String),
    /// More `+`-separated keys than the wire format's three usage slots.
    TooManyKeys(usize),
    InvalidHexCode,
    InvalidMouseButton,
    InvalidMacroIndex,
    InvalidGamepadButton,
    EmptyCombo,
}

impl fmt::Display for ParseKeyActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(name) => write!(f, "unknown key: \"{name}\""),
            Self::TooManyKeys(n) => write!(f, "a chord holds at most {CHORD_SLOTS} keys, got {n}"),
            Self::InvalidHexCode => write!(f, "invalid hex keycode"),
            Self::InvalidMouseButton => write!(f, "invalid mouse button number"),
            Self::InvalidMacroIndex => write!(f, "invalid macro index"),
            Self::InvalidGamepadButton => write!(f, "invalid gamepad button number"),
            Self::EmptyCombo => write!(f, "empty key combo"),
        }
    }
}

impl std::error::Error for ParseKeyActionError {}

impl FromStr for KeyAction {
    type Err = ParseKeyActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Disabled / None
        match s.to_ascii_lowercase().as_str() {
            "disabled" | "none" | "off" => return Ok(KeyAction::Disabled),
            "fn" => return Ok(KeyAction::Fn),
            _ => {}
        }

        // The named firmware actions, from the same tables `Display` prints, so
        // anything the listing shows can be written back.
        if let Some((sub, _)) = SPECIAL_FN_NAMES
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(s))
        {
            return Ok(KeyAction::SpecialFn {
                sub: *sub,
                b2: 0,
                b3: 0,
            });
        }
        if let Some((data, _)) = LED_CONTROL_NAMES
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(s))
        {
            return Ok(KeyAction::LedControl { data: *data });
        }
        for (b2, label) in [(0u8, "Windows"), (1, "Mac"), (2, "iOS"), (3, "Cycle")] {
            if s.eq_ignore_ascii_case(&format!("OS: {label}")) {
                return Ok(KeyAction::SpecialFn {
                    sub: special_fn::OS_MODE,
                    b2,
                    b3: 0,
                });
            }
        }

        // Mouse button: "Mouse1", "mouse(2)"
        if let Some(rest) = s.strip_prefix("Mouse").or_else(|| s.strip_prefix("mouse")) {
            let inner = rest.trim_start_matches('(').trim_end_matches(')');
            let btn: u8 = inner
                .parse()
                .map_err(|_| ParseKeyActionError::InvalidMouseButton)?;
            return Ok(KeyAction::Mouse(btn));
        }

        // Macro: "Macro(0)", "Macro(1,toggle)", "Macro(2,hold)"
        if let Some(rest) = s.strip_prefix("Macro").or_else(|| s.strip_prefix("macro")) {
            let inner = rest.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            let index: u8 = parts[0]
                .trim()
                .parse()
                .map_err(|_| ParseKeyActionError::InvalidMacroIndex)?;
            let kind = if parts.len() > 1 {
                match parts[1].trim().to_ascii_lowercase().as_str() {
                    "toggle" => 1,
                    "hold" => 2,
                    "repeat" | "count" => 0,
                    other => other.parse().unwrap_or(0),
                }
            } else {
                0
            };
            return Ok(KeyAction::Macro { index, kind });
        }

        // Gamepad: "Gamepad(1)", "gamepad1"
        if let Some(rest) = s
            .strip_prefix("Gamepad")
            .or_else(|| s.strip_prefix("gamepad"))
        {
            let inner = rest.trim_start_matches('(').trim_end_matches(')');
            let btn: u8 = inner
                .parse()
                .map_err(|_| ParseKeyActionError::InvalidGamepadButton)?;
            return Ok(KeyAction::Gamepad(btn));
        }

        // Hex literal: "0x04", "0X2C"
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            let code =
                u8::from_str_radix(hex, 16).map_err(|_| ParseKeyActionError::InvalidHexCode)?;
            let usage = HidUsage::new(code);
            return Ok(if usage == HidUsage::NONE {
                KeyAction::Disabled
            } else {
                KeyAction::Key(usage)
            });
        }

        // Consumer/media usages, by name or by the `Consumer(0x00E9)` form that
        // Display emits for codes with no name. Checked before the chord split so
        // names containing '+' (none today, but "Vol+" is the obvious future one)
        // can't be mistaken for a chord.
        if let Some(&(code, _)) = CONSUMER_KEYS
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(s))
        {
            return Ok(KeyAction::Consumer(code));
        }
        if let Some(rest) = s
            .strip_prefix("Consumer(")
            .or_else(|| s.strip_prefix("consumer("))
            .and_then(|r| r.strip_suffix(')'))
        {
            let hex = rest
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let code =
                u16::from_str_radix(hex, 16).map_err(|_| ParseKeyActionError::InvalidHexCode)?;
            return Ok(KeyAction::Consumer(code));
        }

        // Chord: "Ctrl+C", "Shift+Alt+F3". Every token is resolved through the same
        // usage table — modifiers are just usages 0xE0..=0xE7 — mirroring the vendor
        // app, which maps all three chord slots through one `htmlCodeMapHIDCode`.
        //
        // A whole-string match wins over splitting, so keys whose own name contains
        // the separator (`KP+`) stay reachable.
        if s.contains('+') && hid::key_code_from_name(s).is_none() {
            let parts: Vec<&str> = s.split('+').map(str::trim).collect();
            if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
                return Err(ParseKeyActionError::EmptyCombo);
            }
            if parts.len() > CHORD_SLOTS {
                return Err(ParseKeyActionError::TooManyKeys(parts.len()));
            }

            let mut usages = Vec::with_capacity(parts.len());
            for &part in &parts {
                usages.push(
                    hid::key_code_from_name(part)
                        .ok_or_else(|| ParseKeyActionError::UnknownKey(part.to_string()))?,
                );
            }
            return Ok(KeyAction::chord(usages));
        }

        // Plain key name: "A", "Enter", "F3", "CapsLock"
        let code = hid::key_code_from_name(s)
            .ok_or_else(|| ParseKeyActionError::UnknownKey(s.to_string()))?;
        Ok(if code == HidUsage::NONE {
            KeyAction::Disabled
        } else {
            KeyAction::Key(code)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FromStr tests ---

    #[test]
    fn parse_disabled() {
        assert_eq!(
            "Disabled".parse::<KeyAction>().unwrap(),
            KeyAction::Disabled
        );
        assert_eq!("none".parse::<KeyAction>().unwrap(), KeyAction::Disabled);
        assert_eq!("off".parse::<KeyAction>().unwrap(), KeyAction::Disabled);
        assert_eq!("0x00".parse::<KeyAction>().unwrap(), KeyAction::Disabled);
    }

    #[test]
    fn parse_plain_key() {
        assert_eq!(
            "A".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x04))
        );
        assert_eq!(
            "a".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x04))
        );
        assert_eq!(
            "Enter".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x28))
        );
        assert_eq!(
            "Escape".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x29))
        );
        assert_eq!(
            "Esc".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x29))
        );
        assert_eq!(
            "F3".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x3C))
        );
        assert_eq!(
            "F12".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x45))
        );
        assert_eq!(
            "Space".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x2C))
        );
    }

    #[test]
    fn parse_hex() {
        assert_eq!(
            "0x04".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x04))
        );
        assert_eq!(
            "0x29".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x29))
        );
        assert_eq!(
            "0xE0".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0xE0))
        );
    }

    #[test]
    fn parse_f13_through_f24() {
        assert_eq!(
            "F13".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x68))
        );
        assert_eq!(
            "F24".parse::<KeyAction>().unwrap(),
            KeyAction::Key(HidUsage::new(0x73))
        );
    }

    #[test]
    fn parse_combo() {
        assert_eq!(
            "Ctrl+C".parse::<KeyAction>().unwrap(),
            KeyAction::Combo {
                keys: [HidUsage::new(0xE0), HidUsage::new(0x06), HidUsage::new(0)]
            }
        );
        assert_eq!(
            "Shift+Alt+F3".parse::<KeyAction>().unwrap(),
            KeyAction::Combo {
                keys: [
                    HidUsage::new(0xE1),
                    HidUsage::new(0xE2),
                    HidUsage::new(0x3C)
                ]
            }
        );
        assert_eq!(
            "RCtrl+RShift+A".parse::<KeyAction>().unwrap(),
            KeyAction::Combo {
                keys: [
                    HidUsage::new(0xE4),
                    HidUsage::new(0xE5),
                    HidUsage::new(0x04)
                ]
            }
        );
    }

    /// The wire format has exactly three usage slots.
    #[test]
    fn parse_combo_rejects_more_than_three_keys() {
        assert!(matches!(
            "Ctrl+Alt+Shift+A".parse::<KeyAction>(),
            Err(ParseKeyActionError::TooManyKeys(4))
        ));
    }

    #[test]
    fn parse_mouse() {
        assert_eq!("Mouse1".parse::<KeyAction>().unwrap(), KeyAction::Mouse(1));
        assert_eq!(
            "mouse(3)".parse::<KeyAction>().unwrap(),
            KeyAction::Mouse(3)
        );
    }

    #[test]
    fn parse_macro() {
        assert_eq!(
            "Macro(0)".parse::<KeyAction>().unwrap(),
            KeyAction::Macro { index: 0, kind: 0 }
        );
        assert_eq!(
            "Macro(1,toggle)".parse::<KeyAction>().unwrap(),
            KeyAction::Macro { index: 1, kind: 1 }
        );
        assert_eq!(
            "Macro(2,hold)".parse::<KeyAction>().unwrap(),
            KeyAction::Macro { index: 2, kind: 2 }
        );
    }

    #[test]
    fn parse_gamepad() {
        assert_eq!(
            "Gamepad(5)".parse::<KeyAction>().unwrap(),
            KeyAction::Gamepad(5)
        );
    }

    #[test]
    fn parse_fn() {
        assert_eq!("Fn".parse::<KeyAction>().unwrap(), KeyAction::Fn);
        assert_eq!("fn".parse::<KeyAction>().unwrap(), KeyAction::Fn);
    }

    #[test]
    fn parse_error_unknown_key() {
        assert!("Foobar".parse::<KeyAction>().is_err());
    }

    #[test]
    fn parse_error_unknown_modifier() {
        assert!("Hyper+A".parse::<KeyAction>().is_err());
    }

    // --- Display tests ---

    #[test]
    fn display_disabled() {
        assert_eq!(KeyAction::Disabled.to_string(), "Disabled");
    }

    #[test]
    fn display_key() {
        assert_eq!(KeyAction::Key(HidUsage::new(0x04)).to_string(), "A");
        assert_eq!(KeyAction::Key(HidUsage::new(0x29)).to_string(), "Escape");
        assert_eq!(KeyAction::Key(HidUsage::new(0x28)).to_string(), "Enter");
    }

    #[test]
    fn display_combo() {
        assert_eq!(
            KeyAction::Combo {
                keys: [HidUsage::new(0xE0), HidUsage::new(0x06), HidUsage::new(0)]
            }
            .to_string(),
            "LCtrl+C"
        );
        assert_eq!(
            KeyAction::Combo {
                keys: [
                    HidUsage::new(0xE1),
                    HidUsage::new(0xE2),
                    HidUsage::new(0x3C)
                ]
            }
            .to_string(),
            "LShift+LAlt+F3"
        );
    }

    #[test]
    fn display_mouse() {
        assert_eq!(KeyAction::Mouse(1).to_string(), "Mouse1");
    }

    #[test]
    fn display_macro() {
        assert_eq!(
            KeyAction::Macro { index: 0, kind: 0 }.to_string(),
            "Macro(0)"
        );
        assert_eq!(
            KeyAction::Macro { index: 1, kind: 1 }.to_string(),
            "Macro(1,toggle)"
        );
        assert_eq!(
            KeyAction::Macro { index: 2, kind: 2 }.to_string(),
            "Macro(2,hold)"
        );
    }

    #[test]
    fn display_gamepad() {
        assert_eq!(KeyAction::Gamepad(5).to_string(), "Gamepad(5)");
    }

    #[test]
    fn display_fn() {
        assert_eq!(KeyAction::Fn.to_string(), "Fn");
    }

    #[test]
    fn display_unknown() {
        let u = KeyAction::Unknown {
            config_type: 22,
            data: [0x01, 0x02, 0x03],
        };
        assert_eq!(u.to_string(), "Unknown(type=22,data=[0x01,0x02,0x03])");
    }

    // --- Wire roundtrip tests ---

    #[test]
    fn wire_roundtrip_disabled() {
        let a = KeyAction::Disabled;
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_roundtrip_key() {
        let a = KeyAction::Key(HidUsage::new(0x04));
        assert_eq!(a.to_config_bytes(), [0, 0, 0x04, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_user_remap_format() {
        // Firmware stores user remaps with keycode at byte 1 (byte 2 = 0)
        assert_eq!(
            KeyAction::from_config_bytes([0, 0x04, 0, 0]),
            KeyAction::Key(HidUsage::new(0x04)) // A
        );
        assert_eq!(
            KeyAction::from_config_bytes([0, 0x29, 0, 0]),
            KeyAction::Key(HidUsage::new(0x29)) // Escape
        );
    }

    #[test]
    fn wire_roundtrip_combo() {
        let a = KeyAction::Combo {
            keys: [
                HidUsage::new(0xE0),
                HidUsage::new(0xE1),
                HidUsage::new(0x06),
            ],
        };
        assert_eq!(a.to_config_bytes(), [0, 0xE0, 0xE1, 0x06]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    /// Byte-for-byte parity with the vendor webapp's `configToMatrix`, which emits
    /// `case "combo": [0, skey, key, key2]` with modifiers as ordinary usages.
    #[test]
    fn vendor_parity_chord_encoding() {
        for (spec, want) in [
            ("A", [0, 0, 0x04, 0]),
            ("Ctrl+C", [0, 0xE0, 0x06, 0]),
            ("Ctrl+Shift+Escape", [0, 0xE0, 0xE1, 0x29]),
        ] {
            let action: KeyAction = spec.parse().unwrap();
            assert_eq!(action.to_config_bytes(), want, "encoding {spec}");
        }
    }

    /// `hid_key_press` (v407 @ 0x080078f4) returns early for usages below 0x04, so a
    /// slot holding one is a firmware no-op. An older release wrote a modifier
    /// *bitmask* into b1; `[0, 0x01, 0x06, 0]` really does emit a bare "C".
    #[test]
    fn legacy_bitmask_bytes_decode_to_what_the_key_emits() {
        assert_eq!(
            KeyAction::from_config_bytes([0, 0x01, 0x06, 0]),
            KeyAction::Key(HidUsage::new(0x06))
        );
        // LAlt's old bitmask 0x04 collides with a real usage ("A"), so that one
        // stays a chord — it is genuinely what the firmware presses.
        assert_eq!(
            KeyAction::from_config_bytes([0, 0x04, 0x06, 0]),
            KeyAction::Combo {
                keys: [HidUsage::new(0x04), HidUsage::new(0x06), HidUsage::new(0)]
            }
        );
    }

    /// Every usage `Display` can name must parse back to itself, so the two HID
    /// name tables can't drift apart (this is what caught `key_name` collapsing
    /// 0x68..=0x73 to a single "F13-F24" label).
    #[test]
    fn parse_display_roundtrip_over_every_usage() {
        for code in (0x04..=0x73u8).chain(0xE0..=0xE7).map(HidUsage::new) {
            let name = hid::key_name(code);
            if name == "?" {
                continue;
            }
            let action = KeyAction::Key(code);
            assert_eq!(
                name.parse::<KeyAction>().unwrap(),
                action,
                "{name} ({:#04x}) did not parse back",
                code.get()
            );
            assert_eq!(action.to_string(), name);
        }
    }

    /// Every named consumer usage must survive Display -> parse, and unnamed ones
    /// must survive through the `Consumer(0xNNNN)` form Display falls back to.
    #[test]
    fn parse_display_roundtrip_over_consumer_usages() {
        for &(code, name) in CONSUMER_KEYS {
            let action = KeyAction::Consumer(code);
            assert_eq!(action.to_string(), name);
            assert_eq!(name.parse::<KeyAction>().unwrap(), action, "{name}");
        }
        let unnamed = KeyAction::Consumer(0x0042);
        assert_eq!(unnamed.to_string(), "Consumer(0x0042)");
        assert_eq!(unnamed.to_string().parse::<KeyAction>().unwrap(), unnamed);
    }

    /// Same, for multi-usage chords.
    #[test]
    fn parse_display_roundtrip_over_chords() {
        for spec in ["LCtrl+C", "LShift+LAlt+F3", "LCtrl+LShift+Escape", "A+B"] {
            let action: KeyAction = spec.parse().unwrap();
            assert_eq!(action.to_string(), spec);
            assert_eq!(action.to_string().parse::<KeyAction>().unwrap(), action);
        }
    }

    /// Decoding normalises slot placement, so a second pass must be a no-op.
    #[test]
    fn decode_is_idempotent() {
        const SLOTS: [u8; 8] = [0, 0x01, 0x04, 0x06, 0x29, 0x68, 0xE0, 0xE1];
        for &b1 in &SLOTS {
            for &b2 in &SLOTS {
                for &b3 in &SLOTS {
                    let once = KeyAction::from_config_bytes([0, b1, b2, b3]);
                    let twice = KeyAction::from_config_bytes(once.to_config_bytes());
                    assert_eq!(once, twice, "[0, {b1:#04x}, {b2:#04x}, {b3:#04x}]");
                }
            }
        }
    }

    #[test]
    fn wire_roundtrip_mouse() {
        let a = KeyAction::Mouse(1);
        assert_eq!(a.to_config_bytes(), [1, 0, 1, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_roundtrip_macro() {
        let a = KeyAction::Macro { index: 3, kind: 1 };
        assert_eq!(a.to_config_bytes(), [9, 1, 3, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_roundtrip_fn() {
        let a = KeyAction::Fn;
        assert_eq!(a.to_config_bytes(), [10, 1, 0, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_special_fn_decoded() {
        // config_type=10 with sub != 1 should decode as SpecialFn
        let bytes = [10, 0x0a, 0, 0]; // WASD Swap
        let a = KeyAction::from_config_bytes(bytes);
        assert_eq!(
            a,
            KeyAction::SpecialFn {
                sub: 0x0a,
                b2: 0,
                b3: 0
            }
        );
        assert_eq!(a.to_config_bytes(), bytes);
        assert_eq!(a.to_string(), "WASD Swap");
    }

    #[test]
    fn wire_roundtrip_gamepad() {
        let a = KeyAction::Gamepad(7);
        assert_eq!(a.to_config_bytes(), [21, 0, 7, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }

    #[test]
    fn wire_roundtrip_profile_switch() {
        // Profile 3 (index 2)
        let bytes = [8, 0, 4, 2];
        let a = KeyAction::from_config_bytes(bytes);
        assert_eq!(
            a,
            KeyAction::ProfileSwitch {
                action: 4,
                index: 2
            }
        );
        assert_eq!(a.to_config_bytes(), bytes);
        assert_eq!(a.to_string(), "Profile 3");
    }

    #[test]
    fn wire_roundtrip_connection_mode() {
        // BT1 (0-indexed slot 0)
        let bytes = [14, 0, 0, 0];
        let a = KeyAction::from_config_bytes(bytes);
        assert_eq!(
            a,
            KeyAction::ConnectionMode {
                b1: 0,
                b2: 0,
                b3: 0
            }
        );
        assert_eq!(a.to_config_bytes(), bytes);
        assert_eq!(a.to_string(), "BT1");

        // BT2, BT3
        assert_eq!(
            KeyAction::from_config_bytes([14, 0, 1, 0]).to_string(),
            "BT2"
        );
        assert_eq!(
            KeyAction::from_config_bytes([14, 0, 2, 0]).to_string(),
            "BT3"
        );

        // 2.4GHz
        assert_eq!(
            KeyAction::from_config_bytes([14, 0, 5, 0]).to_string(),
            "2.4GHz"
        );

        // USB
        assert_eq!(
            KeyAction::from_config_bytes([14, 0, 6, 0]).to_string(),
            "USB"
        );
    }

    #[test]
    fn wire_roundtrip_knob() {
        let bytes = [18, 1, 2, 3];
        let a = KeyAction::from_config_bytes(bytes);
        assert_eq!(a, KeyAction::Knob { data: [1, 2, 3] });
        assert_eq!(a.to_config_bytes(), bytes);
    }

    #[test]
    fn display_special_fn_variants() {
        assert_eq!(
            KeyAction::SpecialFn {
                sub: 2,
                b2: 0,
                b3: 0
            }
            .to_string(),
            "Game Mode"
        );
        assert_eq!(
            KeyAction::SpecialFn {
                sub: 3,
                b2: 0,
                b3: 0
            }
            .to_string(),
            "Win Lock"
        );
        assert_eq!(
            KeyAction::SpecialFn {
                sub: 8,
                b2: 0,
                b3: 0
            }
            .to_string(),
            "BT Pairing"
        );
        assert_eq!(
            KeyAction::SpecialFn {
                sub: 0x0c,
                b2: 0,
                b3: 0
            }
            .to_string(),
            "Fn Lock"
        );
        assert_eq!(
            KeyAction::SpecialFn {
                sub: 0x17,
                b2: 0,
                b3: 0
            }
            .to_string(),
            "RCtrl Modifier"
        );
    }

    #[test]
    fn wire_unknown_preserved() {
        let bytes = [22, 0x01, 0x02, 0x03];
        let a = KeyAction::from_config_bytes(bytes);
        assert_eq!(a.to_config_bytes(), bytes);
    }

    // --- Parse → Display roundtrip ---

    #[test]
    fn parse_display_roundtrip() {
        let cases = [
            "Disabled",
            "A",
            "Escape",
            "F3",
            "Ctrl+C",
            "Shift+Alt+F3",
            "Mouse1",
            "Macro(0)",
            "Macro(1,toggle)",
            "Gamepad(5)",
            "Fn",
        ];
        for input in cases {
            let action: KeyAction = input.parse().unwrap();
            let displayed = action.to_string();
            let reparsed: KeyAction = displayed.parse().unwrap();
            assert_eq!(action, reparsed, "roundtrip failed for {input:?}");
        }
    }

    // --- Consumer key tests ---

    #[test]
    fn parse_consumer() {
        // [3, 0, 0xe9, 0] → Consumer(0x00E9) = Volume Up
        assert_eq!(
            KeyAction::from_config_bytes([3, 0, 0xE9, 0]),
            KeyAction::Consumer(0x00E9)
        );
        // [3, 0, 146, 1] → Consumer(0x0192) = Calculator (146 + 1*256 = 402 = 0x192)
        assert_eq!(
            KeyAction::from_config_bytes([3, 0, 0x92, 0x01]),
            KeyAction::Consumer(0x0192)
        );
    }

    #[test]
    fn display_consumer_known() {
        assert_eq!(KeyAction::Consumer(0x00E9).to_string(), "Volume Up");
        assert_eq!(KeyAction::Consumer(0x00CD).to_string(), "Play/Pause");
        assert_eq!(KeyAction::Consumer(0x0192).to_string(), "Calculator");
        assert_eq!(KeyAction::Consumer(0x00B5).to_string(), "Next Track");
        assert_eq!(KeyAction::Consumer(0x00E2).to_string(), "Mute");
    }

    #[test]
    fn display_consumer_unknown() {
        assert_eq!(KeyAction::Consumer(0x1234).to_string(), "Consumer(0x1234)");
    }

    #[test]
    fn wire_roundtrip_consumer() {
        let a = KeyAction::Consumer(0x00E9);
        assert_eq!(a.to_config_bytes(), [3, 0, 0xE9, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);

        let b = KeyAction::Consumer(0x0192);
        assert_eq!(b.to_config_bytes(), [3, 0, 0x92, 0x01]);
        assert_eq!(KeyAction::from_config_bytes(b.to_config_bytes()), b);
    }

    // --- LedControl tests ---

    #[test]
    fn parse_led_control() {
        assert_eq!(
            KeyAction::from_config_bytes([13, 2, 1, 0]),
            KeyAction::LedControl { data: [2, 1, 0] }
        );
    }

    #[test]
    fn display_led_control_known() {
        assert_eq!(
            KeyAction::LedControl { data: [2, 1, 0] }.to_string(),
            "LED Brightness Up"
        );
        assert_eq!(
            KeyAction::LedControl { data: [2, 2, 0] }.to_string(),
            "LED Brightness Down"
        );
        assert_eq!(
            KeyAction::LedControl { data: [3, 1, 0] }.to_string(),
            "LED Speed Up"
        );
        assert_eq!(
            KeyAction::LedControl { data: [3, 2, 0] }.to_string(),
            "LED Speed Down"
        );
    }

    #[test]
    fn display_led_control_unknown() {
        assert_eq!(
            KeyAction::LedControl { data: [99, 1, 0] }.to_string(),
            "LedControl(99,1,0)"
        );
    }

    #[test]
    fn wire_roundtrip_led_control() {
        let a = KeyAction::LedControl { data: [2, 1, 0] };
        assert_eq!(a.to_config_bytes(), [13, 2, 1, 0]);
        assert_eq!(KeyAction::from_config_bytes(a.to_config_bytes()), a);
    }
}

#[cfg(test)]
mod display_parse_roundtrip {
    use super::*;

    /// Anything the listing prints must parse back, or a value can be read off the
    /// device and shown to the user but never written again — which is exactly the
    /// state the encoder's mode toggle was in.
    #[test]
    fn every_printed_action_parses_back() {
        let cases = [
            KeyAction::Disabled,
            KeyAction::Fn,
            KeyAction::Key(HidUsage::new(0x04)),
            KeyAction::Consumer(0x00E9),
            KeyAction::SpecialFn {
                sub: special_fn::KNOB_MODE_TOGGLE,
                b2: 0,
                b3: 0,
            },
            KeyAction::SpecialFn {
                sub: special_fn::WASD_SWAP,
                b2: 0,
                b3: 0,
            },
            KeyAction::SpecialFn {
                sub: special_fn::WIN_LOCK,
                b2: 0,
                b3: 0,
            },
            KeyAction::LedControl { data: [2, 1, 0] },
        ];
        // Every entry of both shared tables, plus the hand-picked cases above.
        let table_cases: Vec<KeyAction> = SPECIAL_FN_NAMES
            .iter()
            .map(|(sub, _)| KeyAction::SpecialFn {
                sub: *sub,
                b2: 0,
                b3: 0,
            })
            .chain(
                LED_CONTROL_NAMES
                    .iter()
                    .map(|(data, _)| KeyAction::LedControl { data: *data }),
            )
            .chain((0..=3u8).map(|b2| KeyAction::SpecialFn {
                sub: special_fn::OS_MODE,
                b2,
                b3: 0,
            }))
            .collect();

        let mut broken = Vec::new();
        for a in cases.into_iter().chain(table_cases) {
            let shown = a.to_string();
            match shown.parse::<KeyAction>() {
                Ok(back) if back == a => {}
                Ok(back) => broken.push(format!("{shown:?} -> {back:?} (wanted {a:?})")),
                Err(e) => broken.push(format!("{shown:?} does not parse: {e}")),
            }
        }
        assert!(
            broken.is_empty(),
            "not round-tripping:\n  {}",
            broken.join("\n  ")
        );
    }
}
