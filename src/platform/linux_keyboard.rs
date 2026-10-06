//! Linux terminal keys preserve the XKB facts available during window input dispatch.
use gpui::{
    KeyDownEvent, KeyUpEvent, Keystroke, ModifierRole, Modifiers, ModifiersChangedEvent,
    NativeKeyEvent,
};

use crate::terminal::key_input::UnhandledKeyEvent;
use crate::terminal::{
    GpuiTerminalKeyInputAdapter, GpuiTerminalKeyInputAdapterFactory, InputModifiers, KeyAction,
    KeyInput, KeyTranslation, OptionAsAltPolicy, TerminalKeyInputAdapter,
    TerminalKeyInputAdapterFactory, TerminalKeyInputEventKind,
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
            pressed_modifiers: Vec::new(),
        })
    }
}

struct LinuxTerminalKeyInputAdapter {
    gpui: GpuiTerminalKeyInputAdapter,
    pressed_modifiers: Vec<PressedModifierKey>,
}

/// A held modifier key, by the role the active keymap gives it.
struct PressedModifierKey {
    scancode: u16,
    /// The ordinary modifier the key holds, kept only when XKB actually reported it.
    active: Modifiers,
    right: bool,
}

impl PressedModifierKey {
    fn new(scancode: u16, role: Option<ModifierRole>, reported: Modifiers) -> Self {
        let (sets, right) = match role {
            Some(ModifierRole::ShiftLeft) => (Modifiers::shift(), false),
            Some(ModifierRole::ShiftRight) => (Modifiers::shift(), true),
            Some(ModifierRole::ControlLeft) => (Modifiers::control(), false),
            Some(ModifierRole::ControlRight) => (Modifiers::control(), true),
            Some(ModifierRole::AltLeft) => (Modifiers::alt(), false),
            Some(ModifierRole::AltRight) => (Modifiers::alt(), true),
            Some(ModifierRole::PlatformLeft) => (Modifiers::super_key(), false),
            Some(ModifierRole::PlatformRight) => (Modifiers::super_key(), true),
            Some(ModifierRole::CapsLock | ModifierRole::NumLock) | None => {
                (Modifiers::none(), false)
            }
        };
        Self {
            scancode,
            active: Modifiers {
                shift: sets.shift && reported.shift,
                control: sets.control && reported.control,
                alt: sets.alt && reported.alt,
                platform: sets.platform && reported.platform,
                ..Modifiers::none()
            },
            right,
        }
    }
}

impl LinuxTerminalKeyInputAdapter {
    fn translate(
        &self,
        keystroke: &Keystroke,
        native: NativeKeyEvent,
        action: KeyAction,
        kind: TerminalKeyInputEventKind,
    ) -> KeyTranslation {
        let physical_key = match native.modifier_role {
            Some(role) => super::linux_keycodes::modifier_role_key(role),
            None => super::linux_keycodes::terminal_key(native.scancode, &keystroke.key),
        };
        let mut input = KeyInput {
            action,
            physical_key,
            native_key_code: Some(native.scancode),
            logical_key: keystroke.key.clone(),
            text: layout_text(keystroke, &native),
            unshifted_codepoint: native.unshifted,
            modifiers: modifiers(native.modifiers),
            consumed_modifiers: modifiers(native.consumed),
            // AltGr is an XKB level shift, not the Alt modifier. Option-as-Alt is a Darwin
            // encoder option and must never reinterpret the text selected by the Linux keymap.
            option_as_alt: OptionAsAltPolicy::None,
        };
        input.modifiers.caps_lock = native.caps_lock;
        input.modifiers.num_lock = native.num_lock;
        let right = |modifier: fn(&Modifiers) -> bool| {
            self.pressed_modifiers
                .iter()
                .any(|key| key.right && modifier(&key.active))
        };
        input.modifiers.shift_right = native.modifiers.shift && right(|active| active.shift);
        input.modifiers.control_right = native.modifiers.control && right(|active| active.control);
        input.modifiers.alt_right = native.modifiers.alt && right(|active| active.alt);
        input.modifiers.platform_right =
            native.modifiers.platform && right(|active| active.platform);
        if input.validate().is_ok() {
            return KeyTranslation::Encoded(input);
        }
        if action != KeyAction::Release
            && !native.modifiers.control
            && !native.modifiers.alt
            && !native.modifiers.platform
            && let Some(text) = input.text
        {
            return KeyTranslation::TextInput(text);
        }
        KeyTranslation::Unhandled(UnhandledKeyEvent {
            kind,
            action,
            native_key_code: Some(native.scancode),
        })
    }
}

/// The text of the keysym the layout selected, as Ghostty's GTK runtime supplies it.
///
/// XKB turns an ASCII keysym into its control character while Ctrl is held and GPUI then
/// drops it, but Ghostty derives Ctrl sequences and Kitty's shifted key from the keysym's text.
/// GPUI's key name keeps that keysym, lowercasing only letters, so the keysym's level restores
/// their case.
fn layout_text(keystroke: &Keystroke, native: &NativeKeyEvent) -> Option<String> {
    if let Some(text) = &keystroke.key_char {
        return (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.clone());
    }
    if !native.modifiers.control {
        return None;
    }
    let mut characters = keystroke.key.chars();
    let character = match (characters.next(), characters.next()) {
        (Some(character), None) if character.is_ascii_graphic() => character,
        _ if keystroke.key == "space" => ' ',
        _ => return None,
    };
    // Letters reach their second level through Shift or Caps Lock, but not both.
    let character = if native.modifiers.shift != native.caps_lock {
        character.to_ascii_uppercase()
    } else {
        character
    };
    Some(character.to_string())
}

fn modifiers(modifiers: Modifiers) -> InputModifiers {
    InputModifiers {
        shift: modifiers.shift,
        control: modifiers.control,
        alt: modifiers.alt,
        platform: modifiers.platform,
        ..InputModifiers::default()
    }
}

impl TerminalKeyInputAdapter for LinuxTerminalKeyInputAdapter {
    fn key_down(&mut self, event: &KeyDownEvent) -> KeyTranslation {
        consume_text_shift(self.gpui.key_down(event))
    }

    fn key_up(&mut self, event: &KeyUpEvent) -> KeyTranslation {
        consume_text_shift(self.gpui.key_up(event))
    }

    fn key_down_with_native(
        &mut self,
        event: &KeyDownEvent,
        native: Option<NativeKeyEvent>,
    ) -> KeyTranslation {
        match native {
            Some(native) => self.translate(
                &event.keystroke,
                native,
                if event.is_held {
                    KeyAction::Repeat
                } else {
                    KeyAction::Press
                },
                TerminalKeyInputEventKind::KeyDown,
            ),
            None => self.key_down(event),
        }
    }

    fn key_up_with_native(
        &mut self,
        event: &KeyUpEvent,
        native: Option<NativeKeyEvent>,
    ) -> KeyTranslation {
        match native {
            Some(native) => self.translate(
                &event.keystroke,
                native,
                KeyAction::Release,
                TerminalKeyInputEventKind::KeyUp,
            ),
            None => self.key_up(event),
        }
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent) -> Option<KeyTranslation> {
        self.pressed_modifiers
            .retain(|PressedModifierKey { active, .. }| {
                (active.shift && event.modifiers.shift)
                    || (active.control && event.modifiers.control)
                    || (active.alt && event.modifiers.alt)
                    || (active.platform && event.modifiers.platform)
            });
        self.gpui.modifiers_changed(event)
    }

    fn modifiers_changed_with_native(
        &mut self,
        event: &ModifiersChangedEvent,
        native: Option<NativeKeyEvent>,
    ) -> Option<KeyTranslation> {
        let Some(mut native) = native else {
            return self.modifiers_changed(event);
        };
        let Some((scancode, pressed)) = native.modifier_key else {
            return self.modifiers_changed(event);
        };
        self.gpui.modifiers_changed(event);
        let previous = self
            .pressed_modifiers
            .iter()
            .position(|key| key.scancode == scancode);
        if pressed {
            if previous.is_some() {
                return None;
            }
            self.pressed_modifiers.push(PressedModifierKey::new(
                scancode,
                native.modifier_role,
                native.modifiers,
            ));
        } else if let Some(previous) = previous {
            self.pressed_modifiers.remove(previous);
        }
        // XKB's predicted release clears the whole modifier bit. Another physical key may
        // still hold it until the following aggregate state arrives.
        for PressedModifierKey { active, .. } in &self.pressed_modifiers {
            native.modifiers.shift |= active.shift;
            native.modifiers.control |= active.control;
            native.modifiers.alt |= active.alt;
            native.modifiers.platform |= active.platform;
        }
        Some(self.translate(
            &Keystroke {
                key: "modifier".into(),
                key_char: None,
                modifiers: native.modifiers,
            },
            native,
            if pressed {
                KeyAction::Press
            } else {
                KeyAction::Release
            },
            TerminalKeyInputEventKind::ModifiersChanged,
        ))
    }

    fn input_method_commit(&mut self, text: String) -> KeyTranslation {
        self.gpui.input_method_commit(text)
    }

    fn reset(&mut self) {
        self.pressed_modifiers.clear();
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

    /// One modifier key transition as GPUI's XKB dispatch reports it: the modifiers include the
    /// transition, and the role is the modifier key the keymap makes the physical key act as.
    fn modifier_transition(
        adapter: &mut dyn TerminalKeyInputAdapter,
        scancode: u16,
        role: Option<ModifierRole>,
        pressed: bool,
        modifiers: Modifiers,
    ) -> KeyInput {
        let event = ModifiersChangedEvent {
            modifiers,
            capslock: gpui::Capslock::default(),
        };
        let native = NativeKeyEvent {
            scancode,
            modifiers,
            modifier_key: Some((scancode, pressed)),
            modifier_role: role,
            ..Default::default()
        };
        let Some(KeyTranslation::Encoded(input)) =
            adapter.modifiers_changed_with_native(&event, Some(native))
        else {
            panic!("expected physical modifier")
        };
        input
    }

    #[test]
    fn modifier_sides_transitions_aggregate_updates_and_reset_stay_distinct() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let left_shift = Some(ModifierRole::ShiftLeft);
        let right_shift = Some(ModifierRole::ShiftRight);
        let left = modifier_transition(adapter.as_mut(), 42, left_shift, true, Modifiers::shift());
        assert_eq!(left.physical_key, PhysicalKey::ShiftLeft);
        assert_eq!(left.action, KeyAction::Press);
        let right =
            modifier_transition(adapter.as_mut(), 54, right_shift, true, Modifiers::shift());
        assert_eq!(right.physical_key, PhysicalKey::ShiftRight);
        assert!(right.modifiers.shift_right);
        assert_eq!(
            adapter.modifiers_changed_with_native(
                &ModifiersChangedEvent {
                    modifiers: Modifiers::shift(),
                    capslock: gpui::Capslock::default(),
                },
                Some(NativeKeyEvent {
                    scancode: 54,
                    modifiers: Modifiers::shift(),
                    modifier_key: Some((54, true)),
                    modifier_role: right_shift,
                    ..Default::default()
                }),
            ),
            None
        );
        assert_eq!(
            adapter.modifiers_changed_with_native(
                &ModifiersChangedEvent {
                    modifiers: Modifiers::shift(),
                    capslock: gpui::Capslock::default()
                },
                None
            ),
            None
        );
        let released =
            modifier_transition(adapter.as_mut(), 54, right_shift, false, Modifiers::none());
        assert_eq!(released.action, KeyAction::Release);
        assert!(released.modifiers.shift);
        assert!(!released.modifiers.shift_right);
        let released =
            modifier_transition(adapter.as_mut(), 42, left_shift, false, Modifiers::none());
        assert!(!released.modifiers.shift);
        modifier_transition(adapter.as_mut(), 54, right_shift, true, Modifiers::shift());
        adapter.reset();
        let reset = modifier_transition(adapter.as_mut(), 42, left_shift, true, Modifiers::shift());
        assert!(!reset.modifiers.shift_right);
    }

    #[test]
    fn remapped_modifier_keys_report_their_modifier_role() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let control = Modifiers::control();
        // caps:ctrl_modifier: Caps Lock keeps its keysym but acts as left Control.
        let caps = modifier_transition(
            adapter.as_mut(),
            58,
            Some(ModifierRole::ControlLeft),
            true,
            control,
        );
        assert_eq!(caps.physical_key, PhysicalKey::ControlLeft);
        assert_eq!(caps.native_key_code, Some(58));
        assert!(
            caps.modifiers.control && !caps.modifiers.control_right && !caps.modifiers.caps_lock
        );
        let caps = modifier_transition(
            adapter.as_mut(),
            58,
            Some(ModifierRole::ControlLeft),
            false,
            Modifiers::none(),
        );
        assert_eq!(caps.physical_key, PhysicalKey::ControlLeft);
        assert!(!caps.modifiers.control);

        // ctrl:swap_ralt_rctl: the right Alt position holds right Control.
        let right_control = modifier_transition(
            adapter.as_mut(),
            100,
            Some(ModifierRole::ControlRight),
            true,
            control,
        );
        assert_eq!(right_control.physical_key, PhysicalKey::ControlRight);
        assert!(right_control.modifiers.control_right && !right_control.modifiers.alt_right);
        let right_alt = modifier_transition(
            adapter.as_mut(),
            97,
            Some(ModifierRole::AltRight),
            true,
            Modifiers {
                alt: true,
                ..control
            },
        );
        assert_eq!(right_alt.physical_key, PhysicalKey::AltRight);
        assert!(right_alt.modifiers.alt_right && right_alt.modifiers.control_right);
        let released = modifier_transition(
            adapter.as_mut(),
            100,
            Some(ModifierRole::ControlRight),
            false,
            Modifiers::alt(),
        );
        assert!(!released.modifiers.control_right && released.modifiers.alt_right);
    }

    #[test]
    fn modifier_keys_without_a_role_keep_their_physical_key() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        // German AltGr shifts levels without acting as Alt.
        let altgr = modifier_transition(adapter.as_mut(), 100, None, true, Modifiers::none());
        assert_eq!(altgr.physical_key, PhysicalKey::AltRight);
        assert!(!altgr.modifiers.alt && !altgr.modifiers.alt_right);
    }

    /// Kitty's report of one modifier key's press and release.
    fn kitty_modifier_report(scancode: u16, role: ModifierRole, held: Modifiers) -> Vec<u8> {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        let mut emulator =
            crate::terminal::testing::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(80, 24),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.feed(b"\x1b[>15u");
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let mut bytes = Vec::new();
        for (pressed, modifiers) in [(true, held), (false, Modifiers::none())] {
            let input =
                modifier_transition(adapter.as_mut(), scancode, Some(role), pressed, modifiers);
            bytes.extend(emulator.key(input).unwrap().bytes);
        }
        bytes
    }

    #[test]
    fn kitty_reports_remapped_modifier_keys_as_the_modifier_they_act_as() {
        let control = Modifiers::control();
        let left_control = kitty_modifier_report(29, ModifierRole::ControlLeft, control);
        assert_eq!(left_control, b"\x1b[57442;5u\x1b[57442;1:3u");
        // caps:ctrl_modifier and ctrl:swap_lalt_lctl.
        assert_eq!(
            kitty_modifier_report(58, ModifierRole::ControlLeft, control),
            left_control
        );
        assert_eq!(
            kitty_modifier_report(56, ModifierRole::ControlLeft, control),
            left_control
        );
        let left_alt = kitty_modifier_report(56, ModifierRole::AltLeft, Modifiers::alt());
        assert_eq!(left_alt, b"\x1b[57443;3u\x1b[57443;1:3u");
        assert_eq!(
            kitty_modifier_report(29, ModifierRole::AltLeft, Modifiers::alt()),
            left_alt
        );
    }

    #[test]
    fn kitty_releases_each_shift_key_as_the_side_it_pressed_with() {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        let mut emulator =
            crate::terminal::testing::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(80, 24),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.feed(b"\x1b[>15u");
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        // grp:shifts_toggle, as GPUI reports it: with right Shift held, left Shift switches
        // layouts and acts as no modifier key, and right Shift releases with its press's role.
        let right_shift = Some(ModifierRole::ShiftRight);
        let mut bytes = Vec::new();
        for (scancode, role, pressed, modifiers) in [
            (54, right_shift, true, Modifiers::shift()),
            (42, None, true, Modifiers::shift()),
            (54, right_shift, false, Modifiers::none()),
            (42, None, false, Modifiers::none()),
        ] {
            let input = modifier_transition(adapter.as_mut(), scancode, role, pressed, modifiers);
            bytes.extend(emulator.key(input).unwrap().bytes);
        }
        assert_eq!(
            bytes,
            b"\x1b[57447;2u\x1b[57441;2u\x1b[57447;1:3u\x1b[57441;1:3u"
        );
    }

    #[gpui::test]
    fn native_window_facts_reach_the_pane_owned_terminal_session(cx: &mut gpui::TestAppContext) {
        use crate::terminal::testing::{
            RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
            test_local_directory,
        };
        use crate::terminal::{TerminalSessionFactory, WorkspaceTerminalSessionFactory};
        use std::{path::PathBuf, rc::Rc};
        cx.update(crate::ui::init).unwrap();
        let records = TestTerminalSessionRecords::default();
        let factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let factory = WorkspaceTerminalSessionFactory::new_local(
            factory,
            test_local_directory(PathBuf::from("/keyboard-test")),
        );
        let prepared = factory.prepare_child_launch().unwrap();
        let (pane, cx) = cx.add_window_view(|window, cx| {
            crate::ui::TerminalPane::new_with_prepared_launch(
                factory, prepared, LinuxTerminalKeyInputAdapterFactory::new().create(),
                &crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
                crate::terminal::native_services::testing::adapters(),
                crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(), window, cx,
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        });
        cx.run_until_parked();
        cx.simulate_native_key_event(
            KeyDownEvent {
                keystroke: gpui::Keystroke {
                    modifiers: gpui::Modifiers::none(),
                    key: "z".into(),
                    key_char: Some("z".into()),
                },
                is_held: false,
                prefer_character_input: false,
            },
            gpui::NativeKeyEvent {
                scancode: 21,
                unshifted: Some('z'),
                ..Default::default()
            },
        );
        let keys = records
            .commands()
            .into_iter()
            .filter_map(|command| match command.command {
                RecordedCommand::Key(input) => Some(input),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].physical_key, PhysicalKey::Y);
        assert_eq!(keys[0].text.as_deref(), Some("z"));
        assert_eq!(keys[0].native_key_code, Some(21));
    }

    #[test]
    fn native_layout_preserves_physical_key_consumed_shift_and_repeat() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let native = gpui::NativeKeyEvent {
            scancode: 21, // The physical Y key produces Z on German keyboards.
            modifiers: gpui::Modifiers::shift(),
            consumed: gpui::Modifiers::shift(),
            unshifted: Some('z'),
            ..Default::default()
        };
        let event = KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: gpui::Modifiers::shift(),
                key: "z".into(),
                key_char: Some("Z".into()),
            },
            is_held: true,
            prefer_character_input: false,
        };
        let KeyTranslation::Encoded(input) = adapter.key_down_with_native(&event, Some(native))
        else {
            panic!("expected encoded key")
        };
        assert_eq!(input.physical_key, PhysicalKey::Y);
        assert_eq!(input.native_key_code, Some(21));
        assert_eq!(input.unshifted_codepoint, Some('z'));
        assert_eq!(input.action, crate::terminal::KeyAction::Repeat);
        assert_eq!(input.text.as_deref(), Some("Z"));
        assert!(input.modifiers.shift && input.consumed_modifiers.shift);
        let KeyTranslation::Encoded(release) = adapter.key_up_with_native(
            &KeyUpEvent {
                keystroke: event.keystroke,
            },
            Some(native),
        ) else {
            panic!("expected release")
        };
        assert_eq!(release.physical_key, PhysicalKey::Y);
        assert_eq!(release.action, crate::terminal::KeyAction::Release);
    }

    /// One native key gesture as GPUI's XKB dispatch reports it.
    struct NativeGesture {
        scancode: u16,
        key: &'static str,
        unshifted: Option<char>,
        modifiers: Modifiers,
    }

    impl NativeGesture {
        const fn new(
            scancode: u16,
            key: &'static str,
            unshifted: Option<char>,
            modifiers: Modifiers,
        ) -> Self {
            Self {
                scancode,
                key,
                unshifted,
                modifiers,
            }
        }
    }

    /// Carries a press and release without key text through the Linux adapter and Ghostty's
    /// encoder. GPUI drops the control character XKB derives for Ctrl chords.
    fn encode_gesture(enabled_modes: &[u8], gesture: &NativeGesture) -> (Vec<u8>, Vec<u8>) {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        let mut emulator =
            crate::terminal::testing::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(80, 24),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.feed(enabled_modes);
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let keystroke = Keystroke {
            // GPUI keeps Shift in a keystroke only for letters.
            modifiers: Modifiers {
                shift: gesture.modifiers.shift
                    && gesture.key.chars().count() == 1
                    && gesture.key.to_uppercase() != gesture.key,
                ..gesture.modifiers
            },
            key: gesture.key.into(),
            key_char: None,
        };
        let native = NativeKeyEvent {
            scancode: gesture.scancode,
            modifiers: gesture.modifiers,
            unshifted: gesture.unshifted,
            ..Default::default()
        };
        let mut encode = |translation| {
            let KeyTranslation::Encoded(input) = translation else {
                panic!("expected an encoded key, got {translation:?}")
            };
            emulator.key(input).unwrap().bytes
        };
        let press = encode(adapter.key_down_with_native(
            &KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            },
            Some(native),
        ));
        let release = encode(adapter.key_up_with_native(&KeyUpEvent { keystroke }, Some(native)));
        (press, release)
    }

    #[test]
    fn native_control_bracket_preserves_layout_and_negotiated_protocols() {
        let control = Modifiers::control();
        // German AltGr selects [ on physical Digit8 without setting portable Alt.
        let german_bracket = NativeGesture::new(9, "[", Some('8'), control);
        let us_bracket = NativeGesture::new(26, "[", Some('['), control);
        for gesture in [&german_bracket, &us_bracket] {
            assert_eq!(encode_gesture(&[], gesture), (b"\x1b".to_vec(), vec![]));
            assert_eq!(
                encode_gesture(b"\x1b[>4;2m", gesture),
                (b"\x1b[27;5;91~".to_vec(), vec![])
            );
        }
        assert_eq!(
            encode_gesture(b"\x1b[>15u", &german_bracket),
            (b"\x1b[56;5u".to_vec(), b"\x1b[56;5:3u".to_vec())
        );
        assert_eq!(
            encode_gesture(b"\x1b[>15u", &us_bracket),
            (b"\x1b[91;5u".to_vec(), b"\x1b[91;5:3u".to_vec())
        );
    }

    #[test]
    fn native_control_chords_encode_the_layout_key_instead_of_its_us_position() {
        let control = Modifiers::control();
        let control_shift = Modifiers {
            control: true,
            shift: true,
            ..Modifiers::none()
        };
        // German QWERTZ: the physical Y key produces z.
        let german_z = NativeGesture::new(21, "z", Some('z'), control);
        assert_eq!(encode_gesture(&[], &german_z).0, b"\x1a");
        // French AZERTY: the physical Q key produces a.
        let azerty_a = NativeGesture::new(16, "a", Some('a'), control);
        assert_eq!(encode_gesture(&[], &azerty_a).0, b"\x01");
        // US layouts keep their encoding. Ctrl+Shift+Z stays distinct from Ctrl+Z, as Ghostty
        // reports it with the shifted keysym's text.
        let us_z = NativeGesture::new(44, "z", Some('z'), control);
        assert_eq!(encode_gesture(&[], &us_z).0, b"\x1a");
        let us_shift_z = NativeGesture::new(44, "z", Some('z'), control_shift);
        assert_eq!(encode_gesture(&[], &us_shift_z).0, b"\x1b[122;6u");
        let us_bracket = NativeGesture::new(26, "[", Some('['), control);
        assert_eq!(encode_gesture(&[], &us_bracket).0, b"\x1b");
        // XKB caps:escape turns the physical Caps Lock key into Escape.
        let caps_escape = NativeGesture::new(58, "escape", Some('\u{1b}'), Modifiers::none());
        assert_eq!(encode_gesture(&[], &caps_escape).0, b"\x1b");

        // Kitty reports keep the layout key, the physical base layout key, and the event type.
        let kitty = b"\x1b[>15u";
        assert_eq!(
            encode_gesture(kitty, &german_z),
            (b"\x1b[122::121;5u".to_vec(), b"\x1b[122::121;5:3u".to_vec())
        );
        assert_eq!(
            encode_gesture(kitty, &azerty_a),
            (b"\x1b[97::113;5u".to_vec(), b"\x1b[97::113;5:3u".to_vec())
        );
        assert_eq!(
            encode_gesture(kitty, &us_shift_z),
            (b"\x1b[122:90;6u".to_vec(), b"\x1b[122:90;6:3u".to_vec())
        );
        assert_eq!(
            encode_gesture(kitty, &us_bracket),
            (b"\x1b[91;5u".to_vec(), b"\x1b[91;5:3u".to_vec())
        );
        assert_eq!(
            encode_gesture(kitty, &caps_escape),
            (b"\x1b[27u".to_vec(), b"\x1b[27;1:3u".to_vec())
        );
    }

    #[test]
    fn altgr_text_is_a_level_shift_without_terminal_meta() {
        let mut adapter = LinuxTerminalKeyInputAdapterFactory::new().create();
        let KeyTranslation::Encoded(input) = adapter.key_down_with_native(
            &KeyDownEvent {
                keystroke: gpui::Keystroke {
                    modifiers: gpui::Modifiers::none(),
                    key: "@".into(),
                    key_char: Some("@".into()),
                },
                is_held: false,
                prefer_character_input: false,
            },
            Some(gpui::NativeKeyEvent {
                scancode: 16,
                unshifted: Some('q'),
                ..Default::default()
            }),
        ) else {
            panic!("expected encoded key")
        };
        assert_eq!(input.physical_key, PhysicalKey::Q);
        assert_eq!(input.text.as_deref(), Some("@"));
        assert!(!input.modifiers.alt && !input.consumed_modifiers.alt);
    }

    fn press(
        adapter: &mut dyn TerminalKeyInputAdapter,
        keystroke: &str,
        text: Option<&str>,
    ) -> KeyTranslation {
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
        crate::terminal::assert_common_adapter_contract(
            LinuxTerminalKeyInputAdapterFactory::new().create(),
        );
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod native_compose_tests {
    use super::*;
    use crate::terminal::WorkspaceTerminalSessionFactory;
    use crate::terminal::testing::{
        RecordedCommand, TerminalEmulator, TestTerminalSessionFactory, TestTerminalSessionRecords,
        test_local_directory,
    };
    use gpui::AppContext as _;
    use std::{path::PathBuf, rc::Rc, time::Duration};

    enum DriverRequest {
        Begin,
        MoveFocus,
        Finished,
    }

    fn focus_x11_window(title: &str) {
        assert!(
            std::process::Command::new("xdotool")
                .args(["search", "--onlyvisible", "--name", title, "windowfocus"])
                .status()
                .unwrap()
                .success()
        );
    }

    fn drive_keys(
        requests: async_channel::Sender<DriverRequest>,
        ready: async_channel::Receiver<()>,
    ) {
        ready.recv_blocking().unwrap();
        let backend = std::env::var("SPACETERM_COMPOSE_BACKEND").unwrap();
        if backend == "x11" {
            focus_x11_window("^SpaceTerm Compose Primary$");
        }
        let connection = zbus::blocking::Connection::session().unwrap();
        let session = if backend == "wayland" {
            let manager = zbus::blocking::Proxy::new(
                &connection,
                "org.gnome.Mutter.RemoteDesktop",
                "/org/gnome/Mutter/RemoteDesktop",
                "org.gnome.Mutter.RemoteDesktop",
            )
            .unwrap();
            let path: zbus::zvariant::OwnedObjectPath = manager.call("CreateSession", &()).unwrap();
            let remote = zbus::blocking::Proxy::new(
                &connection,
                "org.gnome.Mutter.RemoteDesktop",
                path,
                "org.gnome.Mutter.RemoteDesktop.Session",
            )
            .unwrap();
            remote.call::<_, _, ()>("Start", &()).unwrap();
            Some(remote)
        } else {
            None
        };
        let key = |name: &str, keycode: u32, pressed: bool| {
            if let Some(remote) = &session {
                remote
                    .call::<_, _, ()>("NotifyKeyboardKeycode", &(keycode, pressed))
                    .unwrap();
            } else {
                assert!(
                    std::process::Command::new("xdotool")
                        .args([if pressed { "keydown" } else { "keyup" }, name])
                        .status()
                        .unwrap()
                        .success()
                );
            }
            std::thread::sleep(Duration::from_millis(80));
        };
        if backend == "wayland" {
            // A newly created Mutter virtual keyboard consumes its first key.
            key("Shift_L", 42, true);
            key("Shift_L", 42, false);
            key("Escape", 1, true);
            key("Escape", 1, false);
            std::thread::sleep(Duration::from_secs(1));
        }
        requests.send_blocking(DriverRequest::Begin).unwrap();
        ready.recv_blocking().unwrap();
        for move_focus in [false, true] {
            key("dead_acute", 40, true);
            key("dead_acute", 40, false);
            key("e", 18, true);
            if move_focus {
                requests.send_blocking(DriverRequest::MoveFocus).unwrap();
                ready.recv_blocking().unwrap();
            }
            key("e", 18, false);
        }
        key("x", 45, true);
        key("x", 45, false);
        requests.send_blocking(DriverRequest::Finished).unwrap();
        if let Some(remote) = session {
            remote.call::<_, _, ()>("Stop", &()).unwrap();
        }
    }

    #[test]
    #[ignore = "run mise run test:compose:linux on a private display"]
    fn linux_compose_input_native_press_commit_release_reaches_kitty_encoder() {
        let backend =
            std::env::var("SPACETERM_COMPOSE_BACKEND").expect("private display runner required");
        match backend.as_str() {
            "x11" => assert_eq!(std::env::var("DISPLAY").unwrap(), ":96"),
            "wayland" => assert_eq!(
                std::env::var("WAYLAND_DISPLAY").unwrap(),
                "spaceterm-compose-test"
            ),
            _ => panic!("unsupported test backend"),
        }
        let records = TestTerminalSessionRecords::default();
        let (requests, receiver) = async_channel::unbounded();
        let (ready, ready_receiver) = async_channel::bounded(1);
        let driver = std::thread::spawn(move || drive_keys(requests, ready_receiver));
        let completed = Rc::new(std::cell::Cell::new(false));
        let first_command = Rc::new(std::cell::Cell::new(0));
        gpui_platform::application().run({
            let records = records.clone();
            let completed = completed.clone();
            let first_command = first_command.clone();
            move |cx| {
                crate::ui::init(cx).unwrap();
                let factory = WorkspaceTerminalSessionFactory::new_local(
                    Rc::new(TestTerminalSessionFactory::new(records.clone())),
                    test_local_directory(PathBuf::from("/compose-test")),
                );
                let prepared = factory.prepare_child_launch().unwrap();
                let primary = cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let pane = cx.new(|cx| crate::ui::TerminalPane::new_with_prepared_launch(
                        factory, prepared, LinuxTerminalKeyInputAdapterFactory::new().create(),
                        &crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
                        crate::terminal::native_services::testing::adapters(),
                        crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(), window, cx,
                    ));
                    window.set_window_title("SpaceTerm Compose Primary");
                    window.activate_window();
                    pane.update(cx, |pane, cx| pane.focus(window, cx));
                    pane
                }).unwrap();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    ready.send(()).await.unwrap();
                    while let Ok(request) = receiver.recv().await {
                        match request {
                            DriverRequest::Begin => {
                                first_command.set(records.commands().len());
                                ready.send(()).await.unwrap();
                            }
                            DriverRequest::MoveFocus => {
                                let secondary = cx.update(|cx| cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                                    window.set_window_title("SpaceTerm Compose Secondary");
                                    window.activate_window();
                                    cx.new(|_| gpui::EmptyView)
                                }).unwrap());
                                cx.background_executor().timer(Duration::from_millis(200)).await;
                                if backend == "x11" {
                                    focus_x11_window("^SpaceTerm Compose Secondary$");
                                    cx.background_executor().timer(Duration::from_millis(200)).await;
                                }
                                cx.update(|cx| primary.update(cx, |pane, window, cx| {
                                    window.activate_window();
                                    pane.focus(window, cx);
                                }).unwrap());
                                if backend == "x11" {
                                    focus_x11_window("^SpaceTerm Compose Primary$");
                                }
                                cx.background_executor().timer(Duration::from_millis(200)).await;
                                cx.update(|cx| secondary.update(cx, |_, window, _| window.remove_window()).unwrap());
                                ready.send(()).await.unwrap();
                            }
                            DriverRequest::Finished => {
                                // Paint without a text responder so the native input handler releases its Pane.
                                let empty = cx.update(|cx| primary.update(cx, |_, window, cx| {
                                    window.replace_root(cx, |_, _| gpui::EmptyView);
                                    window.window_handle()
                                }).unwrap());
                                cx.background_executor().timer(Duration::from_millis(200)).await;
                                completed.set(true);
                                cx.update(|cx| empty.update(cx, |_, window, _| window.remove_window()).unwrap());
                                cx.update(|cx| cx.quit());
                                break;
                            }
                        }
                    }
                }).detach();
                cx.spawn(async |cx| {
                    cx.background_executor().timer(Duration::from_secs(15)).await;
                    cx.update(|cx| cx.quit());
                }).detach();
            }
        });
        assert!(completed.get(), "native input driver timed out");
        driver.join().unwrap();
        let geometry = crate::terminal::geometry::TerminalGeometry::from_grid(
            crate::terminal::geometry::CellGridSize::new(80, 24),
            crate::terminal::geometry::LogicalCellSize::new(10.0, 20.0),
            crate::terminal::geometry::BackingScale::ONE,
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        emulator.feed(b"\x1b[>11u");
        let mut bytes = Vec::new();
        for command in records.commands().into_iter().skip(first_command.get()) {
            if let RecordedCommand::Key(input) = command.command {
                bytes.extend(emulator.key(input).unwrap().bytes);
            }
        }
        assert_eq!(bytes, "éé\x1b[120u\x1b[120;1:3u".as_bytes());
    }
}
