pub use libghostty_vt::key::Key as PhysicalKey;
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
