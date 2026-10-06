use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyBinding, Modifiers, ParentElement as _, Render, SharedString, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px, rgba,
};

use super::ShortcutTone;
use crate::*;

gpui::actions!(shortcut_recorder_tests, [CloseProbe, Traverse]);

struct Root {
    recorder: Entity<ShortcutRecorder>,
    other_focus: FocusHandle,
    closes: Rc<Cell<usize>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let closes = self.closes.clone();
        div()
            .size_full()
            .on_action(move |_: &CloseProbe, _, _| closes.set(closes.get() + 1))
            .on_action(|_: &Traverse, window, cx| window.focus_next(cx))
            .child(self.recorder.clone())
            .child(div().track_focus(&self.other_focus).child("Other"))
    }
}

struct Harness<'a> {
    recorder: Entity<ShortcutRecorder>,
    other_focus: FocusHandle,
    closes: Rc<Cell<usize>>,
    events: Rc<RefCell<Vec<ShortcutRecorderEvent>>>,
    cx: &'a mut VisualTestContext,
}

fn install_theme(cx: &mut TestAppContext) {
    let text = rgba(0xffffffff);
    let surface = rgba(0x202024ff);
    let muted = rgba(0x808088ff);
    let accent = rgba(0x5599ffff);
    let paint = TextInputPaint::new(text, muted, accent, text, muted, accent);
    cx.set_global(TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
    ));
    let button_paint = ButtonPaint::new(surface, text, rgba(0x00000000));
    let buttons = ButtonVariantStyle::new(button_paint, button_paint, button_paint, button_paint);
    cx.set_global(SearchFieldTheme::new(
        FieldFrameTheme::new(surface, muted, rgba(0xdd3344ff), accent, surface, muted),
        SearchFieldPaint::new(muted, buttons, accent, buttons, buttons),
        SearchFieldMetrics::new(px(28.0)),
    ));
}

/// Opens a window with a focused recorder that refuses bare `x` and names held modifiers.
fn harness(cx: &mut TestAppContext) -> Harness<'_> {
    install_theme(cx);
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("cmd-w", CloseProbe, None),
            KeyBinding::new("tab", Traverse, None),
        ])
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let closes = Rc::new(Cell::new(0));
    let root_closes = closes.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let recorder = cx.new(|cx| {
            ShortcutRecorder::new("recorder", "New Tab", window, cx)
                .empty_label("None")
                .recording_placeholder("Type Shortcut")
                .validator(|keystroke, _| {
                    if keystroke.modifiers.modified() {
                        Ok(())
                    } else {
                        Err("Use a modifier.".into())
                    }
                })
                .modifier_formatter(|modifiers| {
                    SharedString::from(match (modifiers.platform, modifiers.shift) {
                        (true, true) => "Shift+Primary",
                        (true, false) => "Primary",
                        (false, true) => "Shift",
                        (false, false) => "Other",
                    })
                })
                .debug_selector("recorder")
        });
        cx.subscribe(&recorder, move |_, _, event: &ShortcutRecorderEvent, _| {
            recorded.borrow_mut().push(event.clone());
        })
        .detach();
        Root {
            recorder,
            other_focus: cx.focus_handle().tab_stop(true),
            closes: root_closes,
        }
    });
    let (recorder, other_focus) = root.read_with(cx, |root, _| {
        (root.recorder.clone(), root.other_focus.clone())
    });
    cx.update(|window, cx| {
        window.activate_window();
        recorder.read(cx).focus_handle().focus(window, cx);
    });
    cx.run_until_parked();
    Harness {
        recorder,
        other_focus,
        closes,
        events,
        cx,
    }
}

impl Harness<'_> {
    fn start(&mut self) {
        self.recorder.update_in(self.cx, |recorder, window, cx| {
            recorder.start_recording(window, cx)
        });
        self.cx.run_until_parked();
    }

    fn recording(&mut self) -> bool {
        self.recorder
            .read_with(self.cx, |recorder, _| recorder.recording.is_some())
    }

    fn presentation(&mut self) -> (SharedString, ShortcutTone) {
        self.recorder
            .read_with(self.cx, |recorder, _| recorder.presentation())
    }

    /// Lets go of `source`'s key, as the platform reports it.
    fn release(&mut self, source: &str) {
        self.cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(source).unwrap(),
        });
    }

    fn take_events(&mut self) -> Vec<ShortcutRecorderEvent> {
        self.events.borrow_mut().drain(..).collect()
    }
}

fn recorded(source: &str) -> ShortcutRecorderEvent {
    ShortcutRecorderEvent::Recorded(gpui::Keystroke::parse(source).unwrap())
}

#[gpui::test]
fn clicking_starts_recording_and_a_second_click_cancels(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    let bounds = harness
        .cx
        .debug_bounds("recorder")
        .expect("recorder is painted");
    harness
        .cx
        .simulate_click(bounds.center(), Modifiers::none());
    assert!(harness.recording());
    assert_eq!(
        harness.presentation(),
        ("Type Shortcut".into(), ShortcutTone::Placeholder)
    );
    harness
        .cx
        .simulate_click(bounds.center(), Modifiers::none());
    assert!(!harness.recording());
    assert_eq!(harness.take_events(), [ShortcutRecorderEvent::Cancelled]);
}

#[gpui::test]
fn return_and_space_start_recording_from_keyboard_focus(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    for key in ["enter", "space"] {
        harness.cx.simulate_keystrokes(key);
        assert!(harness.recording(), "{key}");
        harness.cx.simulate_keystrokes("escape");
        assert!(!harness.recording(), "{key}");
    }
    assert_eq!(
        harness.take_events(),
        [
            ShortcutRecorderEvent::Cancelled,
            ShortcutRecorderEvent::Cancelled
        ]
    );
}

#[gpui::test]
fn an_accepted_chord_records_without_its_typed_character(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness.cx.simulate_keystrokes("cmd-shift-k");
    assert!(!harness.recording());
    let events = harness.take_events();
    assert_eq!(events, [recorded("cmd-shift-k")]);
    let ShortcutRecorderEvent::Recorded(keystroke) = &events[0] else {
        unreachable!()
    };
    assert_eq!(keystroke.key_char, None);
}

#[gpui::test]
fn a_refused_chord_marks_the_field_and_keeps_recording(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness.cx.simulate_keystrokes("x");
    assert!(harness.recording());
    assert_eq!(
        harness.take_events(),
        [ShortcutRecorderEvent::Rejected("Use a modifier.".into())]
    );
    harness.cx.run_until_parked();
    let bounds = harness
        .cx
        .debug_bounds("recorder")
        .expect("the rejected field renders");
    let scale = harness.cx.update(|window, _| window.scale_factor());
    assert!(harness.cx.update(|window, _| {
        window.painted_quads().iter().any(|quad| {
            quad.bounds == bounds.scale(scale)
                && quad.border_color == gpui::Hsla::from(rgba(0xdd3344ff))
        })
    }));
    harness.cx.simulate_keystrokes("cmd-x");
    assert!(!harness.recording());
    assert_eq!(harness.take_events(), [recorded("cmd-x")]);
}

#[gpui::test]
fn escape_cancels_and_delete_or_backspace_clears(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    for (key, expected) in [
        ("escape", ShortcutRecorderEvent::Cancelled),
        ("backspace", ShortcutRecorderEvent::Cleared),
        ("delete", ShortcutRecorderEvent::Cleared),
    ] {
        harness.start();
        harness.cx.simulate_keystrokes(key);
        assert!(!harness.recording(), "{key}");
        assert_eq!(harness.take_events(), [expected], "{key}");
    }
}

#[gpui::test]
fn modified_escape_and_delete_are_recorded_as_chords(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    for key in ["cmd-escape", "cmd-backspace"] {
        harness.start();
        harness.cx.simulate_keystrokes(key);
        assert_eq!(harness.take_events(), [recorded(key)], "{key}");
    }
}

#[gpui::test]
fn a_bound_chord_is_captured_instead_of_performed(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness.cx.simulate_keystrokes("cmd-w");
    assert_eq!(harness.closes.get(), 0);
    assert_eq!(harness.take_events(), [recorded("cmd-w")]);
    harness.release("cmd-w");
    harness.cx.simulate_keystrokes("cmd-w");
    assert_eq!(harness.closes.get(), 1);
    assert!(harness.take_events().is_empty());
}

#[gpui::test]
fn a_recorded_chord_held_down_does_not_repeat_into_its_action(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness.cx.simulate_keystrokes("cmd-w cmd-w cmd-w");
    assert_eq!(harness.closes.get(), 0, "auto-repeat is held back");
    assert_eq!(harness.take_events(), [recorded("cmd-w")]);

    harness.cx.simulate_modifiers_change(Modifiers::none());
    harness.cx.simulate_keystrokes("cmd-w");
    assert_eq!(
        harness.closes.get(),
        1,
        "letting Command go ends the hold even without the key's release"
    );
}

#[gpui::test]
fn another_key_after_a_recorded_chord_reaches_the_window(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness.cx.simulate_keystrokes("cmd-shift-k cmd-w");
    assert_eq!(harness.take_events(), [recorded("cmd-shift-k")]);
    assert_eq!(harness.closes.get(), 1);
}

#[gpui::test]
fn held_modifiers_are_presented_while_recording(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness
        .cx
        .simulate_modifiers_change(Modifiers::command_shift());
    assert_eq!(
        harness.presentation(),
        ("Shift+Primary".into(), ShortcutTone::Value)
    );
    harness.cx.simulate_keystrokes("shift");
    assert!(harness.recording(), "a modifier alone never completes");
    harness.cx.simulate_modifiers_change(Modifiers::none());
    assert_eq!(
        harness.presentation(),
        ("Type Shortcut".into(), ShortcutTone::Placeholder)
    );
}

#[gpui::test]
fn tab_cancels_and_leaves_traversal_to_the_window(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    assert!(harness.recording());
    harness.cx.simulate_keystrokes("tab");
    assert!(!harness.recording());
    assert_eq!(harness.take_events(), [ShortcutRecorderEvent::Cancelled]);
    let other = harness.other_focus.clone();
    assert!(harness.cx.update(|window, _| other.is_focused(window)));
    harness.cx.simulate_keystrokes("cmd-w");
    assert_eq!(harness.closes.get(), 1);
    assert!(harness.take_events().is_empty());
}

#[gpui::test]
fn losing_focus_cancels_recording(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    let other = harness.other_focus.clone();
    harness.cx.update(|window, cx| other.focus(window, cx));
    harness.cx.run_until_parked();
    assert!(!harness.recording());
    assert_eq!(harness.take_events(), [ShortcutRecorderEvent::Cancelled]);
    harness.cx.simulate_keystrokes("cmd-w");
    assert_eq!(harness.closes.get(), 1, "keys reach the window again");
}

#[gpui::test]
fn disabling_ends_recording_and_refuses_new_ones(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    harness.start();
    harness
        .recorder
        .update(harness.cx, |recorder, cx| recorder.set_disabled(true, cx));
    assert!(!harness.recording());
    assert_eq!(harness.take_events(), [ShortcutRecorderEvent::Cancelled]);
    harness.start();
    assert!(!harness.recording());
    let bounds = harness.cx.debug_bounds("recorder").unwrap();
    harness
        .cx
        .simulate_click(bounds.center(), Modifiers::none());
    assert!(!harness.recording());
}

#[gpui::test]
fn the_value_or_empty_label_is_presented_when_idle(cx: &mut TestAppContext) {
    let mut harness = harness(cx);
    assert_eq!(
        harness.presentation(),
        ("None".into(), ShortcutTone::Placeholder)
    );
    harness.recorder.update(harness.cx, |recorder, cx| {
        recorder.set_value(Some("Primary+T".into()), cx)
    });
    assert_eq!(
        harness.presentation(),
        ("Primary+T".into(), ShortcutTone::Value)
    );
}
