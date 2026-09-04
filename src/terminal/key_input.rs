use gpui::{Capslock, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent};

use super::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalKeyInputEventKind {
    KeyDown,
    KeyUp,
    ModifiersChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UnhandledKeyEvent {
    pub(crate) kind: TerminalKeyInputEventKind,
    pub(crate) action: KeyAction,
    pub(crate) native_key_code: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KeyTranslation {
    Encoded(KeyInput),
    TextInput(String),
    Unhandled(UnhandledKeyEvent),
}

pub(crate) trait TerminalKeyInputAdapter {
    fn key_down(&mut self, event: &KeyDownEvent) -> KeyTranslation;

    fn key_up(&mut self, event: &KeyUpEvent) -> KeyTranslation;

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent) -> Option<KeyTranslation>;

    fn input_method_commit(&mut self, text: String) -> KeyTranslation;

    fn reset(&mut self);
}

pub(crate) trait TerminalKeyInputAdapterFactory {
    fn create(&self) -> Box<dyn TerminalKeyInputAdapter>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GpuiTerminalKeyInputAdapterFactory {
    option_as_alt: OptionAsAltPolicy,
}

impl GpuiTerminalKeyInputAdapterFactory {
    pub(crate) const fn new(option_as_alt: OptionAsAltPolicy) -> Self {
        Self { option_as_alt }
    }

    pub(crate) fn adapter(&self) -> GpuiTerminalKeyInputAdapter {
        GpuiTerminalKeyInputAdapter::new(self.option_as_alt)
    }
}

impl Default for GpuiTerminalKeyInputAdapterFactory {
    fn default() -> Self {
        Self::new(OptionAsAltPolicy::default())
    }
}

impl TerminalKeyInputAdapterFactory for GpuiTerminalKeyInputAdapterFactory {
    fn create(&self) -> Box<dyn TerminalKeyInputAdapter> {
        Box::new(self.adapter())
    }
}

#[derive(Debug)]
pub(crate) struct GpuiTerminalKeyInputAdapter {
    option_as_alt: OptionAsAltPolicy,
    modifiers: Modifiers,
    caps_lock: Capslock,
}

impl GpuiTerminalKeyInputAdapter {
    pub(crate) const fn new(option_as_alt: OptionAsAltPolicy) -> Self {
        Self {
            option_as_alt,
            modifiers: Modifiers {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                function: false,
            },
            caps_lock: Capslock { on: false },
        }
    }

    fn translate_keystroke(&mut self, keystroke: &Keystroke, action: KeyAction) -> KeyTranslation {
        self.modifiers = keystroke.modifiers;
        let physical_key = physical_key(&keystroke.key);
        let text = keystroke
            .key_char
            .clone()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
        let unshifted_codepoint = single_char(&keystroke.key).map(unshifted_character);
        let modifiers = gpui_input_modifiers(self.modifiers, self.caps_lock);
        let input = KeyInput {
            action,
            physical_key,
            native_key_code: None,
            logical_key: keystroke.key.clone(),
            text,
            unshifted_codepoint,
            modifiers,
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: self.option_as_alt,
        };
        let allows_text_input = !modifiers.control && !modifiers.platform && !modifiers.alt;
        match input.validate() {
            Ok(()) => KeyTranslation::Encoded(input),
            Err(_) => match input.text {
                Some(text) if action != KeyAction::Release && allows_text_input => {
                    KeyTranslation::TextInput(text)
                }
                _ => KeyTranslation::Unhandled(UnhandledKeyEvent {
                    kind: match action {
                        KeyAction::Press | KeyAction::Repeat => TerminalKeyInputEventKind::KeyDown,
                        KeyAction::Release => TerminalKeyInputEventKind::KeyUp,
                    },
                    action,
                    native_key_code: None,
                }),
            },
        }
    }
}

impl TerminalKeyInputAdapter for GpuiTerminalKeyInputAdapter {
    fn key_down(&mut self, event: &KeyDownEvent) -> KeyTranslation {
        self.translate_keystroke(
            &event.keystroke,
            if event.is_held {
                KeyAction::Repeat
            } else {
                KeyAction::Press
            },
        )
    }

    fn key_up(&mut self, event: &KeyUpEvent) -> KeyTranslation {
        self.translate_keystroke(&event.keystroke, KeyAction::Release)
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent) -> Option<KeyTranslation> {
        self.modifiers = event.modifiers;
        self.caps_lock = event.capslock;
        None
    }

    fn input_method_commit(&mut self, text: String) -> KeyTranslation {
        KeyTranslation::Encoded(KeyInput::input_method_commit(text))
    }

    fn reset(&mut self) {
        self.modifiers = Modifiers::default();
        self.caps_lock = Capslock::default();
    }
}

fn gpui_input_modifiers(modifiers: Modifiers, caps_lock: Capslock) -> InputModifiers {
    InputModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
        platform: modifiers.platform,
        caps_lock: caps_lock.on,
        ..InputModifiers::default()
    }
}

fn physical_key(key: &str) -> PhysicalKey {
    match key {
        "enter" => PhysicalKey::Enter,
        "backspace" => PhysicalKey::Backspace,
        "tab" => PhysicalKey::Tab,
        "escape" => PhysicalKey::Escape,
        "up" => PhysicalKey::ArrowUp,
        "down" => PhysicalKey::ArrowDown,
        "right" => PhysicalKey::ArrowRight,
        "left" => PhysicalKey::ArrowLeft,
        "home" => PhysicalKey::Home,
        "end" => PhysicalKey::End,
        "pageup" => PhysicalKey::PageUp,
        "pagedown" => PhysicalKey::PageDown,
        "insert" => PhysicalKey::Insert,
        "delete" => PhysicalKey::Delete,
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
        "space" | " " => PhysicalKey::Space,
        value => single_char(value)
            .map(physical_character_key)
            .unwrap_or(PhysicalKey::Unidentified),
    }
}

fn physical_character_key(character: char) -> PhysicalKey {
    match unshifted_character(character) {
        '`' => PhysicalKey::Backquote,
        '\\' => PhysicalKey::Backslash,
        '[' => PhysicalKey::BracketLeft,
        ']' => PhysicalKey::BracketRight,
        ',' => PhysicalKey::Comma,
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
        '=' => PhysicalKey::Equal,
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
        '-' => PhysicalKey::Minus,
        '.' => PhysicalKey::Period,
        '\'' => PhysicalKey::Quote,
        ';' => PhysicalKey::Semicolon,
        '/' => PhysicalKey::Slash,
        ' ' => PhysicalKey::Space,
        _ => PhysicalKey::Unidentified,
    }
}

fn unshifted_character(character: char) -> char {
    match character {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        character if character.is_ascii_uppercase() => character.to_ascii_lowercase(),
        character => character,
    }
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

#[cfg(test)]
pub(crate) fn assert_common_adapter_contract(mut adapter: Box<dyn TerminalKeyInputAdapter>) {
    let modifier_translation = adapter.modifiers_changed(&ModifiersChangedEvent {
        modifiers: Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        },
        capslock: Capslock { on: true },
    });
    let modifiers = Modifiers {
        control: true,
        alt: true,
        ..Modifiers::default()
    };
    let press = adapter.key_down(&KeyDownEvent {
        keystroke: Keystroke {
            key: "c".to_owned(),
            key_char: Some("c".to_owned()),
            modifiers,
        },
        is_held: false,
    });
    let repeat = adapter.key_down(&KeyDownEvent {
        keystroke: Keystroke {
            key: "up".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        },
        is_held: true,
    });
    let release = adapter.key_up(&KeyUpEvent {
        keystroke: Keystroke {
            key: "up".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        },
    });
    let text = adapter.key_down(&KeyDownEvent {
        keystroke: Keystroke {
            key: "hyper".to_owned(),
            key_char: Some("界".to_owned()),
            modifiers: Modifiers::default(),
        },
        is_held: false,
    });
    let unsupported = adapter.key_down(&KeyDownEvent {
        keystroke: Keystroke {
            key: "hyper".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        },
        is_held: false,
    });
    let input_method_commit = adapter.input_method_commit("日本語".to_owned());

    assert_eq!(modifier_translation, None);
    assert!(matches!(
        press,
        KeyTranslation::Encoded(KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::C,
            modifiers: InputModifiers {
                control: true,
                alt: true,
                caps_lock: true,
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        repeat,
        KeyTranslation::Encoded(KeyInput {
            action: KeyAction::Repeat,
            physical_key: PhysicalKey::ArrowUp,
            ..
        })
    ));
    assert!(matches!(
        release,
        KeyTranslation::Encoded(KeyInput {
            action: KeyAction::Release,
            physical_key: PhysicalKey::ArrowUp,
            ..
        })
    ));
    assert_eq!(text, KeyTranslation::TextInput("界".to_owned()));
    assert_eq!(
        unsupported,
        KeyTranslation::Unhandled(UnhandledKeyEvent {
            kind: TerminalKeyInputEventKind::KeyDown,
            action: KeyAction::Press,
            native_key_code: None,
        })
    );
    assert!(matches!(
        input_method_commit,
        KeyTranslation::Encoded(input) if input.is_input_method_commit()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_gpui_adapter_satisfies_the_shared_contract() {
        assert_common_adapter_contract(Box::new(GpuiTerminalKeyInputAdapter::new(
            OptionAsAltPolicy::default(),
        )));
    }

    #[test]
    fn modifier_events_supply_aggregate_and_caps_lock_state_without_guessing_a_physical_key() {
        let mut adapter = GpuiTerminalKeyInputAdapter::new(OptionAsAltPolicy::default());
        let translation = adapter.modifiers_changed(&ModifiersChangedEvent {
            modifiers: Modifiers {
                shift: true,
                platform: true,
                ..Modifiers::default()
            },
            capslock: Capslock { on: true },
        });
        let key = adapter.key_down(&KeyDownEvent {
            keystroke: Keystroke {
                key: "a".to_owned(),
                key_char: Some("A".to_owned()),
                modifiers: Modifiers {
                    shift: true,
                    platform: true,
                    ..Modifiers::default()
                },
            },
            is_held: false,
        });

        assert_eq!(translation, None);
        assert!(matches!(
            key,
            KeyTranslation::Encoded(KeyInput {
                modifiers: InputModifiers {
                    shift: true,
                    platform: true,
                    caps_lock: true,
                    ..
                },
                ..
            })
        ));
    }
}
