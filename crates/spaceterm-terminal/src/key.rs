/// Physical keys shared by client input and VT encoding.
#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, int_enum::IntEnum)]
#[non_exhaustive]
pub enum PhysicalKey {
    Unidentified = 0,
    Backquote = 1,
    Backslash = 2,
    BracketLeft = 3,
    BracketRight = 4,
    Comma = 5,
    Digit0 = 6,
    Digit1 = 7,
    Digit2 = 8,
    Digit3 = 9,
    Digit4 = 10,
    Digit5 = 11,
    Digit6 = 12,
    Digit7 = 13,
    Digit8 = 14,
    Digit9 = 15,
    Equal = 16,
    IntlBackslash = 17,
    IntlRo = 18,
    IntlYen = 19,
    A = 20,
    B = 21,
    C = 22,
    D = 23,
    E = 24,
    F = 25,
    G = 26,
    H = 27,
    I = 28,
    J = 29,
    K = 30,
    L = 31,
    M = 32,
    N = 33,
    O = 34,
    P = 35,
    Q = 36,
    R = 37,
    S = 38,
    T = 39,
    U = 40,
    V = 41,
    W = 42,
    X = 43,
    Y = 44,
    Z = 45,
    Minus = 46,
    Period = 47,
    Quote = 48,
    Semicolon = 49,
    Slash = 50,
    AltLeft = 51,
    AltRight = 52,
    Backspace = 53,
    CapsLock = 54,
    ContextMenu = 55,
    ControlLeft = 56,
    ControlRight = 57,
    Enter = 58,
    MetaLeft = 59,
    MetaRight = 60,
    ShiftLeft = 61,
    ShiftRight = 62,
    Space = 63,
    Tab = 64,
    Convert = 65,
    KanaMode = 66,
    NonConvert = 67,
    Delete = 68,
    End = 69,
    Help = 70,
    Home = 71,
    Insert = 72,
    PageDown = 73,
    PageUp = 74,
    ArrowDown = 75,
    ArrowLeft = 76,
    ArrowRight = 77,
    ArrowUp = 78,
    NumLock = 79,
    Numpad0 = 80,
    Numpad1 = 81,
    Numpad2 = 82,
    Numpad3 = 83,
    Numpad4 = 84,
    Numpad5 = 85,
    Numpad6 = 86,
    Numpad7 = 87,
    Numpad8 = 88,
    Numpad9 = 89,
    NumpadAdd = 90,
    NumpadBackspace = 91,
    NumpadClear = 92,
    NumpadClearEntry = 93,
    NumpadComma = 94,
    NumpadDecimal = 95,
    NumpadDivide = 96,
    NumpadEnter = 97,
    NumpadEqual = 98,
    NumpadMemoryAdd = 99,
    NumpadMemoryClear = 100,
    NumpadMemoryRecall = 101,
    NumpadMemoryStore = 102,
    NumpadMemorySubtract = 103,
    NumpadMultiply = 104,
    NumpadParenLeft = 105,
    NumpadParenRight = 106,
    NumpadSubtract = 107,
    NumpadSeparator = 108,
    NumpadUp = 109,
    NumpadDown = 110,
    NumpadRight = 111,
    NumpadLeft = 112,
    NumpadBegin = 113,
    NumpadHome = 114,
    NumpadEnd = 115,
    NumpadInsert = 116,
    NumpadDelete = 117,
    NumpadPageUp = 118,
    NumpadPageDown = 119,
    Escape = 120,
    F1 = 121,
    F2 = 122,
    F3 = 123,
    F4 = 124,
    F5 = 125,
    F6 = 126,
    F7 = 127,
    F8 = 128,
    F9 = 129,
    F10 = 130,
    F11 = 131,
    F12 = 132,
    F13 = 133,
    F14 = 134,
    F15 = 135,
    F16 = 136,
    F17 = 137,
    F18 = 138,
    F19 = 139,
    F20 = 140,
    F21 = 141,
    F22 = 142,
    F23 = 143,
    F24 = 144,
    F25 = 145,
    Fn = 146,
    FnLock = 147,
    PrintScreen = 148,
    ScrollLock = 149,
    Pause = 150,
    BrowserBack = 151,
    BrowserFavorites = 152,
    BrowserForward = 153,
    BrowserHome = 154,
    BrowserRefresh = 155,
    BrowserSearch = 156,
    BrowserStop = 157,
    Eject = 158,
    LaunchApp1 = 159,
    LaunchApp2 = 160,
    LaunchMail = 161,
    MediaPlayPause = 162,
    MediaSelect = 163,
    MediaStop = 164,
    MediaTrackNext = 165,
    MediaTrackPrevious = 166,
    Power = 167,
    Sleep = 168,
    AudioVolumeDown = 169,
    AudioVolumeMute = 170,
    AudioVolumeUp = 171,
    WakeUp = 172,
    Copy = 173,
    Cut = 174,
    Paste = 175,
}
#[cfg(feature = "engine")]
impl From<PhysicalKey> for libghostty_vt::key::Key {
    fn from(key: PhysicalKey) -> Self {
        match key {
            PhysicalKey::Unidentified => Self::Unidentified,
            PhysicalKey::Backquote => Self::Backquote,
            PhysicalKey::Backslash => Self::Backslash,
            PhysicalKey::BracketLeft => Self::BracketLeft,
            PhysicalKey::BracketRight => Self::BracketRight,
            PhysicalKey::Comma => Self::Comma,
            PhysicalKey::Digit0 => Self::Digit0,
            PhysicalKey::Digit1 => Self::Digit1,
            PhysicalKey::Digit2 => Self::Digit2,
            PhysicalKey::Digit3 => Self::Digit3,
            PhysicalKey::Digit4 => Self::Digit4,
            PhysicalKey::Digit5 => Self::Digit5,
            PhysicalKey::Digit6 => Self::Digit6,
            PhysicalKey::Digit7 => Self::Digit7,
            PhysicalKey::Digit8 => Self::Digit8,
            PhysicalKey::Digit9 => Self::Digit9,
            PhysicalKey::Equal => Self::Equal,
            PhysicalKey::IntlBackslash => Self::IntlBackslash,
            PhysicalKey::IntlRo => Self::IntlRo,
            PhysicalKey::IntlYen => Self::IntlYen,
            PhysicalKey::A => Self::A,
            PhysicalKey::B => Self::B,
            PhysicalKey::C => Self::C,
            PhysicalKey::D => Self::D,
            PhysicalKey::E => Self::E,
            PhysicalKey::F => Self::F,
            PhysicalKey::G => Self::G,
            PhysicalKey::H => Self::H,
            PhysicalKey::I => Self::I,
            PhysicalKey::J => Self::J,
            PhysicalKey::K => Self::K,
            PhysicalKey::L => Self::L,
            PhysicalKey::M => Self::M,
            PhysicalKey::N => Self::N,
            PhysicalKey::O => Self::O,
            PhysicalKey::P => Self::P,
            PhysicalKey::Q => Self::Q,
            PhysicalKey::R => Self::R,
            PhysicalKey::S => Self::S,
            PhysicalKey::T => Self::T,
            PhysicalKey::U => Self::U,
            PhysicalKey::V => Self::V,
            PhysicalKey::W => Self::W,
            PhysicalKey::X => Self::X,
            PhysicalKey::Y => Self::Y,
            PhysicalKey::Z => Self::Z,
            PhysicalKey::Minus => Self::Minus,
            PhysicalKey::Period => Self::Period,
            PhysicalKey::Quote => Self::Quote,
            PhysicalKey::Semicolon => Self::Semicolon,
            PhysicalKey::Slash => Self::Slash,
            PhysicalKey::AltLeft => Self::AltLeft,
            PhysicalKey::AltRight => Self::AltRight,
            PhysicalKey::Backspace => Self::Backspace,
            PhysicalKey::CapsLock => Self::CapsLock,
            PhysicalKey::ContextMenu => Self::ContextMenu,
            PhysicalKey::ControlLeft => Self::ControlLeft,
            PhysicalKey::ControlRight => Self::ControlRight,
            PhysicalKey::Enter => Self::Enter,
            PhysicalKey::MetaLeft => Self::MetaLeft,
            PhysicalKey::MetaRight => Self::MetaRight,
            PhysicalKey::ShiftLeft => Self::ShiftLeft,
            PhysicalKey::ShiftRight => Self::ShiftRight,
            PhysicalKey::Space => Self::Space,
            PhysicalKey::Tab => Self::Tab,
            PhysicalKey::Convert => Self::Convert,
            PhysicalKey::KanaMode => Self::KanaMode,
            PhysicalKey::NonConvert => Self::NonConvert,
            PhysicalKey::Delete => Self::Delete,
            PhysicalKey::End => Self::End,
            PhysicalKey::Help => Self::Help,
            PhysicalKey::Home => Self::Home,
            PhysicalKey::Insert => Self::Insert,
            PhysicalKey::PageDown => Self::PageDown,
            PhysicalKey::PageUp => Self::PageUp,
            PhysicalKey::ArrowDown => Self::ArrowDown,
            PhysicalKey::ArrowLeft => Self::ArrowLeft,
            PhysicalKey::ArrowRight => Self::ArrowRight,
            PhysicalKey::ArrowUp => Self::ArrowUp,
            PhysicalKey::NumLock => Self::NumLock,
            PhysicalKey::Numpad0 => Self::Numpad0,
            PhysicalKey::Numpad1 => Self::Numpad1,
            PhysicalKey::Numpad2 => Self::Numpad2,
            PhysicalKey::Numpad3 => Self::Numpad3,
            PhysicalKey::Numpad4 => Self::Numpad4,
            PhysicalKey::Numpad5 => Self::Numpad5,
            PhysicalKey::Numpad6 => Self::Numpad6,
            PhysicalKey::Numpad7 => Self::Numpad7,
            PhysicalKey::Numpad8 => Self::Numpad8,
            PhysicalKey::Numpad9 => Self::Numpad9,
            PhysicalKey::NumpadAdd => Self::NumpadAdd,
            PhysicalKey::NumpadBackspace => Self::NumpadBackspace,
            PhysicalKey::NumpadClear => Self::NumpadClear,
            PhysicalKey::NumpadClearEntry => Self::NumpadClearEntry,
            PhysicalKey::NumpadComma => Self::NumpadComma,
            PhysicalKey::NumpadDecimal => Self::NumpadDecimal,
            PhysicalKey::NumpadDivide => Self::NumpadDivide,
            PhysicalKey::NumpadEnter => Self::NumpadEnter,
            PhysicalKey::NumpadEqual => Self::NumpadEqual,
            PhysicalKey::NumpadMemoryAdd => Self::NumpadMemoryAdd,
            PhysicalKey::NumpadMemoryClear => Self::NumpadMemoryClear,
            PhysicalKey::NumpadMemoryRecall => Self::NumpadMemoryRecall,
            PhysicalKey::NumpadMemoryStore => Self::NumpadMemoryStore,
            PhysicalKey::NumpadMemorySubtract => Self::NumpadMemorySubtract,
            PhysicalKey::NumpadMultiply => Self::NumpadMultiply,
            PhysicalKey::NumpadParenLeft => Self::NumpadParenLeft,
            PhysicalKey::NumpadParenRight => Self::NumpadParenRight,
            PhysicalKey::NumpadSubtract => Self::NumpadSubtract,
            PhysicalKey::NumpadSeparator => Self::NumpadSeparator,
            PhysicalKey::NumpadUp => Self::NumpadUp,
            PhysicalKey::NumpadDown => Self::NumpadDown,
            PhysicalKey::NumpadRight => Self::NumpadRight,
            PhysicalKey::NumpadLeft => Self::NumpadLeft,
            PhysicalKey::NumpadBegin => Self::NumpadBegin,
            PhysicalKey::NumpadHome => Self::NumpadHome,
            PhysicalKey::NumpadEnd => Self::NumpadEnd,
            PhysicalKey::NumpadInsert => Self::NumpadInsert,
            PhysicalKey::NumpadDelete => Self::NumpadDelete,
            PhysicalKey::NumpadPageUp => Self::NumpadPageUp,
            PhysicalKey::NumpadPageDown => Self::NumpadPageDown,
            PhysicalKey::Escape => Self::Escape,
            PhysicalKey::F1 => Self::F1,
            PhysicalKey::F2 => Self::F2,
            PhysicalKey::F3 => Self::F3,
            PhysicalKey::F4 => Self::F4,
            PhysicalKey::F5 => Self::F5,
            PhysicalKey::F6 => Self::F6,
            PhysicalKey::F7 => Self::F7,
            PhysicalKey::F8 => Self::F8,
            PhysicalKey::F9 => Self::F9,
            PhysicalKey::F10 => Self::F10,
            PhysicalKey::F11 => Self::F11,
            PhysicalKey::F12 => Self::F12,
            PhysicalKey::F13 => Self::F13,
            PhysicalKey::F14 => Self::F14,
            PhysicalKey::F15 => Self::F15,
            PhysicalKey::F16 => Self::F16,
            PhysicalKey::F17 => Self::F17,
            PhysicalKey::F18 => Self::F18,
            PhysicalKey::F19 => Self::F19,
            PhysicalKey::F20 => Self::F20,
            PhysicalKey::F21 => Self::F21,
            PhysicalKey::F22 => Self::F22,
            PhysicalKey::F23 => Self::F23,
            PhysicalKey::F24 => Self::F24,
            PhysicalKey::F25 => Self::F25,
            PhysicalKey::Fn => Self::Fn,
            PhysicalKey::FnLock => Self::FnLock,
            PhysicalKey::PrintScreen => Self::PrintScreen,
            PhysicalKey::ScrollLock => Self::ScrollLock,
            PhysicalKey::Pause => Self::Pause,
            PhysicalKey::BrowserBack => Self::BrowserBack,
            PhysicalKey::BrowserFavorites => Self::BrowserFavorites,
            PhysicalKey::BrowserForward => Self::BrowserForward,
            PhysicalKey::BrowserHome => Self::BrowserHome,
            PhysicalKey::BrowserRefresh => Self::BrowserRefresh,
            PhysicalKey::BrowserSearch => Self::BrowserSearch,
            PhysicalKey::BrowserStop => Self::BrowserStop,
            PhysicalKey::Eject => Self::Eject,
            PhysicalKey::LaunchApp1 => Self::LaunchApp1,
            PhysicalKey::LaunchApp2 => Self::LaunchApp2,
            PhysicalKey::LaunchMail => Self::LaunchMail,
            PhysicalKey::MediaPlayPause => Self::MediaPlayPause,
            PhysicalKey::MediaSelect => Self::MediaSelect,
            PhysicalKey::MediaStop => Self::MediaStop,
            PhysicalKey::MediaTrackNext => Self::MediaTrackNext,
            PhysicalKey::MediaTrackPrevious => Self::MediaTrackPrevious,
            PhysicalKey::Power => Self::Power,
            PhysicalKey::Sleep => Self::Sleep,
            PhysicalKey::AudioVolumeDown => Self::AudioVolumeDown,
            PhysicalKey::AudioVolumeMute => Self::AudioVolumeMute,
            PhysicalKey::AudioVolumeUp => Self::AudioVolumeUp,
            PhysicalKey::WakeUp => Self::WakeUp,
            PhysicalKey::Copy => Self::Copy,
            PhysicalKey::Cut => Self::Cut,
            PhysicalKey::Paste => Self::Paste,
        }
    }
}

use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub platform: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub shift_right: bool,
    pub alt_right: bool,
    pub control_right: bool,
    pub platform_right: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OptionAsAltPolicy {
    None,
    #[default]
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyInput {
    pub action: KeyAction,
    pub physical_key: PhysicalKey,
    pub native_key_code: Option<u16>,
    pub logical_key: String,
    pub text: Option<String>,
    pub unshifted_codepoint: Option<char>,
    pub modifiers: InputModifiers,
    pub consumed_modifiers: InputModifiers,
    pub option_as_alt: OptionAsAltPolicy,
}

impl KeyInput {
    const TEXT_INPUT_LOGICAL_KEY: &'static str = "text-input";
    const INPUT_METHOD_LOGICAL_KEY: &'static str = "input-method-commit";

    pub fn input_method_commit(text: impl Into<String>) -> Self {
        let mut input = Self::text_input(text);
        input.logical_key = Self::INPUT_METHOD_LOGICAL_KEY.to_owned();
        input
    }

    pub fn text_input(text: impl Into<String>) -> Self {
        Self {
            action: KeyAction::Press,
            physical_key: PhysicalKey::Unidentified,
            native_key_code: None,
            logical_key: Self::TEXT_INPUT_LOGICAL_KEY.to_owned(),
            text: Some(text.into()),
            unshifted_codepoint: None,
            modifiers: InputModifiers::default(),
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::None,
        }
    }

    pub fn is_text_input(&self) -> bool {
        self.action == KeyAction::Press
            && self.physical_key == PhysicalKey::Unidentified
            && self.native_key_code.is_none()
            && self.logical_key == Self::TEXT_INPUT_LOGICAL_KEY
            && self.text.is_some()
    }

    pub fn is_input_method_commit(&self) -> bool {
        self.action == KeyAction::Press
            && self.physical_key == PhysicalKey::Unidentified
            && self.native_key_code.is_none()
            && self.logical_key == Self::INPUT_METHOD_LOGICAL_KEY
            && self.text.is_some()
    }

    pub fn is_modifier_key(&self) -> bool {
        matches!(
            self.physical_key,
            PhysicalKey::AltLeft
                | PhysicalKey::AltRight
                | PhysicalKey::CapsLock
                | PhysicalKey::ControlLeft
                | PhysicalKey::ControlRight
                | PhysicalKey::Fn
                | PhysicalKey::FnLock
                | PhysicalKey::MetaLeft
                | PhysicalKey::MetaRight
                | PhysicalKey::NumLock
                | PhysicalKey::ShiftLeft
                | PhysicalKey::ShiftRight
        )
    }

    pub fn validate(&self) -> Result<(), KeyInputError> {
        if self.physical_key == PhysicalKey::Unidentified
            && !self.is_text_input()
            && !self.is_input_method_commit()
        {
            return Err(KeyInputError::UnsupportedKey {
                native_key_code: self.native_key_code,
                logical_key: self.logical_key.clone(),
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum KeyInputError {
    #[error("unsupported terminal key {logical_key:?} with native key code {native_key_code:?}")]
    UnsupportedKey {
        native_key_code: Option<u16>,
        logical_key: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn input_method_commits_are_valid_text_without_a_physical_key() {
        let input = KeyInput::input_method_commit("日本語");

        assert_eq!(input.validate(), Ok(()));
        assert!(input.is_input_method_commit());
        assert_eq!(input.text.as_deref(), Some("日本語"));
        assert_eq!(input.physical_key, PhysicalKey::Unidentified);
    }

    #[test]
    fn identified_keys_validate_and_modifier_classification_is_explicit() {
        for (physical_key, modifier) in [
            (PhysicalKey::A, false),
            (PhysicalKey::IntlYen, false),
            (PhysicalKey::ArrowUp, false),
            (PhysicalKey::NumpadEnter, false),
            (PhysicalKey::F25, false),
            (PhysicalKey::MediaPlayPause, false),
            (PhysicalKey::Copy, false),
            (PhysicalKey::Unidentified, false),
            (PhysicalKey::AltLeft, true),
            (PhysicalKey::AltRight, true),
            (PhysicalKey::CapsLock, true),
            (PhysicalKey::ControlLeft, true),
            (PhysicalKey::ControlRight, true),
            (PhysicalKey::Fn, true),
            (PhysicalKey::FnLock, true),
            (PhysicalKey::MetaLeft, true),
            (PhysicalKey::MetaRight, true),
            (PhysicalKey::NumLock, true),
            (PhysicalKey::ShiftLeft, true),
            (PhysicalKey::ShiftRight, true),
        ] {
            for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
                let input = KeyInput {
                    action,
                    physical_key,
                    native_key_code: None,
                    logical_key: format!("{physical_key:?}"),
                    text: None,
                    unshifted_codepoint: None,
                    modifiers: InputModifiers::default(),
                    consumed_modifiers: InputModifiers::default(),
                    option_as_alt: OptionAsAltPolicy::default(),
                };
                assert_eq!(input.is_modifier_key(), modifier, "{physical_key:?}");
                if physical_key != PhysicalKey::Unidentified {
                    assert_eq!(input.validate(), Ok(()), "{physical_key:?}");
                } else {
                    assert_eq!(
                        input.validate(),
                        Err(KeyInputError::UnsupportedKey {
                            native_key_code: None,
                            logical_key: "Unidentified".into(),
                        })
                    );
                }
            }
        }
    }
}
