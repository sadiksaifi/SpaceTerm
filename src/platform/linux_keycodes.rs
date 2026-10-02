//! Physical Linux keys from the retained Ghostty XKB table (XKB code minus eight).
use crate::terminal::PhysicalKey;

pub(super) fn physical_key(scancode: u16) -> PhysicalKey {
    match scancode {
        1 => PhysicalKey::Escape,
        2 => PhysicalKey::Digit1,
        3 => PhysicalKey::Digit2,
        4 => PhysicalKey::Digit3,
        5 => PhysicalKey::Digit4,
        6 => PhysicalKey::Digit5,
        7 => PhysicalKey::Digit6,
        8 => PhysicalKey::Digit7,
        9 => PhysicalKey::Digit8,
        10 => PhysicalKey::Digit9,
        11 => PhysicalKey::Digit0,
        12 => PhysicalKey::Minus,
        13 => PhysicalKey::Equal,
        14 => PhysicalKey::Backspace,
        15 => PhysicalKey::Tab,
        16 => PhysicalKey::Q,
        17 => PhysicalKey::W,
        18 => PhysicalKey::E,
        19 => PhysicalKey::R,
        20 => PhysicalKey::T,
        21 => PhysicalKey::Y,
        22 => PhysicalKey::U,
        23 => PhysicalKey::I,
        24 => PhysicalKey::O,
        25 => PhysicalKey::P,
        26 => PhysicalKey::BracketLeft,
        27 => PhysicalKey::BracketRight,
        28 => PhysicalKey::Enter,
        29 => PhysicalKey::ControlLeft,
        30 => PhysicalKey::A,
        31 => PhysicalKey::S,
        32 => PhysicalKey::D,
        33 => PhysicalKey::F,
        34 => PhysicalKey::G,
        35 => PhysicalKey::H,
        36 => PhysicalKey::J,
        37 => PhysicalKey::K,
        38 => PhysicalKey::L,
        39 => PhysicalKey::Semicolon,
        40 => PhysicalKey::Quote,
        41 => PhysicalKey::Backquote,
        42 => PhysicalKey::ShiftLeft,
        43 => PhysicalKey::Backslash,
        44 => PhysicalKey::Z,
        45 => PhysicalKey::X,
        46 => PhysicalKey::C,
        47 => PhysicalKey::V,
        48 => PhysicalKey::B,
        49 => PhysicalKey::N,
        50 => PhysicalKey::M,
        51 => PhysicalKey::Comma,
        52 => PhysicalKey::Period,
        53 => PhysicalKey::Slash,
        54 => PhysicalKey::ShiftRight,
        55 => PhysicalKey::NumpadMultiply,
        56 => PhysicalKey::AltLeft,
        57 => PhysicalKey::Space,
        58 => PhysicalKey::CapsLock,
        59 => PhysicalKey::F1,
        60 => PhysicalKey::F2,
        61 => PhysicalKey::F3,
        62 => PhysicalKey::F4,
        63 => PhysicalKey::F5,
        64 => PhysicalKey::F6,
        65 => PhysicalKey::F7,
        66 => PhysicalKey::F8,
        67 => PhysicalKey::F9,
        68 => PhysicalKey::F10,
        69 => PhysicalKey::NumLock,
        70 => PhysicalKey::ScrollLock,
        71 => PhysicalKey::Numpad7,
        72 => PhysicalKey::Numpad8,
        73 => PhysicalKey::Numpad9,
        74 => PhysicalKey::NumpadSubtract,
        75 => PhysicalKey::Numpad4,
        76 => PhysicalKey::Numpad5,
        77 => PhysicalKey::Numpad6,
        78 => PhysicalKey::NumpadAdd,
        79 => PhysicalKey::Numpad1,
        80 => PhysicalKey::Numpad2,
        81 => PhysicalKey::Numpad3,
        82 => PhysicalKey::Numpad0,
        83 => PhysicalKey::NumpadDecimal,
        86 => PhysicalKey::IntlBackslash,
        87 => PhysicalKey::F11,
        88 => PhysicalKey::F12,
        89 => PhysicalKey::IntlRo,
        92 => PhysicalKey::Convert,
        93 => PhysicalKey::KanaMode,
        94 => PhysicalKey::NonConvert,
        96 => PhysicalKey::NumpadEnter,
        97 => PhysicalKey::ControlRight,
        98 => PhysicalKey::NumpadDivide,
        99 => PhysicalKey::PrintScreen,
        100 => PhysicalKey::AltRight,
        102 => PhysicalKey::Home,
        103 => PhysicalKey::ArrowUp,
        104 => PhysicalKey::PageUp,
        105 => PhysicalKey::ArrowLeft,
        106 => PhysicalKey::ArrowRight,
        107 => PhysicalKey::End,
        108 => PhysicalKey::ArrowDown,
        109 => PhysicalKey::PageDown,
        110 => PhysicalKey::Insert,
        111 => PhysicalKey::Delete,
        113 => PhysicalKey::AudioVolumeMute,
        114 => PhysicalKey::AudioVolumeDown,
        115 => PhysicalKey::AudioVolumeUp,
        116 => PhysicalKey::Power,
        117 => PhysicalKey::NumpadEqual,
        119 => PhysicalKey::Pause,
        121 => PhysicalKey::NumpadComma,
        124 => PhysicalKey::IntlYen,
        125 => PhysicalKey::MetaLeft,
        126 => PhysicalKey::MetaRight,
        127 => PhysicalKey::ContextMenu,
        128 => PhysicalKey::BrowserStop,
        133 => PhysicalKey::Copy,
        135 => PhysicalKey::Paste,
        137 => PhysicalKey::Cut,
        138 => PhysicalKey::Help,
        140 => PhysicalKey::LaunchApp2,
        142 => PhysicalKey::Sleep,
        143 => PhysicalKey::WakeUp,
        144 => PhysicalKey::LaunchApp1,
        155 => PhysicalKey::LaunchMail,
        156 => PhysicalKey::BrowserFavorites,
        158 => PhysicalKey::BrowserBack,
        159 => PhysicalKey::BrowserForward,
        161 => PhysicalKey::Eject,
        163 => PhysicalKey::MediaTrackNext,
        164 => PhysicalKey::MediaPlayPause,
        165 => PhysicalKey::MediaTrackPrevious,
        166 => PhysicalKey::MediaStop,
        171 => PhysicalKey::MediaSelect,
        172 => PhysicalKey::BrowserHome,
        173 => PhysicalKey::BrowserRefresh,
        179 => PhysicalKey::NumpadParenLeft,
        180 => PhysicalKey::NumpadParenRight,
        183 => PhysicalKey::F13,
        184 => PhysicalKey::F14,
        185 => PhysicalKey::F15,
        186 => PhysicalKey::F16,
        187 => PhysicalKey::F17,
        188 => PhysicalKey::F18,
        189 => PhysicalKey::F19,
        190 => PhysicalKey::F20,
        191 => PhysicalKey::F21,
        192 => PhysicalKey::F22,
        193 => PhysicalKey::F23,
        194 => PhysicalKey::F24,
        217 => PhysicalKey::BrowserSearch,
        _ => PhysicalKey::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_keys_agree_with_every_supported_retained_ghostty_xkb_entry() {
        let physical_keys = include_str!("../../third_party/libghostty-vt/src/key.rs")
            .split_once("pub enum Key {")
            .unwrap()
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        let mut checked = 0;
        for line in include_str!("../../third_party/ghostty/src/input/keycodes.zig").lines() {
            let Some(entry) = line.trim().strip_prefix(".{ 0x") else {
                continue;
            };
            let fields = entry.split(", ").collect::<Vec<_>>();
            if fields.len() != 6 {
                continue;
            }
            let xkb = u16::from_str_radix(fields[2].trim_start_matches("0x"), 16).unwrap();
            let Some(scancode) = xkb.checked_sub(8) else {
                continue;
            };
            let name = fields[5].split('"').nth(1).unwrap();
            let expected = name.strip_prefix("Key").unwrap_or(name);
            if physical_keys.contains(&format!("    {expected} = ")) {
                assert_eq!(
                    format!("{:?}", physical_key(scancode)),
                    expected,
                    "evdev {scancode}"
                );
                checked += 1;
            } else {
                assert_eq!(physical_key(scancode), PhysicalKey::Unidentified, "{name}");
            }
        }
        assert!(checked > 140, "the vendor table must be exercised");
        for scancode in [0, u16::MAX] {
            assert_eq!(physical_key(scancode), PhysicalKey::Unidentified);
        }
    }
}
