use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext, rgba};

struct EventRoot {
    input: Entity<TextInput>,
    other_focus: FocusHandle,
    unrelated_menu: bool,
    outer_submit: Rc<Cell<usize>>,
    outer_cancel: Rc<Cell<usize>>,
}

impl Render for EventRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let submit = self.outer_submit.clone();
        let cancel = self.outer_cancel.clone();
        div()
            .size_full()
            .on_action(move |_: &Submit, _, _| submit.set(submit.get() + 1))
            .on_action(move |_: &Cancel, _, _| cancel.set(cancel.get() + 1))
            .flex()
            .flex_col()
            .child(div().h(px(32.0)).child(self.input.clone()))
            .child(div().track_focus(&self.other_focus).child("Other"))
            .when(self.unrelated_menu, |root| {
                root.child(
                    crate::menu::Menu::new(
                        "unrelated-menu",
                        "Unrelated menu",
                        vec![MenuEntry::action("Action", ())],
                    )
                    .debug_selector("unrelated-menu-target")
                    .on_activate(|_, _, _| {}),
                )
            })
    }
}
fn install_theme(cx: &mut TestAppContext) {
    let paint = TextInputPaint::new(
        rgba(0xffffffff),
        rgba(0x888888ff),
        rgba(0x3355aaff),
        rgba(0xffffffff),
        rgba(0x777777ff),
        rgba(0x555555ff),
    );
    cx.set_global(TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
    ));
    let menu_paint = crate::menu::MenuPaint::new(
        rgba(0x141415ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x606079ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0xd8647eff),
        rgba(0x252530ff),
    );
    let metrics = crate::menu::MenuMetrics::new(px(160.0), px(26.0));
    cx.set_global(crate::menu::MenuTheme::new(
        menu_paint,
        crate::menu::MenuSizes::new(metrics, metrics, metrics),
    ));
    cx.update(super::init);
    cx.update(crate::menu::init);
    cx.update(|cx| install_text_input_keybindings(cx, TextInputKeybindingProfile::MacOs));
}

fn input<'a>(
    cx: &'a mut TestAppContext,
    value: &'static str,
) -> (Entity<TextInput>, &'a mut VisualTestContext) {
    install_theme(cx);
    let (input, cx) = cx.add_window_view(move |window, cx| {
        TextInput::new("test-input", "Test input", value, window, cx).debug_selector("test-input")
    });
    cx.update(|window, cx| {
        window.activate_window();
        input.read(cx).focus_handle().focus(window);
    });
    cx.run_until_parked();
    (input, cx)
}

fn obscured_input<'a>(
    cx: &'a mut TestAppContext,
    value: &'static str,
) -> (Entity<TextInput>, &'a mut VisualTestContext) {
    install_theme(cx);
    let (input, cx) = cx.add_window_view(move |window, cx| {
        TextInput::new("test-input", "Secret", value, window, cx)
            .content_mode(TextInputContentMode::Obscured)
            .debug_selector("test-input")
    });
    cx.update(|window, cx| {
        window.activate_window();
        input.read(cx).focus_handle().focus(window);
    });
    cx.run_until_parked();
    (input, cx)
}

fn input_with_events<'a>(
    cx: &'a mut TestAppContext,
    value: &'static str,
    unrelated_menu: bool,
) -> (
    Entity<TextInput>,
    FocusHandle,
    Rc<RefCell<Vec<TextInputEvent>>>,
    &'a mut VisualTestContext,
) {
    install_theme(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded_events = events.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let input = cx.new(|cx| {
            TextInput::new("test-input", "Test input", value, window, cx)
                .debug_selector("test-input")
        });
        cx.subscribe(&input, move |_, _, event: &TextInputEvent, _| {
            recorded_events.borrow_mut().push(*event);
        })
        .detach();
        EventRoot {
            input,
            other_focus: cx.focus_handle(),
            unrelated_menu,
            outer_submit: Rc::new(Cell::new(0)),
            outer_cancel: Rc::new(Cell::new(0)),
        }
    });
    let (input, other_focus) =
        root.read_with(cx, |root, _| (root.input.clone(), root.other_focus.clone()));
    cx.update(|window, cx| {
        window.activate_window();
        input.read(cx).focus_handle().focus(window);
    });
    cx.run_until_parked();
    events.borrow_mut().clear();
    (input, other_focus, events, cx)
}

#[gpui::test]
fn default_home_and_end_should_continue_to_move_the_editor_caret(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");

    cx.simulate_keystrokes("home X end Y");
    cx.run_until_parked();

    assert!(input.read_with(cx, |input, _| input.value() == "XabcY"));
}

fn mark_text(input: &Entity<TextInput>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, text, None, window, cx);
        });
    });
}

#[gpui::test]
fn configured_larger_limit_preserves_initial_value_above_default_limit(cx: &mut TestAppContext) {
    install_theme(cx);
    let value = "é".repeat(DEFAULT_VALUE_LIMIT / 2 + 1024);
    let expected = value.clone();
    let (input, cx) = cx.add_window_view(move |window, cx| {
        TextInput::new("test-input", "Test input", value, window, cx)
            .input_length_limit(Some(128 * 1024))
    });
    cx.run_until_parked();

    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        expected
    );
}

#[gpui::test]
fn removing_default_limit_preserves_initial_value_above_default_limit(cx: &mut TestAppContext) {
    install_theme(cx);
    let value = "é".repeat(DEFAULT_VALUE_LIMIT / 2 + 1024);
    let expected = value.clone();
    let (input, cx) = cx.add_window_view(move |window, cx| {
        TextInput::new("test-input", "Test input", value, window, cx).input_length_limit(None)
    });
    cx.run_until_parked();

    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        expected
    );
}

#[test]
fn normalization_preserves_word_boundaries_for_every_line_separator() {
    let source = "a\r\nb\rc\nd\te\u{0007}f\u{2028}g\u{2029}h";
    assert_eq!(normalize_single_line(source), "a b c d e f g h");
    assert_eq!(
        normalized_single_line_len(source),
        normalize_single_line(source).len()
    );
}
#[test]
fn surrogate_collapsed_range_stays_empty() {
    let text = "A😀B";
    let range = utf16_replacement_range_to_bytes(text, 2..2);
    assert!(range.is_empty());
    assert_eq!(range.start, 1);
}
#[test]
fn surrogate_nonempty_range_expands_scalar() {
    let text = "A😀B";
    let range = utf16_replacement_range_to_bytes(text, 2..3);
    assert_eq!(&text[range], "😀");
}
#[test]
fn grapheme_editing_is_atomic() {
    let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
    assert!(buffer.delete_backward(HARD_VALUE_LIMIT));
    assert!(buffer.delete_backward(HARD_VALUE_LIMIT));
    assert_eq!(buffer.text.as_str(), "");
}
#[test]
fn transposition_swaps_complete_graphemes() {
    let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
    assert!(buffer.transpose(HARD_VALUE_LIMIT));
    assert_eq!(buffer.text.as_str(), "👩‍💻e\u{301}");
}
#[test]
fn oversized_edit_is_atomic() {
    let mut buffer = TextBuffer::new("ok".into());
    let before = buffer.snapshot();
    assert!(!buffer.replace(0..2, "x".repeat(10), EditKind::Atomic, 4));
    assert_eq!(buffer.snapshot(), before);
}
#[test]
fn history_is_bounded_across_undo_and_redo() {
    let mut buffer = TextBuffer::new(String::new());
    for _ in 0..240 {
        buffer.history.break_group();
        let end = buffer.text.len();
        assert!(buffer.replace(
            end..end,
            "x".repeat(4096),
            EditKind::Atomic,
            HARD_VALUE_LIMIT
        ));
    }
    for _ in 0..80 {
        buffer.undo();
    }
    assert!(buffer.history.undo.len() + buffer.history.redo.len() <= HISTORY_ENTRY_LIMIT);
    assert!(buffer.history.retained_bytes <= HISTORY_BYTE_LIMIT);
}
#[test]
fn kill_capture_ends_at_grapheme_boundary() {
    let text = format!("{}👩‍💻", "x".repeat(KILL_RING_LIMIT - 1));
    let killed = truncate_grapheme(&text, KILL_RING_LIMIT);
    assert!(!killed.ends_with('\u{200d}'));
    assert!(killed.len() <= KILL_RING_LIMIT);
}

#[gpui::test]
fn set_value_semantics_and_safe_event(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "old");
    input.update(cx, |input, cx| {
        input.emit_programmatic_changes = true;
        assert!(input.set_value("new\r\nvalue", cx));
        assert!(!input.set_value("new value", cx));
    });
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.revision(),
            input.selection()
        )),
        (
            "new value".into(),
            1,
            TextInputSelection {
                range: 9..9,
                reversed: false
            }
        )
    );
    assert_eq!(input.read_with(cx, |input, _| input.revision()), 1);
}

#[gpui::test]
fn composition_empty_state_cancels_before_cancel_event(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "", None, window, cx)
        })
    });
    assert!(input.read_with(cx, |input, _| input.composition().is_some()));
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.composition().is_none()));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn read_only_and_disabled_reject_mutation(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    input.update(cx, |input, _| input.editable = false);
    cx.simulate_keystrokes("backspace");
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
    input.update(cx, |input, _| {
        input.editable = true;
        input.enabled = false;
    });
    cx.simulate_keystrokes("backspace");
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn pointer_lost_button_and_deactivation_cancel_owned_gesture(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abcdef");
    input.update(cx, |input, cx| {
        input.pointer_generation = 10;
        input.pointer_gesture = Some(PointerGesture {
            generation: 10,
            anchor: 0,
            latest_position: point(px(0.0), px(0.0)),
        });
        input.on_global_mouse_move(
            &MouseMoveEvent {
                position: point(px(0.0), px(0.0)),
                pressed_button: None,
                modifiers: Default::default(),
            },
            cx,
        );
        assert!(input.pointer_gesture.is_none());
        input.pointer_gesture = Some(PointerGesture {
            generation: 11,
            anchor: 0,
            latest_position: point(px(0.0), px(0.0)),
        });
    });
    cx.deactivate_window();
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));
}

#[gpui::test]
fn caret_does_not_schedule_while_unfocused_or_inactive(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    cx.deactivate_window();
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_task.is_none()));
    input.update(cx, |input, cx| {
        input.focused = false;
        input.window_active = true;
        input.restart_caret(cx);
        assert!(input.caret_task.is_none());
    });
}

#[test]
fn grapheme_movement_and_replacement_never_split_clusters() {
    let mut buffer = TextBuffer::new("Ae\u{301}👩‍💻B".into());
    buffer.move_to(1);
    buffer.move_right(false);
    assert_eq!(buffer.selection.cursor(), "Ae\u{301}".len());
    buffer.select_to(2);
    assert_eq!(&buffer.text[buffer.selection.range.clone()], "e\u{301}");
    assert!(buffer.replace(
        buffer.selection.range.clone(),
        "X".into(),
        EditKind::Atomic,
        HARD_VALUE_LIMIT,
    ));
    assert_eq!(buffer.text.as_str(), "AX👩‍💻B");
}

#[gpui::test]
fn obscured_ascii_projection_maps_source_and_display_offsets(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "abc");

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.display_text().to_string(),
            [0, 1, 2, 3].map(|offset| input.display_offset_for_source(offset)),
            [0, 3, 6, 9].map(|offset| input.source_offset_for_display(offset)),
        )),
        ("•••".into(), [0, 3, 6, 9], [0, 1, 2, 3],)
    );
}

#[gpui::test]
fn obscured_emoji_projection_preserves_grapheme_boundaries(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "Ae\u{301}👩‍💻B");

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.display_text().to_string(),
            [0, 1, 4, 15, 16].map(|offset| input.display_offset_for_source(offset)),
            [0, 3, 6, 9, 12].map(|offset| input.source_offset_for_display(offset)),
        )),
        ("••••".into(), [0, 3, 6, 9, 12], [0, 1, 4, 15, 16],)
    );
}

#[gpui::test]
fn obscured_shaping_never_receives_plaintext(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "correct horse battery staple");

    let (display, plaintext_clone_count) = input.read_with(cx, |input, _| {
        (
            input.last_shaped_display.as_ref().map(ToString::to_string),
            input.value_shape_clone_count,
        )
    });
    assert_eq!((display, plaintext_clone_count), (Some("•".repeat(28)), 0));
}

#[gpui::test]
fn obscured_native_whole_range_query_returns_no_plaintext(cx: &mut TestAppContext) {
    const SECRET: &str = "päss👩‍💻phrase";
    let (input, cx) = obscured_input(cx, SECRET);
    let mut adjusted = Some(1..2);

    let queried = cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.text_for_range(0..SECRET.encode_utf16().count(), &mut adjusted, window, cx)
        })
    });

    assert_eq!((queried, adjusted), (None, None));
}

#[gpui::test]
fn obscured_native_composition_still_updates_and_commits(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "");

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "日本", Some(1..1), window, cx);
            input.replace_text_in_range(None, "日本語", window, cx);
        });
    });

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.composition().is_none(),
            input.buffer.history.undo.len(),
        )),
        ("日本語".into(), true, 0)
    );
}

#[gpui::test]
fn obscured_clipboard_kill_and_history_actions_are_inert(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "secret");
    cx.write_to_clipboard(ClipboardItem::new_string("clipboard sentinel".into()));

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.select_all(cx);
            input.copy(&Copy, window, cx);
            input.cut(&Cut, window, cx);
            input.kill_to_end(&KillToEnd, window, cx);
            input.yank(&Yank, window, cx);
            input.undo(&Undo, window, cx);
            input.redo(&Redo, window, cx);
        });
    });

    let clipboard = cx.update(|_, cx| cx.read_from_clipboard().and_then(bounded_clipboard_text));
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.buffer.history.undo.len(),
            input.buffer.history.redo.len(),
            clipboard,
        )),
        ("secret".into(), 0, 0, Some("clipboard sentinel".into()))
    );
}

#[gpui::test]
fn obscured_input_supports_paste_selection_and_editing_without_history(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "old");
    cx.write_to_clipboard(ClipboardItem::new_string("new👩‍💻".into()));
    input.update(cx, |input, cx| input.select_all(cx));
    cx.update(|window, cx| {
        input.update(cx, |input, cx| input.paste(&Paste, window, cx));
    });
    cx.simulate_keystrokes("backspace");

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.buffer.history.undo.len(),
        )),
        ("new".into(), 0)
    );
}

#[gpui::test]
fn obscured_input_enforces_sixteen_kibibyte_limit(cx: &mut TestAppContext) {
    install_theme(cx);
    let (input, cx) = cx.add_window_view(|window, cx| {
        TextInput::new(
            "test-input",
            "Secret",
            "x".repeat(OBSCURED_VALUE_LIMIT + 1),
            window,
            cx,
        )
        .content_mode(TextInputContentMode::Obscured)
        .input_length_limit(None)
    });

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().len(),
            input.input_length_limit,
        )),
        (OBSCURED_VALUE_LIMIT, OBSCURED_VALUE_LIMIT)
    );
}

#[gpui::test]
fn take_and_clear_remove_retained_obscured_state(cx: &mut TestAppContext) {
    let (input, cx) = obscured_input(cx, "first secret");

    let (taken, empty_after_take) = input.update(cx, |input, cx| {
        let taken = input.take_value(cx);
        let empty = input.value().is_empty()
            && input.composition.is_none()
            && input.buffer.history.undo.is_empty()
            && input.buffer.history.redo.is_empty()
            && input.initial_value_source.is_none()
            && input.geometry.is_none();
        (taken, empty)
    });
    input.update(cx, |input, cx| {
        assert!(input.set_value("second secret", cx));
        assert!(input.clear(cx));
    });

    assert_eq!(
        (
            taken,
            empty_after_take,
            input.read_with(cx, |input, _| input.value().is_empty())
        ),
        ("first secret".into(), true, true)
    );
}

#[test]
fn forward_delete_removes_one_complete_grapheme() {
    let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
    buffer.move_to(0);
    assert!(buffer.delete_forward(HARD_VALUE_LIMIT));
    assert_eq!(buffer.text.as_str(), "👩‍💻");
}

#[test]
fn configured_limit_rejects_replacement_without_selection_or_history_changes() {
    let mut buffer = TextBuffer::new("abcd".into());
    buffer.select_from_anchor(1, 3);
    let before = buffer.snapshot();
    assert!(!buffer.replace(1..3, "wxyz".into(), EditKind::Atomic, 5));
    assert_eq!(buffer.snapshot(), before);
    assert!(buffer.history.undo.is_empty());
}

#[gpui::test]
fn macos_command_option_and_control_bindings_drive_editing(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "Workspace Name");
    cx.simulate_keystrokes("alt-backspace ctrl-a X ctrl-e ctrl-h cmd-backspace");
    cx.run_until_parked();
    assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
}

#[gpui::test]
fn macos_select_all_replacement_is_one_undo_edit(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "Workspace Name");
    cx.simulate_keystrokes("cmd-a D e v cmd-z");
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "Workspace Name"
    );
}

#[gpui::test]
fn macos_kill_and_yank_share_the_bounded_application_ring(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    cx.simulate_keystrokes("ctrl-b ctrl-k ctrl-y");
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn paste_availability_matches_clipboard_and_final_value_bounds(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "safe");
    input.update(cx, |input, _| {
        input.input_length_limit = 5;
        assert!(input.can_accept_paste("x"));
        assert!(!input.can_accept_paste("xy"));
        assert!(!input.can_accept_paste(""));
        assert!(!input.can_accept_paste(&"x".repeat(CLIPBOARD_INSERTION_LIMIT + 1)));
        input.buffer.select_all();
        assert!(input.can_accept_paste("xy"));
        input.editable = false;
        assert!(!input.can_accept_paste("x"));
    });
}

#[gpui::test]
fn caret_renders_do_not_materialize_clipboard_for_paste_availability(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "safe");
    cx.write_to_clipboard(ClipboardItem::new_string(
        "x".repeat(CLIPBOARD_INSERTION_LIMIT + 1),
    ));
    let reads_before_blink = input.read_with(cx, |input, _| input.clipboard_read_count);

    cx.executor().advance_clock(CARET_BLINK_INTERVAL);
    cx.run_until_parked();

    assert_eq!(
        input.read_with(cx, |input, _| input.clipboard_read_count),
        reads_before_blink
    );

    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.clipboard_read_count),
        reads_before_blink + 1
    );
}

#[gpui::test]
fn clipboard_over_hard_limit_is_rejected_atomically(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "safe");
    input.update(cx, |input, cx| input.select_all(cx));
    cx.write_to_clipboard(ClipboardItem::new_string(
        "x".repeat(CLIPBOARD_INSERTION_LIMIT + 1),
    ));
    cx.update(|window, cx| input.update(cx, |input, cx| input.paste(&Paste, window, cx)));
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.selection(),
            input.revision(),
            input.buffer.history.undo.len(),
        )),
        (
            "safe".into(),
            TextInputSelection {
                range: 0..4,
                reversed: false,
            },
            0,
            0,
        )
    );
}

#[gpui::test]
fn oversized_input_method_update_is_rejected_without_starting_composition(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "safe");
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.input_length_limit = HARD_VALUE_LIMIT;
            input.replace_and_mark_text_in_range(
                None,
                &"x".repeat(HARD_VALUE_LIMIT + 1),
                None,
                window,
                cx,
            );
        });
    });
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.revision(),
            input.composition().is_none(),
            input.buffer.history.undo.len(),
        )),
        ("safe".into(), 0, true, 0)
    );
}

#[gpui::test]
fn oversized_final_input_method_commit_ends_active_composition_atomically(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "safe", false);
    mark_text(&input, cx, "日");
    events.borrow_mut().clear();
    let before = input.read_with(cx, |input, _| {
        (
            input.value().to_owned(),
            input.revision(),
            input.selection(),
        )
    });

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_text_in_range(None, &"x".repeat(DEFAULT_VALUE_LIMIT + 1), window, cx);
        });
    });

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.revision(),
            input.selection(),
            input.composition().is_none(),
            input.buffer.history.undo.len(),
        )),
        (before.0, before.1, before.2, true, 1),
        "a rejected final commit must end composition without replacing accepted marked text"
    );
    assert_eq!(
        events.borrow().as_slice(),
        &[TextInputEvent::CompositionCommitted]
    );

    cx.update(|window, cx| input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "safe"
    );
}

#[gpui::test]
fn kill_ring_capture_is_bounded_while_the_whole_selection_is_deleted(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "seed");
    let value = format!("{}👩‍💻tail", "x".repeat(KILL_RING_LIMIT));
    input.update(cx, |input, cx| {
        input.input_length_limit = HARD_VALUE_LIMIT;
        assert!(input.set_value(value, cx));
        input.select_all(cx);
    });
    cx.update(|window, cx| {
        input.update(cx, |input, cx| input.kill_to_end(&KillToEnd, window, cx));
    });
    assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    let killed = cx.update(|_, cx| cx.global::<TextKillRing>().0.clone());
    assert!(killed.len() <= KILL_RING_LIMIT);
    assert!(killed.is_char_boundary(killed.len()));
}

#[gpui::test]
fn native_unmark_commits_and_immediately_restarts_caret_presentation(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    mark_text(&input, cx, "日");
    let generation = input.read_with(cx, |input, _| input.caret_generation);

    cx.update(|window, cx| {
        input.update(cx, |input, cx| input.unmark_text(window, cx));
    });

    assert!(input.read_with(cx, |input, _| {
        input.composition().is_none() && input.caret_visible && input.caret_generation > generation
    }));
}

#[gpui::test]
fn selection_only_and_unchanged_ime_commits_restart_hidden_caret(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    mark_text(&input, cx, "日");
    let selection_update_generation = input.update(cx, |input, _| {
        input.caret_visible = false;
        input.caret_generation
    });

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "日", Some(0..0), window, cx);
        });
    });
    assert!(input.read_with(cx, |input, _| {
        input.caret_visible && input.caret_generation > selection_update_generation
    }));

    let commit_generation = input.update(cx, |input, _| {
        input.caret_visible = false;
        input.caret_generation
    });
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_text_in_range(None, "日", window, cx);
        });
    });
    assert!(input.read_with(cx, |input, _| {
        input.composition().is_none()
            && input.caret_visible
            && input.caret_generation > commit_generation
    }));
}

#[gpui::test]
fn composition_movement_commits_one_undoable_edit_before_moving(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    mark_text(&input, cx, "日");
    cx.update(|window, cx| {
        input.update(cx, |input, cx| input.move_left(&MoveLeft, window, cx));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[
            TextInputEvent::CompositionStarted,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 1,
                source: TextInputChangeSource::InputMethodComposition,
            }),
            TextInputEvent::CompositionCommitted,
        ]
    );
    cx.update(|window, cx| input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn composition_undo_commits_then_restores_with_exact_revision_order(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    mark_text(&input, cx, "日本");
    cx.update(|window, cx| input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
    assert_eq!(
        events.borrow().as_slice(),
        &[
            TextInputEvent::CompositionStarted,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 1,
                source: TextInputChangeSource::InputMethodComposition,
            }),
            TextInputEvent::CompositionCommitted,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 2,
                source: TextInputChangeSource::Undo,
            }),
        ]
    );
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn composition_redo_and_return_commit_each_interrupt_composition(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    mark_text(&input, cx, "日");
    cx.update(|window, cx| input.update(cx, |input, cx| input.redo(&Redo, window, cx)));
    assert!(input.read_with(cx, |input, _| input.composition().is_none()));
    mark_text(&input, cx, "本");
    cx.update(|window, cx| input.update(cx, |input, cx| input.submit(&Submit, window, cx)));
    assert!(input.read_with(cx, |input, _| input.composition().is_none()));
    assert!(events.borrow().ends_with(&[
        TextInputEvent::CompositionStarted,
        TextInputEvent::ValueChanged(TextInputValueChanged {
            revision: 2,
            source: TextInputChangeSource::InputMethodComposition,
        }),
        TextInputEvent::CompositionCommitted,
    ]));
    assert!(!events.borrow().contains(&TextInputEvent::Submitted));
}

#[gpui::test]
fn empty_marked_text_continues_and_cancel_restores_original_state(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "", Some(0..0), window, cx);
            assert!(input.composition().is_some());
            input.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
            input.cancel(&Cancel, window, cx);
        });
    });
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.composition().is_none(),
            input.revision(),
        )),
        ("abc".into(), true, 2)
    );
    assert_eq!(
        events.borrow().as_slice(),
        &[
            TextInputEvent::CompositionStarted,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 1,
                source: TextInputChangeSource::InputMethodComposition,
            }),
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 2,
                source: TextInputChangeSource::InputMethodComposition,
            }),
            TextInputEvent::CompositionCancelled,
        ]
    );
}

#[gpui::test]
fn repeated_combining_mark_updates_preserve_adjacent_grapheme_and_ranges(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "e");
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "\u{301}", None, window, cx);
            input.replace_and_mark_text_in_range(None, "\u{308}", None, window, cx);
        });
    });

    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input
                .composition()
                .map(|composition| (composition.marked_range(), composition.selection().range(),)),
        )),
        ("e\u{308}".to_owned(), Some((1..3, 3..3)))
    );
}

#[gpui::test]
fn multi_update_composition_commits_as_exactly_one_undo_edit(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "Workspace");
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "日", None, window, cx);
            input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
            input.replace_text_in_range(None, "日本", window, cx);
            assert_eq!(input.buffer.history.undo.len(), 1);
        });
    });
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "Workspace"
    );
}

#[gpui::test]
fn pointer_press_commits_composition_before_starting_selection(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abcdef", false);
    mark_text(&input, cx, "日");
    let click_position = input.read_with(cx, |input, _| {
        let bounds = input.last_bounds.expect("input should be painted");
        let geometry = input
            .geometry
            .as_ref()
            .expect("marked text should have current geometry");
        point(
            bounds.left() + geometry.line.x_for_index(1) - input.scroll,
            bounds.center().y,
        )
    });
    cx.simulate_mouse_down(click_position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(click_position, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.composition().is_none(),
            input.selection().range(),
        )),
        (true, 1..1),
        "composition commit must preserve the click's hit-tested position"
    );
    assert!(
        events
            .borrow()
            .contains(&TextInputEvent::CompositionCommitted)
    );
}

#[gpui::test]
fn real_pointer_drag_selects_inside_and_releases_outside(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abcdefghijklmnopqrstuvwxyz");
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    let start = point(bounds.left() + px(4.0), bounds.center().y);
    let outside = point(bounds.right() + px(40.0), bounds.center().y);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(bounds.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| {
        !input.selection().is_empty()
            && input.pointer_gesture.is_none()
            && input.autoscroll_task.is_none()
    }));
}

#[gpui::test]
fn real_pointer_drag_autoscroll_progresses_on_each_deadline(cx: &mut TestAppContext) {
    let (input, cx) = input(
        cx,
        concat!(
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        ),
    );
    input.update(cx, |input, cx| {
        input.buffer.move_to(0);
        input.scroll = px(0.0);
        input.restart_caret(cx);
    });
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    let start = point(bounds.left() + px(2.0), bounds.center().y);
    let outside = point(bounds.right() + px(50.0), bounds.center().y);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let before_deadline = input.read_with(cx, |input, _| (input.scroll, input.selection()));
    cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let pending_deadline = input.read_with(cx, |input, _| (input.scroll, input.selection()));
    assert_eq!(
        pending_deadline, before_deadline,
        "outside drag must wait for the first bounded autoscroll deadline"
    );
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
    let first = input.read_with(cx, |input, _| input.scroll);
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
    let second = input.read_with(cx, |input, _| input.scroll);
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
    assert!(first > px(0.0), "first autoscroll deadline must advance");
    assert!(
        second > first,
        "later autoscroll deadline must progress: first={first:?}, second={second:?}"
    );
}

#[gpui::test]
fn stale_autoscroll_generation_is_inert_and_releases_its_task_slot(cx: &mut TestAppContext) {
    let (input, cx) = input(
        cx,
        concat!(
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        ),
    );
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    input.update(cx, |input, cx| {
        input.buffer.move_to(0);
        input.scroll = px(0.0);
        input.pointer_gesture = Some(PointerGesture {
            generation: 7,
            anchor: 0,
            latest_position: point(bounds.right() + px(50.0), bounds.center().y),
        });
        input.start_autoscroll(7, cx);
    });
    cx.run_until_parked();
    input.update(cx, |input, _| {
        input.pointer_gesture = Some(PointerGesture {
            generation: 8,
            anchor: 0,
            latest_position: bounds.center(),
        });
    });
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| {
        input.scroll == px(0.0)
            && input
                .pointer_gesture
                .is_some_and(|gesture| gesture.generation == 8)
            && input.autoscroll_generation.is_none()
            && input.autoscroll_task.is_none()
    }));
}

#[gpui::test]
fn lost_button_blur_disable_and_stale_generation_cancel_drag(cx: &mut TestAppContext) {
    let (input, other_focus, _, cx) = input_with_events(cx, "abcdef", false);
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));

    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
    cx.update(|window, _| other_focus.focus(window));
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));

    input.update(cx, |input, cx| {
        input.focused = true;
        input.pointer_generation = 40;
        input.pointer_gesture = Some(PointerGesture {
            generation: 39,
            anchor: 0,
            latest_position: bounds.center(),
        });
        input.set_enabled(false, cx);
        assert!(input.pointer_gesture.is_none());
        assert!(input.pointer_generation > 40);
    });
}

#[gpui::test]
fn synchronous_marked_update_refreshes_range_and_character_geometry(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    let (marked_bounds, character) = cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
            let marked = input
                .bounds_for_range(3..5, bounds, window, cx)
                .expect("marked text bounds should exist");
            let character = input
                .character_index_for_point(marked.bottom_right(), window, cx)
                .expect("character geometry should exist");
            (marked, character)
        })
    });
    assert!(marked_bounds.size.width > px(0.0));
    assert_eq!(character, 5);
}

#[gpui::test]
fn synchronous_marked_update_reconciles_horizontal_scroll_for_candidate_geometry(
    cx: &mut TestAppContext,
) {
    const LONG_VALUE: &str = concat!(
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
    );
    let (input, cx) = input(cx, LONG_VALUE);
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    assert!(input.read_with(cx, |input, _| input.scroll > px(0.0)));

    let (candidate, character) = cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(
                Some(0..LONG_VALUE.len()),
                "日本",
                Some(2..2),
                window,
                cx,
            );
            let candidate = input
                .bounds_for_range(2..2, bounds, window, cx)
                .expect("candidate bounds should exist");
            let character = input
                .character_index_for_point(candidate.bottom_right(), window, cx)
                .expect("candidate point should map to current text");
            (candidate, character)
        })
    });

    assert!(
        candidate.left() >= bounds.left() && candidate.right() <= bounds.right(),
        "candidate geometry used stale scroll: candidate={candidate:?}, input={bounds:?}"
    );
    assert_eq!(character, 2);
}

#[gpui::test]
fn caret_restart_waits_a_full_interval_and_stale_generation_is_inert(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    cx.executor().advance_clock(CARET_BLINK_INTERVAL / 2);
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_visible));
    let stale_generation = input.read_with(cx, |input, _| input.caret_generation);
    input.update(cx, |input, cx| input.restart_caret(cx));
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_generation != stale_generation));
    cx.executor()
        .advance_clock(CARET_BLINK_INTERVAL - Duration::from_millis(1));
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_visible));
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(!input.read_with(cx, |input, _| input.caret_visible));
}

#[gpui::test]
fn caret_reactivation_is_visible_and_schedules_only_when_active(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "abc");
    cx.executor().advance_clock(CARET_BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!input.read_with(cx, |input, _| input.caret_visible));
    cx.deactivate_window();
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_visible
        && input.caret_task.is_none()));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(input.read_with(cx, |input, _| input.caret_visible
        && input.caret_task.is_some()));
}

#[gpui::test]
fn caret_phase_changes_reuse_stable_shaping(cx: &mut TestAppContext) {
    let (input, cx) = input(cx, "stable shaping");
    let before = input.read_with(cx, |input, _| {
        (input.shape_count, input.value_shape_clone_count)
    });
    cx.executor().advance_clock(CARET_BLINK_INTERVAL);
    cx.run_until_parked();
    let after_cache_hit = input.read_with(cx, |input, _| {
        (input.shape_count, input.value_shape_clone_count)
    });
    assert_eq!(after_cache_hit, before);

    input.update(cx, |input, cx| {
        assert!(input.set_value("changed shaping", cx));
    });
    cx.run_until_parked();
    let after_value_change = input.read_with(cx, |input, _| {
        (input.shape_count, input.value_shape_clone_count)
    });
    assert_eq!(after_value_change, (before.0 + 1, before.1 + 1));
}

#[gpui::test]
fn runtime_editable_and_enabled_transitions_preserve_selection_and_gate_edits(
    cx: &mut TestAppContext,
) {
    let (input, cx) = input(cx, "abc");
    input.update(cx, |input, cx| {
        input.select_all(cx);
        input.set_editable(false, cx);
    });
    cx.simulate_keystrokes("backspace");
    assert_eq!(
        input.read_with(cx, |input, _| input.selection().range()),
        0..3
    );
    input.update(cx, |input, cx| {
        input.set_editable(true, cx);
        input.set_enabled(false, cx);
    });
    cx.simulate_keystrokes("backspace");
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "abc"
    );
    input.update(cx, |input, cx| input.set_enabled(true, cx));
    cx.simulate_keystrokes("backspace");
    assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
}

#[gpui::test]
fn set_value_cancels_composition_clears_history_and_emits_in_order(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    mark_text(&input, cx, "日");
    events.borrow_mut().clear();
    input.update(cx, |input, cx| {
        input.emit_programmatic_changes = true;
        assert!(input.set_value("new\r\nvalue", cx));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[
            TextInputEvent::CompositionCancelled,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 2,
                source: TextInputChangeSource::Programmatic,
            }),
        ]
    );
    assert_eq!(
        input.read_with(cx, |input, _| (
            input.value().to_owned(),
            input.selection().range(),
            input.composition().is_none(),
            input.buffer.history.undo.len(),
            input.buffer.history.redo.len(),
        )),
        ("new value".into(), 9..9, true, 0, 0)
    );
}

#[gpui::test]
fn default_return_and_escape_behaviors_consume_before_parent(cx: &mut TestAppContext) {
    install_theme(cx);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| TextInput::new("test-input", "Test input", "", window, cx));
        EventRoot {
            input,
            other_focus: cx.focus_handle(),
            unrelated_menu: false,
            outer_submit: Rc::new(Cell::new(0)),
            outer_cancel: Rc::new(Cell::new(0)),
        }
    });
    let (input, submit, cancel) = root.read_with(cx, |root, _| {
        (
            root.input.clone(),
            root.outer_submit.clone(),
            root.outer_cancel.clone(),
        )
    });
    cx.update(|window, cx| input.read(cx).focus_handle().focus(window));

    cx.simulate_keystrokes("enter escape");

    assert_eq!((submit.get(), cancel.get()), (0, 0));
}

#[gpui::test]
fn propagated_return_and_escape_reach_parent_after_typed_events(cx: &mut TestAppContext) {
    install_theme(cx);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            TextInput::new("test-input", "Test input", "", window, cx)
                .return_behavior(TextInputReturnBehavior::Propagate)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
        });
        EventRoot {
            input,
            other_focus: cx.focus_handle(),
            unrelated_menu: false,
            outer_submit: Rc::new(Cell::new(0)),
            outer_cancel: Rc::new(Cell::new(0)),
        }
    });
    let (input, submit, cancel) = root.read_with(cx, |root, _| {
        (
            root.input.clone(),
            root.outer_submit.clone(),
            root.outer_cancel.clone(),
        )
    });
    cx.update(|window, cx| input.read(cx).focus_handle().focus(window));

    cx.simulate_keystrokes("enter escape");

    assert_eq!((submit.get(), cancel.get()), (1, 1));
}

#[gpui::test]
fn propagated_escape_consumes_composition_before_later_parent_cancel(cx: &mut TestAppContext) {
    install_theme(cx);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            TextInput::new("test-input", "Test input", "", window, cx)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
        });
        EventRoot {
            input,
            other_focus: cx.focus_handle(),
            unrelated_menu: false,
            outer_submit: Rc::new(Cell::new(0)),
            outer_cancel: Rc::new(Cell::new(0)),
        }
    });
    let (input, cancel) = root.read_with(cx, |root, _| {
        (root.input.clone(), root.outer_cancel.clone())
    });
    cx.update(|window, cx| input.read(cx).focus_handle().focus(window));
    mark_text(&input, cx, "日");

    cx.simulate_keystrokes("escape");
    assert_eq!(cancel.get(), 0);
    cx.simulate_keystrokes("escape");

    assert_eq!(cancel.get(), 1);
}

#[gpui::test]
fn propagated_tab_requests_are_typed_and_keep_focus_with_the_input(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    input.update(cx, |input, _| {
        input.tab_behavior = TextInputTabBehavior::Propagate;
    });

    cx.simulate_keystrokes("tab shift-tab");
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        &[
            TextInputEvent::TabForwardRequested,
            TextInputEvent::TabBackwardRequested,
        ]
    );
    assert!(input.read_with(cx, |input, _| input.is_focused()));
}

#[gpui::test]
fn owned_context_menu_reports_truthful_focus_and_lifecycle_continuity(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", false);
    let bounds = cx
        .debug_bounds("test-input")
        .expect("input should be painted");
    cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
    assert!(!input.read_with(cx, |input, _| input.is_focused()));
    assert_eq!(
        &events.borrow()[..2],
        &[TextInputEvent::ContextMenuOpened, TextInputEvent::FocusLost,]
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(events.borrow().contains(&TextInputEvent::ContextMenuClosed));
    assert!(cx.update(|window, cx| input.read(cx).focus_handle().is_focused(window)));
    assert!(input.read_with(cx, |input, _| input.is_focused()));
}

#[gpui::test]
fn unrelated_menu_causes_focus_lost_without_owned_context_events(cx: &mut TestAppContext) {
    let (input, _, events, cx) = input_with_events(cx, "abc", true);
    let menu = cx
        .debug_bounds("unrelated-menu-target")
        .expect("unrelated menu should be painted");
    cx.simulate_click(menu.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(!input.read_with(cx, |input, _| input.is_focused()));
    assert!(events.borrow().contains(&TextInputEvent::FocusLost));
    assert!(!events.borrow().iter().any(|event| matches!(
        event,
        TextInputEvent::ContextMenuOpened | TextInputEvent::ContextMenuClosed
    )));
}
