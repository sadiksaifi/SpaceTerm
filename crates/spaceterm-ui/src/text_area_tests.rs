use std::{cell::RefCell, rc::Rc};

use super::*;
use gpui::{TestAppContext, VisualTestContext, rgba};

use crate::{TextInputMetrics, TextInputTheme, TextInputVariants};

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
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x606079ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0xd8647eff),
    );
    let metrics = crate::menu::MenuMetrics::new(px(160.0), px(26.0));
    cx.set_global(crate::menu::MenuTheme::new(
        menu_paint,
        crate::menu::MenuSizes::new(metrics, metrics, metrics),
    ));
    cx.update(crate::text_input::init);
    cx.update(crate::menu::init);
    cx.update(|cx| install_text_area_keybindings(cx, TextInputKeybindingProfile::MacOs));
}

fn area<'a>(
    cx: &'a mut TestAppContext,
    value: &'static str,
    configure: impl FnOnce(TextArea) -> TextArea + 'static,
) -> (Entity<TextArea>, &'a mut VisualTestContext) {
    install_theme(cx);
    let (area, cx) = cx.add_window_view(move |window, cx| {
        configure(TextArea::new("test-area", "Test area", value, window, cx))
    });
    cx.update(|window, cx| {
        window.activate_window();
        area.read(cx).focus_handle().focus(window, cx);
    });
    cx.run_until_parked();
    (area, cx)
}

fn value(area: &Entity<TextArea>, cx: &mut VisualTestContext) -> String {
    cx.read(|cx| area.read(cx).value().to_owned())
}

fn caret(area: &Entity<TextArea>, cx: &mut VisualTestContext) -> usize {
    cx.read(|cx| area.read(cx).buffer.selection.cursor())
}

fn place_caret(area: &Entity<TextArea>, line: usize, column: usize, cx: &mut VisualTestContext) {
    area.update(cx, |area, cx| area.move_caret_to_position(line, column, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn typing_and_return_break_lines_and_keep_indentation(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "{\n  \"a\": 1", |area| area);
    place_caret(&area, 2, 9, cx);

    cx.simulate_keystrokes(", enter b");

    assert_eq!(value(&area, cx), "{\n  \"a\": 1,\n  b");
}

#[gpui::test]
fn vertical_movement_keeps_the_goal_column_across_a_short_line(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "abcdef\nab\nabcdef", |area| area);
    place_caret(&area, 1, 5, cx);

    cx.simulate_keystrokes("down");
    assert_eq!(caret(&area, cx), "abcdef\nab".len());

    cx.simulate_keystrokes("down");
    assert_eq!(caret(&area, cx), "abcdef\nab\nabcd".len());

    cx.simulate_keystrokes("up up");
    assert_eq!(caret(&area, cx), "abcd".len());
}

#[gpui::test]
fn moving_past_the_first_or_last_line_reaches_that_end(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "abc\ndef", |area| area);
    place_caret(&area, 1, 2, cx);

    cx.simulate_keystrokes("up");
    assert_eq!(caret(&area, cx), 0);

    cx.simulate_keystrokes("down down");
    assert_eq!(caret(&area, cx), "abc\ndef".len());
}

#[gpui::test]
fn line_edges_stop_at_the_current_line(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "one\ntwo\nthree", |area| area);
    place_caret(&area, 2, 2, cx);

    cx.simulate_keystrokes("cmd-right");
    assert_eq!(caret(&area, cx), "one\ntwo".len());

    cx.simulate_keystrokes("cmd-left");
    assert_eq!(caret(&area, cx), "one\n".len());

    cx.simulate_keystrokes("cmd-down");
    assert_eq!(caret(&area, cx), "one\ntwo\nthree".len());
}

#[gpui::test]
fn tab_indents_and_shift_tab_outdents_every_selected_line(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "a\n  b\nc", |area| area);
    place_caret(&area, 1, 1, cx);

    cx.simulate_keystrokes("shift-down shift-down shift-right tab");
    assert_eq!(value(&area, cx), "  a\n    b\n  c");

    cx.simulate_keystrokes("shift-tab shift-tab");
    assert_eq!(value(&area, cx), "a\nb\nc");

    cx.simulate_keystrokes("cmd-z");
    assert_eq!(value(&area, cx), "a\n  b\nc");
}

#[gpui::test]
fn a_selection_ending_at_a_line_start_leaves_that_line_unindented(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "a\nb", |area| area);
    place_caret(&area, 1, 1, cx);

    cx.simulate_keystrokes("shift-down tab");

    assert_eq!(value(&area, cx), "  a\nb");
}

#[gpui::test]
fn tab_without_a_multi_line_selection_inserts_the_indent_unit(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "ab", |area| area);
    place_caret(&area, 1, 2, cx);

    cx.simulate_keystrokes("tab");

    assert_eq!(value(&area, cx), "a  b");
}

#[gpui::test]
fn paste_normalizes_line_breaks_tabs_and_control_characters(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "", |area| area);
    cx.write_to_clipboard(ClipboardItem::new_string(
        "a\r\nb\rc\u{2028}d\te\u{7}f".into(),
    ));

    cx.simulate_keystrokes("cmd-v");

    assert_eq!(value(&area, cx), "a\nb\nc\nd  e f");
}

#[gpui::test]
fn read_only_content_rejects_edits_and_still_copies(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "{\"a\": 1}", |area| area.editable(false));
    cx.write_to_clipboard(ClipboardItem::new_string("pasted".into()));

    cx.simulate_keystrokes("x enter tab backspace cmd-v cmd-a cmd-x cmd-c");

    assert_eq!(value(&area, cx), "{\"a\": 1}");
    let clipboard = cx.update(|_, cx| cx.read_from_clipboard().and_then(bounded_clipboard_text));
    assert_eq!(clipboard.as_deref(), Some("{\"a\": 1}"));
}

#[gpui::test]
fn becoming_editable_allows_edits(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "", |area| area.editable(false));
    area.update(cx, |area, cx| area.set_editable(true, cx));

    cx.simulate_keystrokes("x");

    assert_eq!(value(&area, cx), "x");
}

#[gpui::test]
fn edits_beyond_the_value_limit_are_rejected(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "abc", |area| area.input_length_limit(Some(4)));
    place_caret(&area, 1, 4, cx);

    cx.simulate_keystrokes("d e enter");

    assert_eq!(value(&area, cx), "abcd");
    let replaced = area.update(cx, |area, cx| area.set_value("abcde", cx));
    assert!(!replaced);
    assert_eq!(value(&area, cx), "abcd");
}

#[gpui::test]
fn set_value_replaces_the_text_and_clears_undo(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "a", |area| area);
    place_caret(&area, 1, 2, cx);
    cx.simulate_keystrokes("b");

    area.update(cx, |area, cx| area.set_value("x\r\ny", cx));
    cx.simulate_keystrokes("cmd-z");

    assert_eq!(value(&area, cx), "x\ny");
    assert_eq!(caret(&area, cx), 0);
}

#[gpui::test]
fn value_changes_report_the_revision_without_content(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "", |area| area);
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&area, move |_, event: &TextAreaEvent, _| {
            recorded.borrow_mut().push(*event);
        })
        .detach();
    });

    cx.simulate_keystrokes("a enter");

    assert_eq!(
        *events.borrow(),
        [
            TextAreaEvent::ValueChanged { revision: 1 },
            TextAreaEvent::ValueChanged { revision: 2 },
        ]
    );
}

#[gpui::test]
fn delete_to_line_start_joins_lines_at_the_start_of_a_line(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "ab\ncd", |area| area);
    place_caret(&area, 2, 2, cx);

    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(value(&area, cx), "ab\nd");

    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(value(&area, cx), "abd");
}

#[gpui::test]
fn caret_positions_count_characters_and_clamp_to_the_line(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "é1\nabc", |area| area);

    place_caret(&area, 1, 2, cx);
    assert_eq!(caret(&area, cx), "é".len());

    place_caret(&area, 2, 99, cx);
    assert_eq!(caret(&area, cx), "é1\nabc".len());

    place_caret(&area, 9, 1, cx);
    assert_eq!(caret(&area, cx), "é1\n".len());
}

#[gpui::test]
fn values_up_to_four_mebibytes_are_retained(cx: &mut TestAppContext) {
    let (area, cx) = area(cx, "", |area| area.editable(false).input_length_limit(None));
    let line = format!("{}\n", "x".repeat(1023));
    let largest = line.repeat(4 * 1024);

    assert!(area.update(cx, |area, cx| area.set_value(largest.clone(), cx)));
    assert_eq!(value(&area, cx).len(), 4 * 1024 * 1024);
    assert!(!area.update(cx, |area, cx| area.set_value(format!("{largest}x"), cx)));
}
