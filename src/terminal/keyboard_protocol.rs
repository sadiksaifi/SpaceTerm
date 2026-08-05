use libghostty_vt::Terminal;
use libghostty_vt::key::{
    Action as GhosttyKeyAction, Encoder as KeyEncoder, Event as KeyEvent, Mods, OptionAsAlt,
};

use crate::terminal::key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy};

pub(crate) struct KeyboardProtocolEncoder {
    encoder: KeyEncoder<'static>,
    event: KeyEvent<'static>,
}

impl KeyboardProtocolEncoder {
    pub(crate) fn new() -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            encoder: KeyEncoder::new()?,
            event: KeyEvent::new()?,
        })
    }

    pub(crate) fn encode(
        &mut self,
        terminal: &Terminal<'_, '_>,
        input: &KeyInput,
        bytes: &mut Vec<u8>,
    ) -> Result<(), String> {
        input.validate().map_err(|error| error.to_string())?;
        if input
            .text
            .as_deref()
            .is_some_and(|text| text.chars().any(char::is_control))
        {
            return Err("terminal key text must not contain control characters".to_owned());
        }

        self.encoder
            .set_options_from_terminal(terminal)
            .set_macos_option_as_alt(match input.option_as_alt {
                OptionAsAltPolicy::None => OptionAsAlt::False,
                OptionAsAltPolicy::Both => OptionAsAlt::True,
                OptionAsAltPolicy::Left => OptionAsAlt::Left,
                OptionAsAltPolicy::Right => OptionAsAlt::Right,
            });
        self.event
            .set_action(match input.action {
                KeyAction::Press => GhosttyKeyAction::Press,
                KeyAction::Repeat => GhosttyKeyAction::Repeat,
                KeyAction::Release => GhosttyKeyAction::Release,
            })
            .set_key(input.physical_key)
            .set_mods(key_modifiers(input.modifiers))
            .set_consumed_mods(key_modifiers(input.consumed_modifiers))
            .set_composing(false)
            .set_utf8(input.text.clone())
            .set_unshifted_codepoint(input.unshifted_codepoint.unwrap_or('\0'));
        self.encoder
            .encode_to_vec(&self.event, bytes)
            .map_err(|error| format!("failed to encode terminal key input: {error}"))
    }
}

fn key_modifiers(modifiers: InputModifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, modifiers.shift);
    result.set(Mods::ALT, modifiers.alt);
    result.set(Mods::CTRL, modifiers.control);
    result.set(Mods::SUPER, modifiers.platform);
    result.set(Mods::CAPS_LOCK, modifiers.caps_lock);
    result.set(Mods::NUM_LOCK, modifiers.num_lock);
    result.set(Mods::SHIFT_SIDE, modifiers.shift_right);
    result.set(Mods::ALT_SIDE, modifiers.alt_right);
    result.set(Mods::CTRL_SIDE, modifiers.control_right);
    result.set(Mods::SUPER_SIDE, modifiers.platform_right);
    result
}
