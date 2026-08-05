use libghostty_vt::key::Key as PhysicalKey;
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
        };

        assert_eq!(
            input.validate(),
            Err(KeyInputError::UnsupportedKey {
                native_key_code: Some(0xffff),
                logical_key: "Hyper".to_owned(),
            })
        );
    }
}
