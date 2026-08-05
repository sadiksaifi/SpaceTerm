use crate::terminal::{
    InputModifiers, KeyAction, KeyInput, KeyInputError, OptionAsAltPolicy, PhysicalKey,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeModifiers {
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) control: bool,
    pub(crate) platform: bool,
    pub(crate) caps_lock: bool,
    pub(crate) num_lock: bool,
    pub(crate) shift_right: bool,
    pub(crate) alt_right: bool,
    pub(crate) control_right: bool,
    pub(crate) platform_right: bool,
    pub(crate) shift_left: bool,
    pub(crate) alt_left: bool,
    pub(crate) control_left: bool,
    pub(crate) platform_left: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeKeyEvent {
    pub(crate) action: KeyAction,
    pub(crate) native_key_code: u16,
    pub(crate) characters: Option<String>,
    pub(crate) characters_ignoring_modifiers: Option<String>,
    pub(crate) unmodified_characters: Option<String>,
    pub(crate) modifiers: NativeModifiers,
}

pub(crate) struct MacosKeyboardBridge {
    option_as_alt: OptionAsAltPolicy,
    pressed_modifier_key_codes: Vec<u16>,
}

impl MacosKeyboardBridge {
    pub(crate) fn new(option_as_alt: OptionAsAltPolicy) -> Self {
        Self {
            option_as_alt,
            pressed_modifier_key_codes: Vec::new(),
        }
    }

    pub(crate) fn translate(&self, event: NativeKeyEvent) -> Result<KeyInput, KeyInputError> {
        let physical_key = physical_key(event.native_key_code);
        let logical_key = event
            .characters_ignoring_modifiers
            .as_deref()
            .or(event.characters.as_deref())
            .unwrap_or("")
            .to_owned();
        let modifiers = input_modifiers(event.modifiers);
        let input = KeyInput {
            action: event.action,
            physical_key,
            native_key_code: Some(event.native_key_code),
            logical_key,
            text: event.characters.filter(|text| printable_text(text)),
            unshifted_codepoint: event
                .unmodified_characters
                .as_deref()
                .and_then(single_scalar),
            modifiers,
            consumed_modifiers: InputModifiers {
                shift: modifiers.shift,
                alt: modifiers.alt,
                shift_right: modifiers.shift_right,
                alt_right: modifiers.alt_right,
                ..InputModifiers::default()
            },
            option_as_alt: self.option_as_alt,
        };
        input.validate()?;
        Ok(input)
    }

    pub(crate) fn modifier_transition(
        &mut self,
        mut event: NativeKeyEvent,
    ) -> Result<Option<KeyInput>, KeyInputError> {
        let physical_key = physical_key(event.native_key_code);
        let active = match physical_key {
            PhysicalKey::ShiftLeft => event.modifiers.shift_left,
            PhysicalKey::ShiftRight => event.modifiers.shift_right,
            PhysicalKey::AltLeft => event.modifiers.alt_left,
            PhysicalKey::AltRight => event.modifiers.alt_right,
            PhysicalKey::ControlLeft => event.modifiers.control_left,
            PhysicalKey::ControlRight => event.modifiers.control_right,
            PhysicalKey::MetaLeft => event.modifiers.platform_left,
            PhysicalKey::MetaRight => event.modifiers.platform_right,
            PhysicalKey::CapsLock => event.modifiers.caps_lock,
            PhysicalKey::Fn => event.modifiers.num_lock,
            _ => {
                return Err(KeyInputError::UnsupportedKey {
                    native_key_code: Some(event.native_key_code),
                    logical_key: format!("{physical_key:?}"),
                });
            }
        };
        let position = self
            .pressed_modifier_key_codes
            .iter()
            .position(|key_code| *key_code == event.native_key_code);
        if active == position.is_some() {
            return Ok(None);
        }

        event.action = if active {
            self.pressed_modifier_key_codes.push(event.native_key_code);
            KeyAction::Press
        } else {
            let Some(position) = position else {
                return Ok(None);
            };
            self.pressed_modifier_key_codes.remove(position);
            KeyAction::Release
        };
        self.translate(event).map(Some)
    }
}

fn input_modifiers(modifiers: NativeModifiers) -> InputModifiers {
    InputModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
        platform: modifiers.platform,
        caps_lock: modifiers.caps_lock,
        num_lock: modifiers.num_lock,
        shift_right: modifiers.shift_right,
        alt_right: modifiers.alt_right,
        control_right: modifiers.control_right,
        platform_right: modifiers.platform_right,
    }
}

fn printable_text(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| !character.is_control() && !is_appkit_function_character(character))
}

fn single_scalar(text: &str) -> Option<char> {
    let mut characters = text.chars();
    let character = characters.next()?;
    (characters.next().is_none()
        && !character.is_control()
        && !is_appkit_function_character(character))
    .then_some(character)
}

fn is_appkit_function_character(character: char) -> bool {
    ('\u{f700}'..='\u{f8ff}').contains(&character)
}

fn physical_key(native_key_code: u16) -> PhysicalKey {
    match native_key_code {
        0 => PhysicalKey::A,
        1 => PhysicalKey::S,
        2 => PhysicalKey::D,
        3 => PhysicalKey::F,
        4 => PhysicalKey::H,
        5 => PhysicalKey::G,
        6 => PhysicalKey::Z,
        7 => PhysicalKey::X,
        8 => PhysicalKey::C,
        9 => PhysicalKey::V,
        10 => PhysicalKey::IntlBackslash,
        11 => PhysicalKey::B,
        12 => PhysicalKey::Q,
        13 => PhysicalKey::W,
        14 => PhysicalKey::E,
        15 => PhysicalKey::R,
        16 => PhysicalKey::Y,
        17 => PhysicalKey::T,
        18 => PhysicalKey::Digit1,
        19 => PhysicalKey::Digit2,
        20 => PhysicalKey::Digit3,
        21 => PhysicalKey::Digit4,
        22 => PhysicalKey::Digit6,
        23 => PhysicalKey::Digit5,
        24 => PhysicalKey::Equal,
        25 => PhysicalKey::Digit9,
        26 => PhysicalKey::Digit7,
        27 => PhysicalKey::Minus,
        28 => PhysicalKey::Digit8,
        29 => PhysicalKey::Digit0,
        30 => PhysicalKey::BracketRight,
        31 => PhysicalKey::O,
        32 => PhysicalKey::U,
        33 => PhysicalKey::BracketLeft,
        34 => PhysicalKey::I,
        35 => PhysicalKey::P,
        36 => PhysicalKey::Enter,
        37 => PhysicalKey::L,
        38 => PhysicalKey::J,
        39 => PhysicalKey::Quote,
        40 => PhysicalKey::K,
        41 => PhysicalKey::Semicolon,
        42 => PhysicalKey::Backslash,
        43 => PhysicalKey::Comma,
        44 => PhysicalKey::Slash,
        45 => PhysicalKey::N,
        46 => PhysicalKey::M,
        47 => PhysicalKey::Period,
        48 => PhysicalKey::Tab,
        49 => PhysicalKey::Space,
        50 => PhysicalKey::Backquote,
        51 => PhysicalKey::Backspace,
        53 => PhysicalKey::Escape,
        54 => PhysicalKey::MetaRight,
        55 => PhysicalKey::MetaLeft,
        56 => PhysicalKey::ShiftLeft,
        57 => PhysicalKey::CapsLock,
        58 => PhysicalKey::AltLeft,
        59 => PhysicalKey::ControlLeft,
        60 => PhysicalKey::ShiftRight,
        61 => PhysicalKey::AltRight,
        62 => PhysicalKey::ControlRight,
        63 => PhysicalKey::Fn,
        64 => PhysicalKey::F17,
        65 => PhysicalKey::NumpadDecimal,
        67 => PhysicalKey::NumpadMultiply,
        69 => PhysicalKey::NumpadAdd,
        71 => PhysicalKey::NumpadClear,
        72 => PhysicalKey::AudioVolumeUp,
        73 => PhysicalKey::AudioVolumeDown,
        74 => PhysicalKey::AudioVolumeMute,
        75 => PhysicalKey::NumpadDivide,
        76 => PhysicalKey::NumpadEnter,
        78 => PhysicalKey::NumpadSubtract,
        79 => PhysicalKey::F18,
        80 => PhysicalKey::F19,
        81 => PhysicalKey::NumpadEqual,
        82 => PhysicalKey::Numpad0,
        83 => PhysicalKey::Numpad1,
        84 => PhysicalKey::Numpad2,
        85 => PhysicalKey::Numpad3,
        86 => PhysicalKey::Numpad4,
        87 => PhysicalKey::Numpad5,
        88 => PhysicalKey::Numpad6,
        89 => PhysicalKey::Numpad7,
        91 => PhysicalKey::Numpad8,
        92 => PhysicalKey::Numpad9,
        93 => PhysicalKey::IntlYen,
        94 => PhysicalKey::IntlRo,
        95 => PhysicalKey::NumpadComma,
        96 => PhysicalKey::F5,
        97 => PhysicalKey::F6,
        98 => PhysicalKey::F7,
        99 => PhysicalKey::F3,
        100 => PhysicalKey::F8,
        101 => PhysicalKey::F9,
        103 => PhysicalKey::F11,
        105 => PhysicalKey::F13,
        106 => PhysicalKey::F16,
        107 => PhysicalKey::F14,
        109 => PhysicalKey::F10,
        111 => PhysicalKey::F12,
        113 => PhysicalKey::F15,
        114 => PhysicalKey::Help,
        115 => PhysicalKey::Home,
        116 => PhysicalKey::PageUp,
        117 => PhysicalKey::Delete,
        118 => PhysicalKey::F4,
        119 => PhysicalKey::End,
        120 => PhysicalKey::F2,
        121 => PhysicalKey::PageDown,
        122 => PhysicalKey::F1,
        123 => PhysicalKey::ArrowLeft,
        124 => PhysicalKey::ArrowRight,
        125 => PhysicalKey::ArrowDown,
        126 => PhysicalKey::ArrowUp,
        _ => PhysicalKey::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(native_key_code: u16, text: &str) -> NativeKeyEvent {
        NativeKeyEvent {
            action: KeyAction::Press,
            native_key_code,
            characters: Some(text.to_owned()),
            characters_ignoring_modifiers: Some(text.to_owned()),
            unmodified_characters: Some(text.to_owned()),
            modifiers: NativeModifiers::default(),
        }
    }

    #[test]
    fn native_identity_stays_physical_when_the_active_layout_changes_text() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::Both);

        let us = bridge.translate(native(12, "q")).unwrap();
        let dvorak = bridge.translate(native(12, "'")).unwrap();

        assert_eq!((us.physical_key, us.logical_key.as_str()), (PhysicalKey::Q, "q"));
        assert_eq!(
            (dvorak.physical_key, dvorak.logical_key.as_str()),
            (PhysicalKey::Q, "'")
        );
    }

    #[test]
    fn option_as_alt_policy_is_carried_per_event_for_each_side() {
        for policy in [
            OptionAsAltPolicy::None,
            OptionAsAltPolicy::Both,
            OptionAsAltPolicy::Left,
            OptionAsAltPolicy::Right,
        ] {
            let bridge = MacosKeyboardBridge::new(policy);
            let input = bridge.translate(native(0, "a")).unwrap();
            assert_eq!(input.option_as_alt, policy);
        }
    }

    #[test]
    fn left_and_right_modifier_transitions_remain_balanced() {
        let mut bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::Both);
        let modifier = |native_key_code, shift_left, shift_right| NativeKeyEvent {
            action: KeyAction::Press,
            native_key_code,
            characters: None,
            characters_ignoring_modifiers: None,
            unmodified_characters: None,
            modifiers: NativeModifiers {
                shift: shift_left || shift_right,
                shift_left,
                shift_right,
                ..NativeModifiers::default()
            },
        };

        let events = [
            bridge.modifier_transition(modifier(56, true, false)).unwrap(),
            bridge.modifier_transition(modifier(60, true, true)).unwrap(),
            bridge.modifier_transition(modifier(56, false, true)).unwrap(),
            bridge.modifier_transition(modifier(60, false, false)).unwrap(),
        ];
        assert_eq!(
            events.map(|event| event.map(|input| (input.physical_key, input.action))),
            [
                Some((PhysicalKey::ShiftLeft, KeyAction::Press)),
                Some((PhysicalKey::ShiftRight, KeyAction::Press)),
                Some((PhysicalKey::ShiftLeft, KeyAction::Release)),
                Some((PhysicalKey::ShiftRight, KeyAction::Release)),
            ]
        );
    }

    #[test]
    fn translation_modifiers_are_consumed_without_hiding_control_or_command() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::Both);
        let mut event = native(0, "A");
        event.modifiers = NativeModifiers {
            shift: true,
            alt: true,
            control: true,
            platform: true,
            shift_right: true,
            alt_right: true,
            control_right: true,
            platform_right: true,
            ..NativeModifiers::default()
        };

        let input = bridge.translate(event).unwrap();
        assert_eq!(
            input.consumed_modifiers,
            InputModifiers {
                shift: true,
                alt: true,
                shift_right: true,
                alt_right: true,
                ..InputModifiers::default()
            }
        );
        assert!(input.modifiers.control);
        assert!(input.modifiers.platform);
    }

    #[test]
    fn dead_key_precursors_retain_identity_without_committing_text() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::None);
        let event = NativeKeyEvent {
            action: KeyAction::Press,
            native_key_code: 14,
            characters: None,
            characters_ignoring_modifiers: Some("´".to_owned()),
            unmodified_characters: Some("e".to_owned()),
            modifiers: NativeModifiers {
                alt: true,
                alt_left: true,
                ..NativeModifiers::default()
            },
        };

        let input = bridge.translate(event).unwrap();
        assert_eq!(input.physical_key, PhysicalKey::E);
        assert_eq!(input.logical_key, "´");
        assert_eq!(input.text, None);
        assert_eq!(input.unshifted_codepoint, Some('e'));
    }
}
