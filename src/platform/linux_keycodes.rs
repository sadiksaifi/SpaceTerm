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

/// The key Ghostty's GTK runtime reports for a key event, given GPUI's layout-resolved key name.
///
/// The key the layout produces replaces the physical key when either one is functional, so an
/// XKB remap such as `caps:escape` encodes Escape. Writing-system keys keep their physical
/// position, which Kitty reports as the base layout key. GPUI names keypad keysyms like their
/// main-block counterparts, so keypad positions stay physical.
pub(super) fn terminal_key(scancode: u16, logical_key: &str) -> PhysicalKey {
    let physical = physical_key(scancode);
    if is_keypad(physical) {
        return physical;
    }
    match layout_key(logical_key) {
        Some(layout) if !is_writing_system(physical) || !is_writing_system(layout) => layout,
        _ => physical,
    }
}

/// The keys Ghostty's GTK keyval table names, spelled as GPUI's XKB keystrokes name them.
fn layout_key(logical_key: &str) -> Option<PhysicalKey> {
    let mut characters = logical_key.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Some(match character {
            'a' => PhysicalKey::A,
            'b' => PhysicalKey::B,
            'c' => PhysicalKey::C,
            'd' => PhysicalKey::D,
            'e' => PhysicalKey::E,
            'f' => PhysicalKey::F,
            'g' => PhysicalKey::G,
            'h' => PhysicalKey::H,
            'i' => PhysicalKey::I,
            'j' => PhysicalKey::J,
            'k' => PhysicalKey::K,
            'l' => PhysicalKey::L,
            'm' => PhysicalKey::M,
            'n' => PhysicalKey::N,
            'o' => PhysicalKey::O,
            'p' => PhysicalKey::P,
            'q' => PhysicalKey::Q,
            'r' => PhysicalKey::R,
            's' => PhysicalKey::S,
            't' => PhysicalKey::T,
            'u' => PhysicalKey::U,
            'v' => PhysicalKey::V,
            'w' => PhysicalKey::W,
            'x' => PhysicalKey::X,
            'y' => PhysicalKey::Y,
            'z' => PhysicalKey::Z,
            '0' => PhysicalKey::Digit0,
            '1' => PhysicalKey::Digit1,
            '2' => PhysicalKey::Digit2,
            '3' => PhysicalKey::Digit3,
            '4' => PhysicalKey::Digit4,
            '5' => PhysicalKey::Digit5,
            '6' => PhysicalKey::Digit6,
            '7' => PhysicalKey::Digit7,
            '8' => PhysicalKey::Digit8,
            '9' => PhysicalKey::Digit9,
            ';' => PhysicalKey::Semicolon,
            '\'' => PhysicalKey::Quote,
            ',' => PhysicalKey::Comma,
            '`' => PhysicalKey::Backquote,
            '.' => PhysicalKey::Period,
            '/' => PhysicalKey::Slash,
            '-' => PhysicalKey::Minus,
            '=' => PhysicalKey::Equal,
            '[' => PhysicalKey::BracketLeft,
            ']' => PhysicalKey::BracketRight,
            '\\' => PhysicalKey::Backslash,
            _ => return None,
        });
    }
    Some(match logical_key {
        "space" => PhysicalKey::Space,
        "up" => PhysicalKey::ArrowUp,
        "down" => PhysicalKey::ArrowDown,
        "right" => PhysicalKey::ArrowRight,
        "left" => PhysicalKey::ArrowLeft,
        "home" => PhysicalKey::Home,
        "end" => PhysicalKey::End,
        "insert" => PhysicalKey::Insert,
        "delete" => PhysicalKey::Delete,
        "caps_lock" => PhysicalKey::CapsLock,
        "scroll_lock" => PhysicalKey::ScrollLock,
        "num_lock" => PhysicalKey::NumLock,
        "pageup" => PhysicalKey::PageUp,
        "pagedown" => PhysicalKey::PageDown,
        "escape" => PhysicalKey::Escape,
        "enter" => PhysicalKey::Enter,
        "tab" => PhysicalKey::Tab,
        "backspace" => PhysicalKey::Backspace,
        "print" => PhysicalKey::PrintScreen,
        "pause" => PhysicalKey::Pause,
        "f1" => PhysicalKey::F1,
        "f2" => PhysicalKey::F2,
        "f3" => PhysicalKey::F3,
        "f4" => PhysicalKey::F4,
        "f5" => PhysicalKey::F5,
        "f6" => PhysicalKey::F6,
        "f7" => PhysicalKey::F7,
        "f8" => PhysicalKey::F8,
        "f9" => PhysicalKey::F9,
        "f10" => PhysicalKey::F10,
        "f11" => PhysicalKey::F11,
        "f12" => PhysicalKey::F12,
        "f13" => PhysicalKey::F13,
        "f14" => PhysicalKey::F14,
        "f15" => PhysicalKey::F15,
        "f16" => PhysicalKey::F16,
        "f17" => PhysicalKey::F17,
        "f18" => PhysicalKey::F18,
        "f19" => PhysicalKey::F19,
        "f20" => PhysicalKey::F20,
        "f21" => PhysicalKey::F21,
        "f22" => PhysicalKey::F22,
        "f23" => PhysicalKey::F23,
        "f24" => PhysicalKey::F24,
        "f25" => PhysicalKey::F25,
        "copy" => PhysicalKey::Copy,
        "cut" => PhysicalKey::Cut,
        "paste" => PhysicalKey::Paste,
        _ => return None,
    })
}

/// Ghostty's "Writing System Keys": the layout decides what these produce.
fn is_writing_system(key: PhysicalKey) -> bool {
    matches!(
        key,
        PhysicalKey::Backquote
            | PhysicalKey::Backslash
            | PhysicalKey::BracketLeft
            | PhysicalKey::BracketRight
            | PhysicalKey::Comma
            | PhysicalKey::Digit0
            | PhysicalKey::Digit1
            | PhysicalKey::Digit2
            | PhysicalKey::Digit3
            | PhysicalKey::Digit4
            | PhysicalKey::Digit5
            | PhysicalKey::Digit6
            | PhysicalKey::Digit7
            | PhysicalKey::Digit8
            | PhysicalKey::Digit9
            | PhysicalKey::Equal
            | PhysicalKey::IntlBackslash
            | PhysicalKey::IntlRo
            | PhysicalKey::IntlYen
            | PhysicalKey::A
            | PhysicalKey::B
            | PhysicalKey::C
            | PhysicalKey::D
            | PhysicalKey::E
            | PhysicalKey::F
            | PhysicalKey::G
            | PhysicalKey::H
            | PhysicalKey::I
            | PhysicalKey::J
            | PhysicalKey::K
            | PhysicalKey::L
            | PhysicalKey::M
            | PhysicalKey::N
            | PhysicalKey::O
            | PhysicalKey::P
            | PhysicalKey::Q
            | PhysicalKey::R
            | PhysicalKey::S
            | PhysicalKey::T
            | PhysicalKey::U
            | PhysicalKey::V
            | PhysicalKey::W
            | PhysicalKey::X
            | PhysicalKey::Y
            | PhysicalKey::Z
            | PhysicalKey::Minus
            | PhysicalKey::Period
            | PhysicalKey::Quote
            | PhysicalKey::Semicolon
            | PhysicalKey::Slash
    )
}

/// The keypad positions [`physical_key`] reports.
fn is_keypad(key: PhysicalKey) -> bool {
    matches!(
        key,
        PhysicalKey::Numpad0
            | PhysicalKey::Numpad1
            | PhysicalKey::Numpad2
            | PhysicalKey::Numpad3
            | PhysicalKey::Numpad4
            | PhysicalKey::Numpad5
            | PhysicalKey::Numpad6
            | PhysicalKey::Numpad7
            | PhysicalKey::Numpad8
            | PhysicalKey::Numpad9
            | PhysicalKey::NumpadAdd
            | PhysicalKey::NumpadComma
            | PhysicalKey::NumpadDecimal
            | PhysicalKey::NumpadDivide
            | PhysicalKey::NumpadEnter
            | PhysicalKey::NumpadEqual
            | PhysicalKey::NumpadMultiply
            | PhysicalKey::NumpadParenLeft
            | PhysicalKey::NumpadParenRight
            | PhysicalKey::NumpadSubtract
    )
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

    #[test]
    fn layout_keys_replace_only_functional_and_unknown_positions() {
        // Writing-system keys keep their position whatever the layout produces.
        assert_eq!(terminal_key(21, "z"), PhysicalKey::Y);
        assert_eq!(terminal_key(16, "a"), PhysicalKey::Q);
        assert_eq!(terminal_key(16, "@"), PhysicalKey::Q);
        // A functional position or produced key follows the layout.
        assert_eq!(terminal_key(58, "escape"), PhysicalKey::Escape);
        assert_eq!(terminal_key(1, "caps_lock"), PhysicalKey::CapsLock);
        assert_eq!(terminal_key(58, "a"), PhysicalKey::A);
        assert_eq!(terminal_key(30, "backspace"), PhysicalKey::Backspace);
        assert_eq!(terminal_key(0, "z"), PhysicalKey::Z);
        assert_eq!(terminal_key(1, "escape"), PhysicalKey::Escape);
        // GPUI names keypad keysyms like the main block, so keypad positions stay physical.
        assert_eq!(terminal_key(75, "left"), PhysicalKey::Numpad4);
        assert_eq!(terminal_key(96, "enter"), PhysicalKey::NumpadEnter);
    }
}
