pub(crate) use libghostty_vt::key::Key as PhysicalKey;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InputModifiers {
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OptionAsAltPolicy {
    None,
    #[default]
    Both,
    Left,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyInput {
    pub(crate) action: KeyAction,
    pub(crate) physical_key: PhysicalKey,
    pub(crate) native_key_code: Option<u16>,
    pub(crate) logical_key: String,
    pub(crate) text: Option<String>,
    pub(crate) unshifted_codepoint: Option<char>,
    pub(crate) modifiers: InputModifiers,
    pub(crate) consumed_modifiers: InputModifiers,
    pub(crate) option_as_alt: OptionAsAltPolicy,
}

impl KeyInput {
    pub(crate) fn validate(&self) -> Result<(), KeyInputError> {
        if self.physical_key == PhysicalKey::Unidentified {
            return Err(KeyInputError::UnsupportedKey {
                native_key_code: self.native_key_code,
                logical_key: self.logical_key.clone(),
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum KeyInputError {
    #[error(
        "unsupported terminal key {logical_key:?} with native key code {native_key_code:?}"
    )]
    UnsupportedKey {
        native_key_code: Option<u16>,
        logical_key: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const ALL_SUPPORTED_PHYSICAL_KEYS: &[PhysicalKey] = &[
        PhysicalKey::Backquote,
        PhysicalKey::Backslash,
        PhysicalKey::BracketLeft,
        PhysicalKey::BracketRight,
        PhysicalKey::Comma,
        PhysicalKey::Digit0,
        PhysicalKey::Digit1,
        PhysicalKey::Digit2,
        PhysicalKey::Digit3,
        PhysicalKey::Digit4,
        PhysicalKey::Digit5,
        PhysicalKey::Digit6,
        PhysicalKey::Digit7,
        PhysicalKey::Digit8,
        PhysicalKey::Digit9,
        PhysicalKey::Equal,
        PhysicalKey::IntlBackslash,
        PhysicalKey::IntlRo,
        PhysicalKey::IntlYen,
        PhysicalKey::A,
        PhysicalKey::B,
        PhysicalKey::C,
        PhysicalKey::D,
        PhysicalKey::E,
        PhysicalKey::F,
        PhysicalKey::G,
        PhysicalKey::H,
        PhysicalKey::I,
        PhysicalKey::J,
        PhysicalKey::K,
        PhysicalKey::L,
        PhysicalKey::M,
        PhysicalKey::N,
        PhysicalKey::O,
        PhysicalKey::P,
        PhysicalKey::Q,
        PhysicalKey::R,
        PhysicalKey::S,
        PhysicalKey::T,
        PhysicalKey::U,
        PhysicalKey::V,
        PhysicalKey::W,
        PhysicalKey::X,
        PhysicalKey::Y,
        PhysicalKey::Z,
        PhysicalKey::Minus,
        PhysicalKey::Period,
        PhysicalKey::Quote,
        PhysicalKey::Semicolon,
        PhysicalKey::Slash,
        PhysicalKey::AltLeft,
        PhysicalKey::AltRight,
        PhysicalKey::Backspace,
        PhysicalKey::CapsLock,
        PhysicalKey::ContextMenu,
        PhysicalKey::ControlLeft,
        PhysicalKey::ControlRight,
        PhysicalKey::Enter,
        PhysicalKey::MetaLeft,
        PhysicalKey::MetaRight,
        PhysicalKey::ShiftLeft,
        PhysicalKey::ShiftRight,
        PhysicalKey::Space,
        PhysicalKey::Tab,
        PhysicalKey::Convert,
        PhysicalKey::KanaMode,
        PhysicalKey::NonConvert,
        PhysicalKey::Delete,
        PhysicalKey::End,
        PhysicalKey::Help,
        PhysicalKey::Home,
        PhysicalKey::Insert,
        PhysicalKey::PageDown,
        PhysicalKey::PageUp,
        PhysicalKey::ArrowDown,
        PhysicalKey::ArrowLeft,
        PhysicalKey::ArrowRight,
        PhysicalKey::ArrowUp,
        PhysicalKey::NumLock,
        PhysicalKey::Numpad0,
        PhysicalKey::Numpad1,
        PhysicalKey::Numpad2,
        PhysicalKey::Numpad3,
        PhysicalKey::Numpad4,
        PhysicalKey::Numpad5,
        PhysicalKey::Numpad6,
        PhysicalKey::Numpad7,
        PhysicalKey::Numpad8,
        PhysicalKey::Numpad9,
        PhysicalKey::NumpadAdd,
        PhysicalKey::NumpadBackspace,
        PhysicalKey::NumpadClear,
        PhysicalKey::NumpadClearEntry,
        PhysicalKey::NumpadComma,
        PhysicalKey::NumpadDecimal,
        PhysicalKey::NumpadDivide,
        PhysicalKey::NumpadEnter,
        PhysicalKey::NumpadEqual,
        PhysicalKey::NumpadMemoryAdd,
        PhysicalKey::NumpadMemoryClear,
        PhysicalKey::NumpadMemoryRecall,
        PhysicalKey::NumpadMemoryStore,
        PhysicalKey::NumpadMemorySubtract,
        PhysicalKey::NumpadMultiply,
        PhysicalKey::NumpadParenLeft,
        PhysicalKey::NumpadParenRight,
        PhysicalKey::NumpadSubtract,
        PhysicalKey::NumpadSeparator,
        PhysicalKey::NumpadUp,
        PhysicalKey::NumpadDown,
        PhysicalKey::NumpadRight,
        PhysicalKey::NumpadLeft,
        PhysicalKey::NumpadBegin,
        PhysicalKey::NumpadHome,
        PhysicalKey::NumpadEnd,
        PhysicalKey::NumpadInsert,
        PhysicalKey::NumpadDelete,
        PhysicalKey::NumpadPageUp,
        PhysicalKey::NumpadPageDown,
        PhysicalKey::Escape,
        PhysicalKey::F1,
        PhysicalKey::F2,
        PhysicalKey::F3,
        PhysicalKey::F4,
        PhysicalKey::F5,
        PhysicalKey::F6,
        PhysicalKey::F7,
        PhysicalKey::F8,
        PhysicalKey::F9,
        PhysicalKey::F10,
        PhysicalKey::F11,
        PhysicalKey::F12,
        PhysicalKey::F13,
        PhysicalKey::F14,
        PhysicalKey::F15,
        PhysicalKey::F16,
        PhysicalKey::F17,
        PhysicalKey::F18,
        PhysicalKey::F19,
        PhysicalKey::F20,
        PhysicalKey::F21,
        PhysicalKey::F22,
        PhysicalKey::F23,
        PhysicalKey::F24,
        PhysicalKey::F25,
        PhysicalKey::Fn,
        PhysicalKey::FnLock,
        PhysicalKey::PrintScreen,
        PhysicalKey::ScrollLock,
        PhysicalKey::Pause,
        PhysicalKey::BrowserBack,
        PhysicalKey::BrowserFavorites,
        PhysicalKey::BrowserForward,
        PhysicalKey::BrowserHome,
        PhysicalKey::BrowserRefresh,
        PhysicalKey::BrowserSearch,
        PhysicalKey::BrowserStop,
        PhysicalKey::Eject,
        PhysicalKey::LaunchApp1,
        PhysicalKey::LaunchApp2,
        PhysicalKey::LaunchMail,
        PhysicalKey::MediaPlayPause,
        PhysicalKey::MediaSelect,
        PhysicalKey::MediaStop,
        PhysicalKey::MediaTrackNext,
        PhysicalKey::MediaTrackPrevious,
        PhysicalKey::Power,
        PhysicalKey::Sleep,
        PhysicalKey::AudioVolumeDown,
        PhysicalKey::AudioVolumeMute,
        PhysicalKey::AudioVolumeUp,
        PhysicalKey::WakeUp,
        PhysicalKey::Copy,
        PhysicalKey::Cut,
        PhysicalKey::Paste,
    ];

    #[test]
    fn unidentified_physical_key_should_fail_with_native_and_logical_identity() {
        let input = KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::Unidentified,
            native_key_code: Some(0xffff),
            logical_key: "Hyper".to_owned(),
            text: None,
            unshifted_codepoint: None,
            modifiers: InputModifiers::default(),
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::default(),
        };

        assert_eq!(
            input.validate(),
            Err(KeyInputError::UnsupportedKey {
                native_key_code: Some(0xffff),
                logical_key: "Hyper".to_owned(),
            })
        );
    }

    #[test]
    fn complete_physical_key_vocabulary_should_be_typed_and_unique() {
        let identities = ALL_SUPPORTED_PHYSICAL_KEYS
            .iter()
            .map(|key| *key as u32)
            .collect::<HashSet<_>>();

        assert_eq!(ALL_SUPPORTED_PHYSICAL_KEYS.len(), 175);
        assert_eq!(identities.len(), ALL_SUPPORTED_PHYSICAL_KEYS.len());
        for physical_key in ALL_SUPPORTED_PHYSICAL_KEYS {
            let input = KeyInput {
                action: KeyAction::Press,
                physical_key: *physical_key,
                native_key_code: None,
                logical_key: format!("{physical_key:?}"),
                text: None,
                unshifted_codepoint: None,
                modifiers: InputModifiers::default(),
                consumed_modifiers: InputModifiers::default(),
                option_as_alt: OptionAsAltPolicy::default(),
            };
            assert_eq!(input.validate(), Ok(()), "{physical_key:?}");
        }
    }
}
