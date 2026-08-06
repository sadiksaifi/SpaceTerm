use std::ffi::CStr;

use cocoa::appkit::{NSApp, NSEvent, NSEventModifierFlags, NSEventType};
use cocoa::base::{id, nil};
use cocoa::foundation::NSString;
use objc::{msg_send, sel, sel_impl};

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
    pub(crate) function: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeKeyEvent {
    pub(crate) action: KeyAction,
    pub(crate) native_key_code: u16,
    pub(crate) characters: Option<String>,
    pub(crate) characters_ignoring_modifiers: Option<String>,
    pub(crate) unmodified_characters: Option<String>,
    pub(crate) characters_without_option: Option<String>,
    pub(crate) modifiers: NativeModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeKeyEventKind {
    KeyDown,
    KeyUp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UnhandledKeyEvent {
    pub(crate) kind: NativeKeyEventKind,
    pub(crate) action: KeyAction,
    pub(crate) native_key_code: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KeyTranslation {
    Encoded(KeyInput),
    TextInput(String),
    Unhandled(UnhandledKeyEvent),
}

impl KeyTranslation {
    pub(crate) fn into_result(self) -> Result<KeyInput, KeyInputError> {
        match self {
            Self::Encoded(input) => Ok(input),
            Self::TextInput(text) => Err(KeyInputError::UnsupportedKey {
                native_key_code: None,
                logical_key: text,
            }),
            Self::Unhandled(event) => Err(KeyInputError::UnsupportedKey {
                native_key_code: Some(event.native_key_code),
                logical_key: format!("{:?}", event.kind),
            }),
        }
    }
}

impl NativeKeyEvent {
    #[allow(
        unexpected_cfgs,
        reason = "objc 0.2's msg_send macro probes its historical cargo-clippy cfg"
    )]
    pub(crate) fn current_key(action: KeyAction) -> Option<Self> {
        let event_type = match action {
            KeyAction::Press | KeyAction::Repeat => NSEventType::NSKeyDown,
            KeyAction::Release => NSEventType::NSKeyUp,
        };
        Self::current(action, event_type, true)
    }

    pub(crate) fn current_modifier() -> Option<Self> {
        Self::current(KeyAction::Press, NSEventType::NSFlagsChanged, false)
    }

    #[allow(
        unexpected_cfgs,
        reason = "objc 0.2's msg_send macro probes its historical cargo-clippy cfg"
    )]
    fn current(action: KeyAction, expected_type: NSEventType, include_text: bool) -> Option<Self> {
        // SAFETY: GPUI invokes the bridge synchronously while AppKit is dispatching
        // the corresponding NSEvent. All returned NSString values are copied before
        // returning, so no Objective-C object escapes this call.
        unsafe {
            let application = NSApp();
            if application == nil {
                return None;
            }
            let event: id = msg_send![application, currentEvent];
            if event == nil {
                return None;
            }
            if event.eventType() != expected_type {
                return None;
            }

            let raw_flags = event.modifierFlags().bits();
            let unmodified: id = if include_text {
                msg_send![event, charactersByApplyingModifiers: 0usize]
            } else {
                nil
            };
            let flags_without_option = raw_flags & !NSEventModifierFlags::NSAlternateKeyMask.bits();
            let without_option: id = if include_text {
                msg_send![event, charactersByApplyingModifiers: flags_without_option]
            } else {
                nil
            };
            let mut characters = include_text
                .then(|| ns_string(event.characters()))
                .flatten();
            if characters.as_deref().is_some_and(is_single_control_text) {
                let flags_without_control =
                    raw_flags & !NSEventModifierFlags::NSControlKeyMask.bits();
                let value: id =
                    msg_send![event, charactersByApplyingModifiers: flags_without_control];
                characters = ns_string(value);
            }
            let mut characters_without_option = ns_string(without_option);
            if characters_without_option
                .as_deref()
                .is_some_and(is_single_control_text)
            {
                let flags_without_option_or_control =
                    flags_without_option & !NSEventModifierFlags::NSControlKeyMask.bits();
                let value: id = msg_send![event, charactersByApplyingModifiers: flags_without_option_or_control];
                characters_without_option = ns_string(value);
            }
            Some(Self {
                action,
                native_key_code: event.keyCode(),
                characters,
                characters_ignoring_modifiers: include_text
                    .then(|| ns_string(event.charactersIgnoringModifiers()))
                    .flatten(),
                unmodified_characters: ns_string(unmodified),
                characters_without_option,
                modifiers: NativeModifiers::from_raw_flags(raw_flags),
            })
        }
    }
}

impl NativeModifiers {
    fn from_raw_flags(flags: u64) -> Self {
        let contains = |flag: NSEventModifierFlags| flags & flag.bits() != 0;
        Self {
            shift: contains(NSEventModifierFlags::NSShiftKeyMask),
            alt: contains(NSEventModifierFlags::NSAlternateKeyMask),
            control: contains(NSEventModifierFlags::NSControlKeyMask),
            platform: contains(NSEventModifierFlags::NSCommandKeyMask),
            caps_lock: contains(NSEventModifierFlags::NSAlphaShiftKeyMask),
            num_lock: false,
            shift_left: flags & 0x0002 != 0,
            shift_right: flags & 0x0004 != 0,
            control_left: flags & 0x0001 != 0,
            control_right: flags & 0x2000 != 0,
            alt_left: flags & 0x0020 != 0,
            alt_right: flags & 0x0040 != 0,
            platform_left: flags & 0x0008 != 0,
            platform_right: flags & 0x0010 != 0,
            function: contains(NSEventModifierFlags::NSFunctionKeyMask),
        }
    }
}

unsafe fn ns_string(value: id) -> Option<String> {
    if value == nil {
        return None;
    }
    // SAFETY: `value` is an NSString supplied by the current NSEvent, and
    // UTF8String remains valid for the duration of this synchronous copy.
    let utf8 = unsafe { value.UTF8String() };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: NSString guarantees UTF8String is NUL-terminated valid UTF-8.
    Some(
        unsafe { CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned(),
    )
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

    pub(crate) fn translate(&self, event: NativeKeyEvent) -> KeyTranslation {
        let unhandled = UnhandledKeyEvent {
            kind: match event.action {
                KeyAction::Press | KeyAction::Repeat => NativeKeyEventKind::KeyDown,
                KeyAction::Release => NativeKeyEventKind::KeyUp,
            },
            action: event.action,
            native_key_code: event.native_key_code,
        };
        let physical_key = physical_key(event.native_key_code);
        let logical_key = event
            .characters_ignoring_modifiers
            .as_deref()
            .or(event.characters.as_deref())
            .unwrap_or("")
            .to_owned();
        let modifiers = input_modifiers(event.modifiers);
        let option_is_alt = option_is_alt(self.option_as_alt, modifiers);
        let input = KeyInput {
            action: event.action,
            physical_key,
            native_key_code: Some(event.native_key_code),
            logical_key,
            text: if option_is_alt {
                event.characters_without_option
            } else {
                event.characters
            }
            .filter(|text| printable_text(text)),
            unshifted_codepoint: event
                .unmodified_characters
                .as_deref()
                .and_then(single_scalar),
            modifiers,
            consumed_modifiers: InputModifiers {
                shift: modifiers.shift,
                alt: modifiers.alt && !option_is_alt,
                shift_right: modifiers.shift_right,
                alt_right: modifiers.alt_right && !option_is_alt,
                ..InputModifiers::default()
            },
            option_as_alt: self.option_as_alt,
        };
        match input.validate() {
            Ok(()) => KeyTranslation::Encoded(input),
            Err(KeyInputError::UnsupportedKey { .. }) => match input.text {
                Some(text) => KeyTranslation::TextInput(text),
                None => KeyTranslation::Unhandled(unhandled),
            },
        }
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
            PhysicalKey::Fn => event.modifiers.function,
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
        self.translate(event).into_result().map(Some)
    }
}

fn option_is_alt(policy: OptionAsAltPolicy, modifiers: InputModifiers) -> bool {
    modifiers.alt
        && match policy {
            OptionAsAltPolicy::None => false,
            OptionAsAltPolicy::Both => true,
            OptionAsAltPolicy::Left => !modifiers.alt_right,
            OptionAsAltPolicy::Right => modifiers.alt_right,
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

fn is_single_control_text(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(char::is_control) && characters.next().is_none()
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

    fn encoded(translation: KeyTranslation) -> KeyInput {
        let KeyTranslation::Encoded(input) = translation else {
            panic!("expected encoded key input, found {translation:?}");
        };
        input
    }

    fn native(native_key_code: u16, text: &str) -> NativeKeyEvent {
        NativeKeyEvent {
            action: KeyAction::Press,
            native_key_code,
            characters: Some(text.to_owned()),
            characters_ignoring_modifiers: Some(text.to_owned()),
            unmodified_characters: Some(text.to_owned()),
            characters_without_option: Some(text.to_owned()),
            modifiers: NativeModifiers::default(),
        }
    }

    #[test]
    fn unknown_native_key_down_is_unhandled_with_privacy_safe_identity() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::None);
        let mut event = native(u16::MAX, "");
        event.characters = None;
        event.characters_ignoring_modifiers = None;
        event.unmodified_characters = None;
        event.characters_without_option = None;

        assert_eq!(
            bridge.translate(event),
            KeyTranslation::Unhandled(UnhandledKeyEvent {
                kind: NativeKeyEventKind::KeyDown,
                action: KeyAction::Press,
                native_key_code: u16::MAX,
            })
        );
    }

    #[test]
    fn unknown_native_key_down_with_printable_text_uses_text_input() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::None);

        assert_eq!(
            bridge.translate(native(u16::MAX, "界")),
            KeyTranslation::TextInput("界".to_owned())
        );
    }

    #[test]
    fn native_identity_stays_physical_when_the_active_layout_changes_text() {
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::None);

        let us = encoded(bridge.translate(native(12, "q")));
        let dvorak = encoded(bridge.translate(native(12, "'")));

        assert_eq!(
            (us.physical_key, us.logical_key.as_str()),
            (PhysicalKey::Q, "q")
        );
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
            let input = encoded(bridge.translate(native(0, "a")));
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
            characters_without_option: None,
            modifiers: NativeModifiers {
                shift: shift_left || shift_right,
                shift_left,
                shift_right,
                ..NativeModifiers::default()
            },
        };

        let events = [
            bridge
                .modifier_transition(modifier(56, true, false))
                .unwrap(),
            bridge
                .modifier_transition(modifier(60, true, true))
                .unwrap(),
            bridge
                .modifier_transition(modifier(56, false, true))
                .unwrap(),
            bridge
                .modifier_transition(modifier(60, false, false))
                .unwrap(),
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
        let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::None);
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

        let input = encoded(bridge.translate(event));
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
            characters_without_option: Some("e".to_owned()),
            modifiers: NativeModifiers {
                alt: true,
                alt_left: true,
                ..NativeModifiers::default()
            },
        };

        let input = encoded(bridge.translate(event));
        assert_eq!(input.physical_key, PhysicalKey::E);
        assert_eq!(input.logical_key, "´");
        assert_eq!(input.text, None);
        assert_eq!(input.unshifted_codepoint, Some('e'));
    }

    #[test]
    fn option_policy_selects_layout_text_and_consumption_by_side() {
        let cases = [
            (OptionAsAltPolicy::None, false, "å", true),
            (OptionAsAltPolicy::Both, false, "a", false),
            (OptionAsAltPolicy::Left, false, "a", false),
            (OptionAsAltPolicy::Right, false, "å", true),
            (OptionAsAltPolicy::None, true, "å", true),
            (OptionAsAltPolicy::Both, true, "a", false),
            (OptionAsAltPolicy::Left, true, "å", true),
            (OptionAsAltPolicy::Right, true, "a", false),
        ];

        for (policy, alt_right, expected_text, consumed_alt) in cases {
            let bridge = MacosKeyboardBridge::new(policy);
            let mut event = native(0, "å");
            event.characters_without_option = Some("a".to_owned());
            event.modifiers = NativeModifiers {
                alt: true,
                alt_left: !alt_right,
                alt_right,
                ..NativeModifiers::default()
            };

            let input = encoded(bridge.translate(event));
            assert_eq!(input.text.as_deref(), Some(expected_text), "{policy:?}");
            assert_eq!(input.consumed_modifiers.alt, consumed_alt, "{policy:?}");
        }
    }
}
