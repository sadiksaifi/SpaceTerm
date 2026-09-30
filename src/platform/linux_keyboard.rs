//! Linux terminal key input from GPUI's xkbcommon keystrokes.
use gpui::{KeyDownEvent, KeyUpEvent, ModifiersChangedEvent};

use crate::terminal::{
    GpuiTerminalKeyInputAdapter, GpuiTerminalKeyInputAdapterFactory, KeyTranslation,
    OptionAsAltPolicy, TerminalKeyInputAdapter, TerminalKeyInputAdapterFactory,
};

pub(super) struct LinuxTerminalKeyInputAdapterFactory {
    gpui: GpuiTerminalKeyInputAdapterFactory,
}

impl LinuxTerminalKeyInputAdapterFactory {
    /// Alt is always Alt on Linux: AltGr text arrives already composed by xkbcommon.
    pub(super) const fn new() -> Self {
        Self {
            gpui: GpuiTerminalKeyInputAdapterFactory::new(OptionAsAltPolicy::Both),
        }
    }
}

impl TerminalKeyInputAdapterFactory for LinuxTerminalKeyInputAdapterFactory {
    fn create(&self) -> Box<dyn TerminalKeyInputAdapter> {
        Box::new(LinuxTerminalKeyInputAdapter {
            gpui: self.gpui.adapter(),
        })
    }
}

struct LinuxTerminalKeyInputAdapter {
    gpui: GpuiTerminalKeyInputAdapter,
}

impl TerminalKeyInputAdapter for LinuxTerminalKeyInputAdapter {
    fn key_down(&mut self, event: &KeyDownEvent) -> KeyTranslation {
        consume_text_shift(self.gpui.key_down(event))
    }

    fn key_up(&mut self, event: &KeyUpEvent) -> KeyTranslation {
        consume_text_shift(self.gpui.key_up(event))
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent) -> Option<KeyTranslation> {
        self.gpui.modifiers_changed(event)
    }

    fn input_method_commit(&mut self, text: String) -> KeyTranslation {
        self.gpui.input_method_commit(text)
    }

    fn reset(&mut self) {
        self.gpui.reset();
    }
}

/// xkbcommon consumes Shift when it selects a key's shifted text, as GTK reports it.
fn consume_text_shift(translation: KeyTranslation) -> KeyTranslation {
    match translation {
        KeyTranslation::Encoded(mut input) => {
            input.consumed_modifiers.shift = input.modifiers.shift && input.text.is_some();
            KeyTranslation::Encoded(input)
        }
        translation => translation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::PhysicalKey;
    use gpui::Keystroke;

    fn press(adapter: &mut dyn TerminalKeyInputAdapter, keystroke: &str, text: Option<&str>) -> KeyTranslation {
        let mut keystroke = Keystroke::parse(keystroke).unwrap();
        keystroke.key_char = text.map(ToOwned::to_owned);
        adapter.key_down(&KeyDownEvent {
            keystroke,
            is_held: false,
            prefer_character_input: false,
        })
    }

    #[test]
    fn linux_key_input_consumes_shift_only_for_shifted_text() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let KeyTranslation::Encoded(upper) = press(adapter.as_mut(), "shift-a", Some("A")) else {
            panic!("expected an encoded key");
        };
        assert_eq!(upper.physical_key, PhysicalKey::A);
        assert!(upper.modifiers.shift && upper.consumed_modifiers.shift);

        let KeyTranslation::Encoded(control) = press(adapter.as_mut(), "ctrl-c", None) else {
            panic!("expected an encoded key");
        };
        assert!(control.modifiers.control && !control.consumed_modifiers.shift);

        let KeyTranslation::Encoded(navigation) = press(adapter.as_mut(), "shift-left", None)
        else {
            panic!("expected an encoded key");
        };
        assert!(navigation.modifiers.shift && !navigation.consumed_modifiers.shift);
    }

    #[cfg(feature = "native-tests")]
    #[test]
    fn linux_key_input_meets_the_common_adapter_contract() {
        crate::terminal::assert_common_adapter_contract(LinuxTerminalKeyInputAdapterFactory::new().create());
    }
}
