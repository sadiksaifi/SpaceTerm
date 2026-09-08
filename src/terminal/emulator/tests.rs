#[cfg(feature = "macos-native-tests")]
use std::fs;

use super::*;
use crate::terminal::TerminalAccessibilityModel;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use crate::terminal::metadata::{DirectoryProvenance, ProgressMetadata};

fn geometry(cols: u16, rows: u16, cell_width: f32, cell_height: f32) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(cols, rows),
        LogicalCellSize::new(cell_width, cell_height),
        BackingScale::ONE,
    )
}

fn emulator(cols: u16, rows: u16) -> TerminalEmulator {
    TerminalEmulator::new(geometry(cols, rows, 10.0, 20.0)).unwrap()
}

fn emulator_with_terminal_name(
    cols: u16,
    rows: u16,
    terminal_name: &'static str,
) -> TerminalEmulator {
    TerminalEmulator::new_with_metadata(
        geometry(cols, rows, 10.0, 20.0),
        "",
        "",
        None,
        terminal_name,
        Instant::now(),
    )
    .unwrap()
}

#[test]
fn terminal_find_matches_across_soft_wraps() {
    let mut emulator = emulator(3, 2);
    emulator.feed(b"abcdef");
    emulator.set_find_query(FindQueryGeneration::test(1), "cde".to_owned());

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 1);
}

#[test]
fn accessibility_snapshot_preserves_production_soft_wraps() {
    let mut emulator = emulator(3, 2);
    emulator.feed(b"abcdef");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let accessibility = TerminalAccessibilityModel::from_screen(&snapshot);

    assert_eq!(snapshot.row_soft_wrapped.as_ref(), &[true, false]);
    assert_eq!(accessibility.text(), "abcdef");
    assert_eq!(accessibility.range_for_line(0), Some(0..3));
    assert_eq!(accessibility.range_for_line(1), Some(3..6));
}

#[test]
fn terminal_find_rejects_matches_across_hard_lines() {
    let mut emulator = emulator(4, 2);
    emulator.feed(b"abc\r\ndef");
    emulator.set_find_query(FindQueryGeneration::test(1), "cde".to_owned());

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 0);
}

#[test]
fn terminal_find_includes_primary_scrollback() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"needle\r\nsecond\r\nthird");
    emulator.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 1);
}

#[test]
fn terminal_find_highlights_the_complete_wide_grapheme() {
    let mut emulator = emulator(8, 2);
    emulator.feed("🙂".as_bytes());
    emulator.set_find_query(FindQueryGeneration::test(1), "🙂".to_owned());

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(
        snapshot.find.as_ref().unwrap().visible_spans.as_ref(),
        [crate::terminal::FindHighlightSpan {
            row: 0,
            start_column: 0,
            end_column: 1,
            current: false,
        }]
    );
}

#[test]
fn terminal_find_query_only_snapshot_reuses_rows() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"needle");
    let first = emulator.snapshot().unwrap().unwrap();
    emulator.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());

    let found = emulator.snapshot().unwrap().unwrap();

    assert!(found.damage.search);
    assert_eq!(found.damage.content, ContentDamageSnapshot::Clean);
    assert!(
        first
            .rows
            .iter()
            .zip(found.rows.iter())
            .all(|(first, second)| Arc::ptr_eq(first, second))
    );
}

#[test]
fn terminal_find_navigation_rejects_stale_query_generation() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"needle");
    emulator.set_find_query(FindQueryGeneration::test(2), "needle".to_owned());
    let _ = emulator.snapshot().unwrap().unwrap();

    let action = emulator
        .navigate_find(FindQueryGeneration::test(1), FindDirection::Next)
        .unwrap();

    assert!(!action.screen_changed);
}

#[test]
fn terminal_find_navigation_wraps_and_marks_the_current_result() {
    let mut emulator = emulator(12, 2);
    emulator.feed(b"one one");
    let generation = FindQueryGeneration::test(1);
    emulator.set_find_query(generation, "one".to_owned());
    let _ = emulator.snapshot().unwrap().unwrap();

    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let first = emulator.snapshot().unwrap().unwrap();
    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let second = emulator.snapshot().unwrap().unwrap();
    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let wrapped = emulator.snapshot().unwrap().unwrap();

    assert_eq!(first.find.as_ref().unwrap().current_match, Some(1));
    assert_eq!(second.find.as_ref().unwrap().current_match, Some(2));
    assert_eq!(wrapped.find.as_ref().unwrap().current_match, Some(1));
}

#[test]
fn terminal_find_initial_navigation_starts_in_the_current_viewport() {
    let mut emulator = emulator(16, 2);
    emulator.feed(b"needle old\r\nmiddle\r\nneedle new");
    let generation = FindQueryGeneration::test(1);
    emulator.set_find_query(generation, "needle".to_owned());
    let before = emulator.snapshot().unwrap().unwrap();
    assert!(before.scrollbar.offset_rows > 0);

    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let selected = emulator.snapshot().unwrap().unwrap();

    assert_eq!(selected.find.as_ref().unwrap().current_match, Some(2));
    assert_eq!(selected.scrollbar.offset_rows, before.scrollbar.offset_rows);
}

#[test]
fn terminal_find_navigation_scrolls_an_offscreen_match_completely_into_view() {
    let mut emulator = emulator(16, 2);
    emulator.feed(b"needle\r\nmiddle\r\nbottom");
    let generation = FindQueryGeneration::test(1);
    emulator.set_find_query(generation, "needle".to_owned());
    let _ = emulator.snapshot().unwrap().unwrap();

    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let selected = emulator.snapshot().unwrap().unwrap();

    assert_eq!(selected.scrollbar.offset_rows, 0);
    assert_eq!(selected.find.as_ref().unwrap().current_match, Some(1));
}

#[test]
fn terminal_find_preserves_current_result_across_output_and_reflow() {
    let mut emulator = emulator(8, 2);
    let generation = FindQueryGeneration::test(1);
    emulator.feed(b"needle");
    emulator.set_find_query(generation, "needle".to_owned());
    let _ = emulator.snapshot().unwrap().unwrap();
    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let _ = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"\r\nmore");
    let after_output = emulator.snapshot().unwrap().unwrap();
    emulator.resize(geometry(4, 3, 10.0, 20.0)).unwrap();
    let after_reflow = emulator.snapshot().unwrap().unwrap();

    assert_eq!(after_output.find.as_ref().unwrap().current_match, Some(1));
    assert_eq!(after_reflow.find.as_ref().unwrap().current_match, Some(1));
}

#[test]
fn terminal_find_drops_current_result_when_scrollback_prunes_it() {
    let mut emulator = emulator(8, 2);
    let generation = FindQueryGeneration::test(1);
    emulator.feed(b"needle");
    emulator.set_find_query(generation, "needle".to_owned());
    let _ = emulator.snapshot().unwrap().unwrap();
    let _ = emulator
        .navigate_find(generation, FindDirection::Next)
        .unwrap();
    let _ = emulator.snapshot().unwrap().unwrap();

    emulator.feed(format!("\r\n{}", "x\r\n".repeat(MAX_SCROLLBACK_ROWS * 2)).as_bytes());
    let pruned = emulator.snapshot().unwrap().unwrap();

    assert_eq!(pruned.find.as_ref().unwrap().total_matches, 0);
    assert_eq!(pruned.find.as_ref().unwrap().current_match, None);
}

#[test]
fn terminal_find_active_screen_scope_restores_primary_results() {
    let mut emulator = emulator(12, 2);
    let generation = FindQueryGeneration::test(1);
    emulator.feed(b"primary");
    emulator.set_find_query(generation, "primary".to_owned());
    let primary = emulator.snapshot().unwrap().unwrap();
    emulator.feed(b"\x1b[?1049halternate");
    let alternate = emulator.snapshot().unwrap().unwrap();
    emulator.feed(b"\x1b[?1049l");
    let restored = emulator.snapshot().unwrap().unwrap();

    assert_eq!(primary.find.as_ref().unwrap().total_matches, 1);
    assert_eq!(alternate.find.as_ref().unwrap().total_matches, 0);
    assert_eq!(restored.find.as_ref().unwrap().total_matches, 1);
}

#[test]
fn pixel_mouse_coordinates_should_share_fractional_backing_geometry() {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(10, 2),
        LogicalCellSize::new(7.5, 20.0),
        BackingScale::new(1.5).unwrap(),
    );
    let mut emulator = TerminalEmulator::new(geometry).unwrap();
    emulator.feed(b"\x1b[?1003h\x1b[?1016h");

    let action = emulator
        .pointer(pointer(PointerPhase::Motion, None, 5.625, 15.0, false))
        .unwrap();

    assert_eq!(
        (action.bytes, emulator.mouse_encoder_size()),
        (
            b"\x1b[<35;6;15M".to_vec(),
            MouseEncoderSize {
                screen_width: 113,
                screen_height: 60,
                cell_width: 12,
                cell_height: 30,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            },
        )
    );
}

#[test]
fn cell_mouse_coordinates_should_not_drift_across_fractional_backing_cells() {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(10, 2),
        LogicalCellSize::new(7.5, 20.0),
        BackingScale::new(1.5).unwrap(),
    );
    let mut emulator = TerminalEmulator::new(geometry).unwrap();
    emulator.feed(b"\x1b[?1003h\x1b[?1006h");

    let action = emulator
        .pointer(pointer(PointerPhase::Motion, None, 11.5, 1.0, false))
        .unwrap();

    assert_eq!(action.bytes, b"\x1b[<35;2;1M");
}

#[test]
fn conventional_mouse_protocol_mode_matrix_is_byte_exact() {
    let mut x10 = emulator(10, 3);
    x10.feed(b"\x1b[?9h");
    assert_eq!(
        x10.pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            11.0,
            21.0,
            false,
        ))
        .unwrap()
        .bytes,
        b"\x1b[M \"\""
    );

    let mut normal = emulator(10, 3);
    normal.feed(b"\x1b[?1000h");
    _ = normal
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            11.0,
            21.0,
            false,
        ))
        .unwrap();
    assert_eq!(
        normal
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
        b"\x1b[M#\"\""
    );

    let mut button = emulator(10, 3);
    button.feed(b"\x1b[?1002h");
    _ = button
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            11.0,
            21.0,
            false,
        ))
        .unwrap();
    assert_eq!(
        button
            .pointer(pointer(
                PointerPhase::Motion,
                Some(PointerButton::Left),
                21.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
        b"\x1b[M@#\""
    );

    let mut any_motion = emulator(10, 3);
    any_motion.feed(b"\x1b[?1003h");
    assert_eq!(
        any_motion
            .pointer(pointer(PointerPhase::Motion, None, 11.0, 21.0, false))
            .unwrap()
            .bytes,
        b"\x1b[MC\"\""
    );

    let mut utf8 = emulator(300, 3);
    utf8.feed(b"\x1b[?1000h\x1b[?1005h");
    assert_eq!(
        utf8.pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2_591.0,
            21.0,
            false,
        ))
        .unwrap()
        .bytes,
        b"\x1b[M \xc4\xa4\""
    );

    let mut sgr = emulator(10, 3);
    sgr.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        sgr.pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            11.0,
            21.0,
            false,
        ))
        .unwrap()
        .bytes,
        b"\x1b[<0;2;2M"
    );

    let mut urxvt = emulator(10, 3);
    urxvt.feed(b"\x1b[?1000h\x1b[?1015h");
    assert_eq!(
        urxvt
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
        b"\x1b[32;2;2M"
    );

    let mut sgr_pixels = emulator(10, 3);
    sgr_pixels.feed(b"\x1b[?1000h\x1b[?1016h");
    assert_eq!(
        sgr_pixels
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
        b"\x1b[<0;11;21M"
    );
}

#[test]
fn offscreen_drag_preserves_button_and_modifiers_at_clamped_boundaries() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?1002h\x1b[?1006h");
    _ = emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            1.0,
            1.0,
            false,
        ))
        .unwrap();

    let mut drag = pointer(
        PointerPhase::Motion,
        Some(PointerButton::Left),
        -10.0,
        100.0,
        false,
    );
    drag.modifiers = InputModifiers {
        shift: true,
        alt: true,
        control: true,
        ..InputModifiers::default()
    };
    let dragged = emulator.pointer(drag).unwrap();

    let mut release = pointer(
        PointerPhase::Release,
        Some(PointerButton::Left),
        200.0,
        -10.0,
        false,
    );
    release.modifiers = drag.modifiers;
    let released = emulator.pointer(release).unwrap();

    assert_eq!(dragged.bytes, b"\x1b[<60;1;2M");
    assert_eq!(released.bytes, b"\x1b[<28;10;1m");
}

fn pointer(
    phase: PointerPhase,
    button: Option<PointerButton>,
    x: f32,
    y: f32,
    shift: bool,
) -> PointerInput {
    PointerInput {
        generation: PresentationGeneration::default(),
        phase,
        button,
        position: SurfacePosition { x, y },
        modifiers: InputModifiers {
            shift,
            ..InputModifiers::default()
        },
        shift_selection: ShiftSelectionPolicy::default(),
    }
}

fn wheel(steps: i32, shift: bool) -> WheelInput {
    WheelInput {
        generation: PresentationGeneration::default(),
        horizontal_steps: 0,
        vertical_steps: steps,
        phase: WheelPhase::GestureChanged,
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers {
            shift,
            ..InputModifiers::default()
        },
        shift_selection: ShiftSelectionPolicy::default(),
    }
}

fn current_pointer(emulator: &TerminalEmulator, mut input: PointerInput) -> PointerInput {
    input.generation = emulator.presentation_generation;
    input
}

fn current_wheel(emulator: &TerminalEmulator, mut input: WheelInput) -> WheelInput {
    input.generation = emulator.presentation_generation;
    input
}

fn key(physical_key: PhysicalKey, modifiers: InputModifiers) -> KeyInput {
    KeyInput {
        action: KeyAction::Press,
        physical_key,
        native_key_code: None,
        logical_key: format!("{physical_key:?}"),
        text: None,
        unshifted_codepoint: None,
        modifiers,
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    }
}

fn text_key(
    physical_key: PhysicalKey,
    text: &str,
    unshifted_codepoint: char,
    action: KeyAction,
    modifiers: InputModifiers,
) -> KeyInput {
    KeyInput {
        action,
        physical_key,
        native_key_code: None,
        logical_key: text.to_owned(),
        text: Some(text.to_owned()),
        unshifted_codepoint: Some(unshifted_codepoint),
        modifiers,
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    }
}

fn select_first_five(emulator: &mut TerminalEmulator, shift: bool) {
    emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            shift,
        ))
        .unwrap();
    emulator
        .pointer(pointer(PointerPhase::Motion, None, 48.0, 10.0, false))
        .unwrap();
    emulator
        .pointer(pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            48.0,
            10.0,
            false,
        ))
        .unwrap();
    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
}

fn select_all(emulator: &mut TerminalEmulator) {
    let selection = emulator.terminal.select_all().unwrap().unwrap();
    emulator.terminal.set_selection(Some(&selection)).unwrap();
}

fn row_text(snapshot: &ScreenSnapshot, row: usize) -> String {
    snapshot.rows[row]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect()
}

#[test]
fn vt_sequences_update_the_screen_without_leaking_escape_bytes() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello\r\n\x1b[31mred\x1b[0m");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let first_row = snapshot.rows[0]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();
    let second_row = snapshot.rows[1]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();

    assert!(first_row.starts_with("hello"));
    assert!(second_row.starts_with("red"));
    assert!(!first_row.contains('\x1b'));
    assert_eq!(
        snapshot.rows[1][0].foreground_source,
        TerminalColor::Palette(1)
    );
}

#[test]
fn snapshots_preserve_foreground_color_sources() {
    let mut emulator = emulator(8, 1);
    emulator.feed(b"d\x1b[31ma\x1b[38;5;200mi\x1b[38;2;1;2;3mr");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let sources = snapshot.rows[0][..4]
        .iter()
        .map(|cell| cell.foreground_source)
        .collect::<Vec<_>>();

    assert_eq!(
        sources,
        vec![
            TerminalColor::Default,
            TerminalColor::Palette(1),
            TerminalColor::Palette(200),
            TerminalColor::Rgb(Color::from_rgb_components(1, 2, 3)),
        ]
    );
}

#[test]
fn snapshots_preserve_background_color_sources() {
    let mut emulator = emulator(8, 1);
    emulator.feed(b"d\x1b[41ma\x1b[48;5;200mi\x1b[48;2;4;5;6mr");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let sources = snapshot.rows[0][..4]
        .iter()
        .map(|cell| cell.background_source)
        .collect::<Vec<_>>();

    assert_eq!(
        sources,
        vec![
            TerminalColor::Default,
            TerminalColor::Palette(1),
            TerminalColor::Palette(200),
            TerminalColor::Rgb(Color::from_rgb_components(4, 5, 6)),
        ]
    );
}

#[test]
fn snapshots_preserve_inverse_and_terminal_reverse_semantics() {
    let mut emulator = emulator(4, 1);
    emulator.feed(b"\x1b[7mx\x1b[27m\x1b[?5h");

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(snapshot.rows[0][0].inverse);
    assert!(snapshot.colors.reversed);
    assert_eq!(snapshot.background, snapshot.colors.foreground);
    assert_eq!(
        snapshot.colors.palette[1],
        ACTIVE_THEME.terminal_normal()[1]
    );
    assert_eq!(
        snapshot.colors.palette[9],
        ACTIVE_THEME.terminal_bright()[1]
    );
}

#[test]
fn snapshots_preserve_text_presentation_attributes_and_invisible_content() {
    let mut emulator = emulator(8, 1);
    emulator.feed(b"\x1b[1;2;3;5;7;8msecret\x1b[0m");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let cell = &snapshot.rows[0][0];

    assert_eq!(cell.text, "s");
    assert!(cell.bold);
    assert!(cell.faint);
    assert!(cell.italic);
    assert!(cell.blinking);
    assert!(cell.inverse);
    assert!(cell.invisible);
}

#[test]
fn snapshot_reports_text_blink_demand_only_for_visible_content() {
    let mut visible = emulator(2, 1);
    visible.feed(b"\x1b[5mA");
    let visible = visible.snapshot().unwrap().unwrap();

    let mut invisible = emulator(2, 1);
    invisible.feed(b"\x1b[5;8mA");
    let invisible = invisible.snapshot().unwrap().unwrap();

    assert!(visible.text_blinking);
    assert!(!invisible.text_blinking);
}

#[test]
fn snapshots_preserve_text_decorations_and_independent_underline_color() {
    let mut emulator = emulator(8, 1);
    emulator.feed(b"\x1b[58;5;200;4:1mS\x1b[4:2mD\x1b[4:3mC\x1b[4:4mO\x1b[4:5mH\x1b[9;53mX");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let cells = &snapshot.rows[0];

    assert_eq!(cells[0].underline, TerminalUnderlineSnapshot::Single);
    assert_eq!(cells[1].underline, TerminalUnderlineSnapshot::Double);
    assert_eq!(cells[2].underline, TerminalUnderlineSnapshot::Curly);
    assert_eq!(cells[3].underline, TerminalUnderlineSnapshot::Dotted);
    assert_eq!(cells[4].underline, TerminalUnderlineSnapshot::Dashed);
    assert_eq!(cells[0].underline_source, TerminalColor::Palette(200));
    assert!(cells[5].strikethrough);
    assert!(cells[5].overline);
}

#[test]
fn erased_cells_preserve_explicit_background_sources() {
    let mut emulator = emulator(4, 1);
    emulator.feed(b"\x1b[41m\x1b[2K");

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot.rows[0]
            .iter()
            .all(|cell| cell.background_source == TerminalColor::Palette(1))
    );
}

#[test]
fn title_only_osc_sequence_publishes_a_screen_snapshot() {
    let mut emulator = emulator(12, 3);
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"\x1b]2;Claude Code\x07");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.title.as_ref(), "Claude Code");
    assert!(
        first
            .rows
            .iter()
            .zip(snapshot.rows.iter())
            .all(|(first, second)| Arc::ptr_eq(first, second))
    );
    assert_eq!(
        snapshot.damage,
        SnapshotDamage {
            title: true,
            metadata: true,
            ..SnapshotDamage::default()
        }
    );
}

#[test]
fn latest_osc_title_replaces_the_previous_snapshot_title() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"\x1b]2;zsh\x07");
    let _ = emulator.snapshot().unwrap();

    emulator.feed(b"\x1b]2;cargo test\x07");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.title.as_ref(), "cargo test");
}

#[test]
fn metadata_only_output_publishes_owned_provenance_without_rebuilding_rows() {
    let geometry = geometry(12, 3, 10.0, 20.0);
    let epoch = Instant::now();
    let mut emulator = TerminalEmulator::new_with_metadata(
        geometry,
        "/Users/me",
        "zsh",
        Some("mac.local"),
        identity::TERM_FALLBACK,
        epoch,
    )
    .unwrap();
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed_at(
        b"\x1b]7;file://mac.local/Users/me/Project\x07\x1b]9;4;3\x07",
        epoch + Duration::from_secs(1),
    );
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.title.as_ref(), "Project");
    assert_eq!(
        snapshot.metadata.directory.path.as_ref(),
        "/Users/me/Project"
    );
    assert_eq!(
        snapshot.metadata.directory.provenance,
        DirectoryProvenance::Osc7
    );
    assert_eq!(snapshot.metadata.progress, ProgressMetadata::Indeterminate);
    assert!(snapshot.damage.metadata);
    assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Clean);
    assert!(
        first
            .rows
            .iter()
            .zip(snapshot.rows.iter())
            .all(|(first, second)| Arc::ptr_eq(first, second))
    );
}

#[test]
fn semantic_prompt_zones_and_command_state_are_owned_by_the_snapshot() {
    let geometry = geometry(16, 2, 10.0, 20.0);
    let epoch = Instant::now();
    let mut emulator = TerminalEmulator::new_with_metadata(
        geometry,
        "/tmp",
        "zsh",
        None,
        identity::TERM_FALLBACK,
        epoch,
    )
    .unwrap();
    emulator.feed_at(
        b"\x1b]133;A\x07$ \x1b]133;B\x07echo\x1b]133;C;cmdline=echo\x07out",
        epoch + Duration::from_secs(1),
    );

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot.rows[0][0..2]
            .iter()
            .all(|cell| cell.semantic_content == CellSemanticSnapshot::Prompt)
    );
    assert!(
        snapshot.rows[0][2..6]
            .iter()
            .all(|cell| cell.semantic_content == CellSemanticSnapshot::Input)
    );
    assert!(
        snapshot.rows[0][6..9]
            .iter()
            .all(|cell| cell.semantic_content == CellSemanticSnapshot::Output)
    );
    assert_eq!(
        snapshot.metadata.prompt_zone,
        crate::terminal::metadata::PromptZone::CommandOutput
    );
    assert_eq!(
        snapshot
            .metadata
            .command
            .as_ref()
            .map(|command| command.line.as_ref()),
        Some("echo")
    );
}

#[test]
fn resize_changes_the_visible_grid() {
    let mut emulator = emulator(10, 2);
    let _ = emulator.snapshot().unwrap();
    emulator.resize(geometry(20, 4, 8.0, 18.0)).unwrap();

    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(
        (
            snapshot.size,
            snapshot.viewport.visible_rows,
            snapshot.rows.len(),
            snapshot.rows.iter().all(|row| row.len() == 20),
        ),
        (ScreenSizeSnapshot { cols: 20, rows: 4 }, 4, 4, true,)
    );
    assert!(snapshot.damage.resize);
    assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Full);
    assert!(!snapshot.damage.title);
    assert!(!snapshot.damage.active_screen);
}

#[test]
fn grid_resize_reflows_logical_content_and_advances_the_presentation() {
    let mut emulator = emulator(8, 3);
    emulator.feed(b"abcdefghijkl");
    let before = emulator.snapshot().unwrap().unwrap();

    emulator.resize(geometry(5, 3, 8.0, 18.0)).unwrap();
    let after = emulator.snapshot().unwrap().unwrap();

    assert!(after.generation > before.generation);
    assert!(row_text(&after, 0).starts_with("abcde"));
    assert!(row_text(&after, 1).starts_with("fghij"));
    assert!(row_text(&after, 2).starts_with("kl"));
    assert!(after.damage.resize);
    assert_eq!(after.damage.content, ContentDamageSnapshot::Full);
}

#[test]
fn grid_resize_preserves_selection_anchors_across_reflow() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world");
    let _ = emulator.snapshot().unwrap().unwrap();
    let press = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            false,
        ),
    );
    emulator.pointer(press).unwrap();
    let drag = current_pointer(
        &emulator,
        pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
    );
    emulator.pointer(drag).unwrap();
    let release = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            48.0,
            10.0,
            false,
        ),
    );
    emulator.pointer(release).unwrap();
    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));

    emulator.resize(geometry(6, 3, 8.0, 18.0)).unwrap();
    let reflowed = emulator.snapshot().unwrap().unwrap();

    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    assert!(
        reflowed
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .take(5)
            .all(|cell| cell.selected)
    );
}

#[test]
fn selection_stays_anchored_when_output_moves_scrollback_pages() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"one\r\ntwo\r\nthree\r\nfour");
    _ = emulator.snapshot().unwrap().unwrap();
    emulator.scroll_to(0);
    let top = emulator.snapshot().unwrap().unwrap();
    let input = |phase, x| PointerInput {
        generation: top.generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position: SurfacePosition { x, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    _ = emulator.pointer(input(PointerPhase::Press, 1.0)).unwrap();
    _ = emulator.pointer(input(PointerPhase::Motion, 28.0)).unwrap();
    _ = emulator
        .pointer(input(PointerPhase::Release, 28.0))
        .unwrap();
    assert_eq!(emulator.selection_text().unwrap().as_deref(), Some("one"));

    emulator.feed(b"\r\nfive");
    let moved = emulator.snapshot().unwrap().unwrap();

    assert!(row_text(&moved, 0).starts_with("one"));
    assert!(moved.rows[0][..3].iter().all(|cell| cell.selected));
    assert_eq!(emulator.selection_text().unwrap().as_deref(), Some("one"));
}

#[test]
fn pixel_only_resize_updates_backing_geometry_without_a_grid_presentation() {
    let mut emulator = emulator(10, 2);
    let before = emulator.snapshot().unwrap().unwrap();

    emulator.resize(geometry(10, 2, 9.0, 20.0)).unwrap();

    assert!(emulator.snapshot().unwrap().is_none());
    assert_eq!(emulator.presentation_generation, before.generation);
    assert_eq!(
        emulator.mouse_encoder_size(),
        MouseEncoderSize {
            screen_width: 90,
            screen_height: 40,
            cell_width: 9,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    );
}

#[test]
fn resize_releases_synchronized_output_without_spurious_grid_damage() {
    let mut emulator = emulator(10, 2);
    let _ = emulator.snapshot().unwrap().unwrap();
    emulator.feed(b"\x1b[?2026hpending");
    assert!(emulator.snapshot().unwrap().is_none());

    emulator.resize(geometry(10, 2, 9.0, 20.0)).unwrap();
    let released = emulator.snapshot().unwrap().unwrap();

    assert!(row_text(&released, 0).starts_with("pending"));
    assert!(!released.damage.resize);
}

#[test]
fn alternate_screen_transition_reports_only_affected_metadata_and_content() {
    let mut emulator = emulator(10, 2);
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"\x1b[?1049h");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.active_screen, ActiveScreenSnapshot::Alternate);
    assert!(
        first
            .rows
            .iter()
            .zip(snapshot.rows.iter())
            .all(|(first, second)| Arc::ptr_eq(first, second))
    );
    assert_eq!(
        snapshot.damage,
        SnapshotDamage {
            active_screen: true,
            ..SnapshotDamage::default()
        }
    );
}

#[test]
fn alternate_screen_exit_restores_the_primary_viewport_and_cached_rows() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"one\r\ntwo\r\nthree");
    let _ = emulator.snapshot().unwrap().unwrap();
    let input = current_wheel(&emulator, wheel(1, false));
    let _ = emulator.wheel(input).unwrap();
    let primary = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&primary, 0).starts_with("one"));

    emulator.feed(b"\x1b[?1049halternate");
    let alternate = emulator.snapshot().unwrap().unwrap();
    assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);
    assert_eq!(
        alternate.scrollbar.total_rows,
        alternate.scrollbar.visible_rows
    );

    emulator.feed(b"\x1b[?1049l");
    let restored = emulator.snapshot().unwrap().unwrap();
    assert_eq!(restored.active_screen, ActiveScreenSnapshot::Primary);
    assert_eq!(restored.viewport, primary.viewport);
    assert!(row_text(&restored, 0).starts_with("one"));
    assert!(
        primary
            .rows
            .iter()
            .zip(restored.rows.iter())
            .all(|(before, after)| Arc::ptr_eq(before, after))
    );
}

#[test]
fn incremental_snapshots_match_a_full_snapshot_of_the_same_terminal_state() {
    let chunks: [&[u8]; 3] = [
        b"one\r\ntwo",
        b"\x1b[31m red\x1b[0m",
        b"\x1b]2;incremental build\x07",
    ];
    let mut incremental = emulator(16, 3);
    let mut incremental_snapshot = None;
    for chunk in chunks {
        incremental.feed(chunk);
        incremental_snapshot = incremental.snapshot().unwrap();
    }

    let mut full = emulator(16, 3);
    full.feed(b"one\r\ntwo\x1b[31m red\x1b[0m\x1b]2;incremental build\x07");

    let mut incremental_snapshot = (*incremental_snapshot.unwrap()).clone();
    let mut full_snapshot = (*full.snapshot().unwrap().unwrap()).clone();
    incremental_snapshot.damage = SnapshotDamage::default();
    full_snapshot.damage = SnapshotDamage::default();
    full_snapshot.generation = incremental_snapshot.generation;

    assert_eq!(incremental_snapshot, full_snapshot);
}

#[test]
fn resize_emits_in_band_size_responses() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?2048h");
    assert_eq!(emulator.take_pty_responses(), b"\x1b[48;2;10;40;100t");

    emulator.resize(geometry(20, 4, 8.0, 18.0)).unwrap();
    assert_eq!(emulator.take_pty_responses(), b"\x1b[48;4;20;72;160t");
}

#[test]
fn size_queries_report_current_engine_cells_while_preserving_fractional_ui_geometry() {
    let initial = geometry(57, 20, 21.6, 40.0);
    assert_eq!(initial.backing_grid_size().width, 1232);
    let mut emulator = TerminalEmulator::new(initial).unwrap();
    emulator.feed(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(
        emulator.take_pty_responses(),
        b"\x1b[4;800;1254t\x1b[6;40;22t\x1b[8;20;57t",
    );

    let resized = geometry(43, 12, 15.2, 31.1);
    emulator.resize(resized).unwrap();
    emulator.feed(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(
        emulator.take_pty_responses(),
        b"\x1b[4;384;688t\x1b[6;32;16t\x1b[8;12;43t",
    );
    assert_eq!(emulator.geometry, resized);
    assert_eq!(resized.backing_grid_size().width, 654);
}

#[test]
fn kitty_auto_sized_placeholder_rows_use_size_replies_without_extra_wrapping() {
    let _guard = crate::terminal::graphics::test_lock();
    let geometry = geometry(57, 20, 21.6, 40.0);
    let mut emulator = TerminalEmulator::new(geometry).unwrap();
    emulator.feed(b"\x1b[16t");
    let response = String::from_utf8(emulator.take_pty_responses()).unwrap();
    let reported_cell_width: u32 = response
        .strip_prefix("\x1b[6;")
        .unwrap()
        .strip_suffix('t')
        .unwrap()
        .split(';')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let pixel_width = geometry.backing_grid_size().width;
    let fallback_cell_width = pixel_width / u32::from(geometry.grid().cols);
    assert_eq!(pixel_width.div_ceil(fallback_cell_width), 59);
    let image_columns = pixel_width.div_ceil(reported_cell_width);
    assert_eq!(image_columns, 56);

    let mut stream = format!(
        "\x1b_Ga=T,t=d,f=24,i=1,U=1,s={image_columns},v=2,c={image_columns},r=2,q=2;{}\x1b\\",
        "AAAA".repeat(image_columns as usize * 2),
    );
    for row in ['\u{305}', '\u{30d}'] {
        stream.push_str(&format!("\x1b[38;5;1m\u{10eeee}{row}\u{305}"));
        stream.push_str(&"\u{10eeee}".repeat(image_columns as usize - 1));
        stream.push_str("\x1b[0m\r\n");
    }
    emulator.feed(stream.as_bytes());
    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(snapshot.cursor.position.unwrap().row, 2);
    assert_eq!(snapshot.graphics.placements.len(), 2);
    assert!(snapshot.graphics.placements.iter().all(|placement| {
        matches!(placement.viewport_row, 0 | 1)
            && placement.viewport_col >= 0
            && placement.viewport_col < i32::from(geometry.grid().cols)
            && placement.destination_width == image_columns * reported_cell_width
    }));
}

#[test]
fn runtime_identity_replies_are_spaceterm_owned_and_capability_bounded() {
    let mut emulator = emulator_with_terminal_name(10, 2, identity::TERM_NAME);

    emulator.feed(b"\x1b[>q\x1b[c\x1b[>c\x1bP+q544E;436F\x1b\\");
    let replies = emulator.take_pty_responses();

    assert!(
        replies
            .windows(identity::XTVERSION.len())
            .any(|window| window == identity::XTVERSION.as_bytes())
    );
    assert!(
        replies
            .windows(b"\x1b[?62;22;52c".len())
            .any(|window| window == b"\x1b[?62;22;52c")
    );
    assert!(
        replies
            .windows(b"\x1b[>1;0;0c".len())
            .any(|window| window == b"\x1b[>1;0;0c")
    );
    assert!(replies.windows(7).any(|window| window == b"1+r544E"));
    assert!(
        replies
            .windows(b"787465726D2D73706163657465726D".len())
            .any(|window| window == b"787465726D2D73706163657465726D")
    );
    assert!(replies.windows(7).any(|window| window == b"1+r436F"));
    assert!(
        !String::from_utf8_lossy(&replies)
            .to_ascii_lowercase()
            .contains("ghostty")
    );
}

#[test]
fn bell_and_command_completion_publish_typed_attention_once() {
    let epoch = Instant::now();
    let mut emulator = emulator(10, 2);

    emulator.feed_at(b"\x07\x1b]133;C;cmdline=true\x07", epoch);
    emulator.feed_at(b"\x1b]133;D;0\x07", epoch + Duration::from_secs(2));
    emulator.feed_at(b"\x1b]133;D;0\x07", epoch + Duration::from_secs(3));

    assert_eq!(
        emulator.take_attention_events(),
        vec![
            AttentionEvent::Bell,
            AttentionEvent::CommandFinished {
                exit_status: Some(0),
                duration: Duration::from_secs(2),
            },
        ]
    );
}

#[test]
fn key_encoding_tracks_cursor_mode_and_modifiers() {
    let mut emulator = emulator(10, 2);
    assert_eq!(
        emulator
            .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
            .unwrap()
            .bytes,
        b"\x1b[A"
    );

    emulator.feed(b"\x1b[?1h");
    assert_eq!(
        emulator
            .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
            .unwrap()
            .bytes,
        b"\x1bOA"
    );

    let printable = KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::E,
        native_key_code: None,
        logical_key: "é".to_owned(),
        text: Some("é".to_owned()),
        unshifted_codepoint: Some('e'),
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    };
    assert_eq!(emulator.key(printable).unwrap().bytes, "é".as_bytes());

    let control_c = KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::C,
        native_key_code: None,
        logical_key: "c".to_owned(),
        text: Some("c".to_owned()),
        unshifted_codepoint: Some('c'),
        modifiers: InputModifiers {
            control: true,
            ..InputModifiers::default()
        },
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    };
    assert_eq!(emulator.key(control_c).unwrap().bytes, b"\x03");

    let alt_x = KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::X,
        native_key_code: None,
        logical_key: "x".to_owned(),
        text: Some("x".to_owned()),
        unshifted_codepoint: Some('x'),
        modifiers: InputModifiers {
            alt: true,
            ..InputModifiers::default()
        },
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    };
    assert_eq!(emulator.key(alt_x).unwrap().bytes, b"\x1bx");
}

#[test]
fn input_method_commits_emit_exact_utf8_independent_of_keyboard_protocol() {
    let mut emulator = emulator(10, 2);

    assert_eq!(
        emulator
            .key(KeyInput::input_method_commit("日本語"))
            .unwrap()
            .bytes,
        "日本語".as_bytes()
    );

    emulator.feed(b"\x1b[>11u");
    assert_eq!(
        emulator
            .key(KeyInput::input_method_commit("👩\u{200d}💻"))
            .unwrap()
            .bytes,
        "👩\u{200d}💻".as_bytes()
    );
}

#[test]
fn named_keys_use_ghostty_key_encoding() {
    let mut emulator = emulator(10, 2);
    let cases: &[(PhysicalKey, InputModifiers, &[u8])] = &[
        (PhysicalKey::Enter, InputModifiers::default(), b"\r"),
        (PhysicalKey::Backspace, InputModifiers::default(), b"\x7f"),
        (PhysicalKey::Tab, InputModifiers::default(), b"\t"),
        (
            PhysicalKey::Tab,
            InputModifiers {
                shift: true,
                ..InputModifiers::default()
            },
            b"\x1b[Z",
        ),
        (PhysicalKey::Escape, InputModifiers::default(), b"\x1b"),
        (PhysicalKey::ArrowDown, InputModifiers::default(), b"\x1b[B"),
        (PhysicalKey::ArrowLeft, InputModifiers::default(), b"\x1b[D"),
        (
            PhysicalKey::ArrowRight,
            InputModifiers::default(),
            b"\x1b[C",
        ),
        (PhysicalKey::Home, InputModifiers::default(), b"\x1b[H"),
        (PhysicalKey::End, InputModifiers::default(), b"\x1b[F"),
        (PhysicalKey::PageUp, InputModifiers::default(), b"\x1b[5~"),
        (PhysicalKey::PageDown, InputModifiers::default(), b"\x1b[6~"),
        (PhysicalKey::Insert, InputModifiers::default(), b"\x1b[2~"),
        (PhysicalKey::Delete, InputModifiers::default(), b"\x1b[3~"),
    ];

    for (code, modifiers, expected) in cases {
        assert_eq!(
            emulator.key(key(*code, *modifiers)).unwrap().bytes,
            *expected,
            "unexpected encoding for {code:?}"
        );
    }
}

#[test]
fn application_keypad_mode_changes_the_next_numpad_key_immediately() {
    let mut emulator = emulator(10, 2);
    let numpad_one = || {
        text_key(
            PhysicalKey::Numpad1,
            "1",
            '1',
            KeyAction::Press,
            InputModifiers::default(),
        )
    };

    assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"1");
    emulator.feed(b"\x1b[?1035l");
    emulator.feed(b"\x1b[?66h");
    assert!(emulator.terminal.mode(Mode::KEYPAD_KEYS).unwrap());
    assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"\x1bOq");
    emulator.feed(b"\x1b[?66l");
    assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"1");
}

#[test]
fn modify_other_keys_mode_changes_the_next_modified_text_key_immediately() {
    let mut emulator = emulator(10, 2);
    let alt_eight = || {
        text_key(
            PhysicalKey::Digit8,
            "8",
            '8',
            KeyAction::Press,
            InputModifiers {
                alt: true,
                ..InputModifiers::default()
            },
        )
    };

    assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b8");
    emulator.feed(b"\x1b[>4;2m");
    assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b[27;3;56~");
    emulator.feed(b"\x1b[>4;0m");
    assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b8");
}

#[test]
fn fixterms_disambiguates_control_keys_that_overlap_legacy_bytes() {
    let mut emulator = emulator(10, 2);
    let cases = [
        (PhysicalKey::I, "i", 'i', false, b"\x1b[105;5u".as_slice()),
        (PhysicalKey::M, "m", 'm', false, b"\x1b[109;5u".as_slice()),
        (
            PhysicalKey::BracketLeft,
            "[",
            '[',
            false,
            b"\x1b[91;5u".as_slice(),
        ),
        (PhysicalKey::M, "M", 'm', true, b"\x1b[109;6u".as_slice()),
    ];

    for (physical_key, text, unshifted, shift, expected) in cases {
        let input = text_key(
            physical_key,
            text,
            unshifted,
            KeyAction::Press,
            InputModifiers {
                shift,
                control: true,
                ..InputModifiers::default()
            },
        );
        assert_eq!(emulator.key(input).unwrap().bytes, expected);
    }
}

#[test]
fn kitty_report_events_encodes_repeats_and_releases_but_legacy_does_not() {
    let mut emulator = emulator(10, 2);
    let action = |action| text_key(PhysicalKey::A, "a", 'a', action, InputModifiers::default());

    assert!(
        emulator
            .key(action(KeyAction::Release))
            .unwrap()
            .bytes
            .is_empty()
    );
    emulator.feed(b"\x1b[>11u");
    assert_eq!(
        emulator.key(action(KeyAction::Repeat)).unwrap().bytes,
        b"\x1b[97;1:2u"
    );
    assert_eq!(
        emulator.key(action(KeyAction::Release)).unwrap().bytes,
        b"\x1b[97;1:3u"
    );
    emulator.feed(b"\x1b[<u");
    assert!(
        emulator
            .key(action(KeyAction::Release))
            .unwrap()
            .bytes
            .is_empty()
    );
}

#[test]
fn dec_backarrow_mode_changes_backspace_policy_immediately() {
    let mut emulator = emulator(10, 2);
    let backspace = || key(PhysicalKey::Backspace, InputModifiers::default());

    assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x7f");
    emulator.feed(b"\x1b[?67h");
    assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x08");
    emulator.feed(b"\x1b[?67l");
    assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x7f");
}

#[test]
fn conventional_application_shortcuts_keep_legacy_compatibility_bytes() {
    let mut emulator = emulator(10, 2);
    let cases = [
        (PhysicalKey::C, "c", b"\x03".as_slice(), "shell interrupt"),
        (PhysicalKey::B, "b", b"\x02".as_slice(), "tmux prefix"),
        (PhysicalKey::R, "r", b"\x12".as_slice(), "fzf history"),
    ];

    for (physical_key, text, expected, fixture) in cases {
        let input = text_key(
            physical_key,
            text,
            text.chars().next().unwrap(),
            KeyAction::Press,
            InputModifiers {
                control: true,
                ..InputModifiers::default()
            },
        );
        assert_eq!(emulator.key(input).unwrap().bytes, expected, "{fixture}");
    }

    emulator.feed(b"\x1b[?1h");
    assert_eq!(
        emulator
            .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
            .unwrap()
            .bytes,
        b"\x1bOA",
        "Vim/Neovim application cursor"
    );
}

#[test]
fn function_key_byte_tables_cover_legacy_and_extended_kitty_ranges() {
    let mut emulator = emulator(10, 2);
    let legacy_cases: &[(PhysicalKey, &[u8])] = &[
        (PhysicalKey::F1, b"\x1bOP"),
        (PhysicalKey::F2, b"\x1bOQ"),
        (PhysicalKey::F3, b"\x1bOR"),
        (PhysicalKey::F4, b"\x1bOS"),
        (PhysicalKey::F5, b"\x1b[15~"),
        (PhysicalKey::F6, b"\x1b[17~"),
        (PhysicalKey::F7, b"\x1b[18~"),
        (PhysicalKey::F8, b"\x1b[19~"),
        (PhysicalKey::F9, b"\x1b[20~"),
        (PhysicalKey::F10, b"\x1b[21~"),
        (PhysicalKey::F11, b"\x1b[23~"),
        (PhysicalKey::F12, b"\x1b[24~"),
    ];
    for (physical_key, expected) in legacy_cases {
        assert_eq!(
            emulator
                .key(key(*physical_key, InputModifiers::default()))
                .unwrap()
                .bytes,
            *expected,
            "{physical_key:?}"
        );
    }

    let extended = [
        PhysicalKey::F13,
        PhysicalKey::F14,
        PhysicalKey::F15,
        PhysicalKey::F16,
        PhysicalKey::F17,
        PhysicalKey::F18,
        PhysicalKey::F19,
        PhysicalKey::F20,
        PhysicalKey::F21,
        PhysicalKey::F22,
        PhysicalKey::F23,
        PhysicalKey::F24,
        PhysicalKey::F25,
    ];
    for (physical_key, code) in extended
        .into_iter()
        .zip([25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45, 46])
    {
        assert_eq!(
            emulator
                .key(key(physical_key, InputModifiers::default()))
                .unwrap()
                .bytes,
            format!("\x1b[{code}~").into_bytes(),
            "{physical_key:?}"
        );
    }

    emulator.feed(b"\x1b[>9u");
    for (offset, physical_key) in extended.into_iter().enumerate() {
        let expected = format!("\x1b[{}u", 57_376 + offset).into_bytes();
        assert_eq!(
            emulator
                .key(key(physical_key, InputModifiers::default()))
                .unwrap()
                .bytes,
            expected,
            "{physical_key:?}"
        );
    }
}

#[test]
fn option_as_alt_policy_matrix_applies_to_both_modifiers() {
    let mut emulator = emulator(10, 2);
    let cases: &[(OptionAsAltPolicy, bool, &[u8])] = &[
        (OptionAsAltPolicy::None, false, b"["),
        (OptionAsAltPolicy::Both, false, b"\x1b["),
        (OptionAsAltPolicy::None, true, b"["),
        (OptionAsAltPolicy::Both, true, b"\x1b["),
    ];

    for (policy, alt_right, expected) in cases {
        let mut input = text_key(
            PhysicalKey::Digit8,
            "[",
            '8',
            KeyAction::Press,
            InputModifiers {
                alt: true,
                alt_right: *alt_right,
                ..InputModifiers::default()
            },
        );
        let policy_applies = match policy {
            OptionAsAltPolicy::None => false,
            OptionAsAltPolicy::Both => true,
        };
        input.consumed_modifiers = InputModifiers {
            alt: !policy_applies,
            alt_right: *alt_right && !policy_applies,
            ..InputModifiers::default()
        };
        input.option_as_alt = *policy;
        assert_eq!(
            emulator.key(input).unwrap().bytes,
            *expected,
            "{policy:?}, right={alt_right}"
        );
    }
}

#[test]
fn clean_screens_do_not_publish_another_snapshot() {
    let mut emulator = emulator(10, 2);

    assert!(emulator.snapshot().unwrap().is_some());
    assert!(emulator.snapshot().unwrap().is_none());
}

#[test]
fn synchronized_output_should_publish_only_the_completed_transaction() {
    let mut emulator = emulator(16, 2);
    let _ = emulator.snapshot().unwrap();

    emulator.feed(b"\x1b[?2026hpartial");
    assert!(emulator.snapshot().unwrap().is_none());

    emulator.feed(b" complete\x1b[?2026l");
    let completed = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&completed, 0).starts_with("partial complete"));
}

#[test]
fn synchronized_output_deadline_should_release_a_stalled_transaction() {
    let mut emulator = emulator(16, 2);
    let _ = emulator.snapshot().unwrap();
    let started = Instant::now();
    emulator.feed_at(b"\x1b[?2026hstalled", started);
    assert!(emulator.snapshot().unwrap().is_none());

    assert!(
        !emulator
            .expire_synchronized_output(started + Duration::from_millis(999))
            .unwrap()
    );
    assert!(emulator.snapshot().unwrap().is_none());
    assert!(
        emulator
            .expire_synchronized_output(started + Duration::from_secs(1))
            .unwrap()
    );
    let released = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&released, 0).starts_with("stalled"));
}

#[test]
fn synchronized_output_deadline_should_follow_the_last_output_activity() {
    let mut emulator = emulator(32, 2);
    let _ = emulator.snapshot().unwrap();
    let started = Instant::now();
    emulator.feed_at(b"\x1b[?2026hlong", started);
    assert!(emulator.snapshot().unwrap().is_none());

    let progressed = started + Duration::from_millis(900);
    emulator.feed_at(b" remote redraw", progressed);
    assert!(emulator.snapshot().unwrap().is_none());
    assert!(
        !emulator
            .expire_synchronized_output(started + Duration::from_secs(1))
            .unwrap(),
        "active synchronized output must not expose an intermediate grid"
    );

    assert!(
        emulator
            .expire_synchronized_output(progressed + Duration::from_secs(1))
            .unwrap(),
        "one second without output must still release a stalled producer"
    );
    let released = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&released, 0).starts_with("long remote redraw"));
}

#[test]
fn unchanged_rows_reuse_their_cell_storage() {
    let mut emulator = emulator(10, 3);
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"x");
    let second = emulator.snapshot().unwrap().unwrap();

    assert!(!Arc::ptr_eq(&first.rows[0], &second.rows[0]));
    assert!(Arc::ptr_eq(&first.rows[1], &second.rows[1]));
    assert!(Arc::ptr_eq(&first.rows[2], &second.rows[2]));
    assert!(Arc::ptr_eq(&first.title, &second.title));
}

#[test]
fn row_dirty_update_reports_only_the_changed_row() {
    let mut emulator = emulator(10, 3);
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"x\x1b[1D");
    let second = emulator.snapshot().unwrap().unwrap();

    assert!(!Arc::ptr_eq(&first.rows[0], &second.rows[0]));
    assert!(Arc::ptr_eq(&first.rows[1], &second.rows[1]));
    assert!(Arc::ptr_eq(&first.rows[2], &second.rows[2]));
    assert_eq!(
        second.damage,
        SnapshotDamage {
            content: ContentDamageSnapshot::Rows(Arc::from([0])),
            ..SnapshotDamage::default()
        }
    );
}

#[test]
fn cursor_only_changes_reuse_every_row_and_report_cursor_damage() {
    let mut emulator = emulator(10, 3);
    emulator.feed(b"abc");
    let first = emulator.snapshot().unwrap().unwrap();

    emulator.feed(b"\x1b[1D");
    let second = emulator.snapshot().unwrap().unwrap();

    assert!(
        first
            .rows
            .iter()
            .zip(second.rows.iter())
            .all(|(first, second)| Arc::ptr_eq(first, second))
    );
    assert_eq!(second.damage, SnapshotDamage::cursor(0));
}

#[test]
fn cursor_snapshot_preserves_position_visibility_style_blink_and_color() {
    let mut emulator = emulator(10, 3);
    emulator.feed(b"\x1b[3;4H\x1b[6 q\x1b]12;#112233\x07");

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(
        snapshot.cursor,
        CursorSnapshot {
            position: Some(CursorPositionSnapshot {
                column: 3,
                row: 2,
                width_cells: 1,
            }),
            visible: true,
            blinking: false,
            password_input: false,
            shape: CursorShapeSnapshot::Bar,
            color: Color::rgb(0x11_22_33),
            text_color: snapshot.colors.background,
        }
    );
}

#[test]
fn cursor_on_a_wide_tail_normalizes_to_the_full_grapheme() {
    let mut emulator = emulator(6, 2);
    emulator.feed("界\x1b[1D".as_bytes());

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(
        snapshot.cursor.position,
        Some(CursorPositionSnapshot {
            column: 0,
            row: 0,
            width_cells: 2,
        })
    );
}

#[test]
fn cursor_snapshot_tracks_blink_requests_and_hidden_state() {
    let mut emulator = emulator(6, 2);
    emulator.feed(b"\x1b[5 q");
    let blinking = emulator.snapshot().unwrap().unwrap();
    assert_eq!(blinking.cursor.shape, CursorShapeSnapshot::Bar);
    assert!(blinking.cursor.blinking);
    assert!(blinking.cursor.visible);

    emulator.feed(b"\x1b[?25l");
    let hidden = emulator.snapshot().unwrap().unwrap();
    assert!(!hidden.cursor.visible);
}

#[test]
fn cursor_movement_damages_only_the_old_and_new_rows() {
    let mut emulator = emulator(6, 3);
    let _ = emulator.snapshot().unwrap().unwrap();
    emulator.feed(b"\x1b[3;1H");

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(
        snapshot.damage.cursor,
        ContentDamageSnapshot::Rows(Arc::from([0, 2]))
    );
    assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Clean);
}

#[test]
fn selection_is_rendered_and_formats_as_plain_text() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world");
    _ = emulator.snapshot().unwrap();
    assert_eq!(emulator.selection_text().unwrap(), None);

    emulator
        .pointer(current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ),
        ))
        .unwrap();
    emulator
        .pointer(current_pointer(
            &emulator,
            pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
        ))
        .unwrap();
    emulator
        .pointer(current_pointer(
            &emulator,
            pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                48.0,
                10.0,
                false,
            ),
        ))
        .unwrap();

    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
    assert!(!snapshot.rows[0][5].selected);
    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
}

#[test]
fn snapshot_preserves_selection_presence_when_selected_content_is_offscreen() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    let presented = emulator.snapshot().unwrap().unwrap();

    let mut pointer = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            21.0,
            false,
        ),
    );
    emulator.pointer(pointer).unwrap();
    pointer.phase = PointerPhase::Motion;
    pointer.button = None;
    pointer.position.x = 48.0;
    emulator.pointer(pointer).unwrap();
    pointer.phase = PointerPhase::Release;
    pointer.button = Some(PointerButton::Left);
    emulator.pointer(pointer).unwrap();
    let selected = emulator.snapshot().unwrap().unwrap();
    assert!(selected.selection_present);

    let action = emulator.scroll_to_at(0, selected.generation);
    assert!(action.screen_changed);
    let scrolled = emulator.snapshot().unwrap().unwrap();

    assert!(scrolled.selection_present);
    assert!(
        scrolled
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .all(|cell| !cell.selected)
    );
    assert!(scrolled.generation > presented.generation);

    emulator.clear_selection().unwrap();
    let cleared = emulator.snapshot().unwrap().unwrap();
    assert!(!cleared.selection_present);
    assert!(cleared.damage.selection_presence);
}

#[test]
fn snapshot_selection_presence_reflects_screen_switch_clearing() {
    let mut emulator = emulator(12, 2);
    emulator.feed(b"hello world");
    select_first_five(&mut emulator, false);

    let primary = emulator.snapshot().unwrap().unwrap();
    assert_eq!(primary.active_screen, ActiveScreenSnapshot::Primary);
    assert!(primary.selection_present);

    emulator.feed(b"\x1b[?1049h");
    let alternate = emulator.snapshot().unwrap().unwrap();
    assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);
    assert!(!alternate.selection_present);
    assert!(
        alternate
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .all(|cell| !cell.selected)
    );
    assert!(alternate.damage.selection_presence);

    emulator.feed(b"\x1b[?1049l");
    let restored = emulator.snapshot().unwrap().unwrap();
    assert_eq!(restored.active_screen, ActiveScreenSnapshot::Primary);
    assert!(!restored.selection_present);
    assert!(!restored.damage.selection_presence);
    assert!(
        restored
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .all(|cell| !cell.selected)
    );
}

#[test]
fn accessibility_selection_requires_the_current_semantic_snapshot() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello");
    let (model, more) = emulator.accessibility_snapshot(true).unwrap();
    assert!(more);
    assert!(model.is_none());
    let (model, more) = emulator.accessibility_snapshot(false).unwrap();
    assert!(!more);
    let model = model.unwrap();
    _ = emulator.snapshot().unwrap();
    let request = model.selection_request(1..4).unwrap();

    emulator
        .set_accessibility_selection(AccessibilitySelectionRequest {
            generation: PresentationGeneration::default(),
            ..request.clone()
        })
        .unwrap();
    assert_eq!(emulator.selection_text().unwrap(), None);

    emulator.set_accessibility_selection(request).unwrap();
    assert_eq!(emulator.selection_text().unwrap(), Some("ell".to_owned()));
}

#[test]
fn accessibility_selection_rejects_synchronized_output_and_resets_pointer_invalidation() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello");
    let (model, more) = emulator.accessibility_snapshot(true).unwrap();
    assert!(more);
    assert!(model.is_none());
    let (model, more) = emulator.accessibility_snapshot(false).unwrap();
    assert!(!more);
    let model = model.unwrap();
    _ = emulator.snapshot().unwrap();
    let request = model.selection_request(1..4).unwrap();

    emulator.feed(b"\x1b[?2026hhidden");
    let action = emulator
        .set_accessibility_selection(request.clone())
        .unwrap();
    assert!(action.bytes.is_empty());
    assert!(!action.screen_changed);
    assert_eq!(emulator.selection_text().unwrap(), None);

    emulator.feed(b"\x1b[?2026l");
    let (model, more) = emulator.accessibility_snapshot(true).unwrap();
    assert!(!more);
    let model = model.unwrap();
    _ = emulator.snapshot().unwrap();
    emulator.pointer_mapping_invalidated = true;
    let request = model.selection_request(1..4).unwrap();
    emulator.set_accessibility_selection(request).unwrap();

    assert!(!emulator.pointer_mapping_invalidated);
}

#[test]
fn modifier_key_transitions_should_preserve_selection_for_application_shortcuts() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world");
    select_first_five(&mut emulator, false);
    let mut meta = key(
        PhysicalKey::MetaLeft,
        InputModifiers {
            platform: true,
            ..InputModifiers::default()
        },
    );

    emulator.key(meta.clone()).unwrap();
    meta.action = KeyAction::Release;
    meta.modifiers.platform = false;
    emulator.key(meta).unwrap();

    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
}

#[test]
fn non_modifier_key_input_should_clear_selection() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world");
    select_first_five(&mut emulator, false);

    emulator
        .key(key(PhysicalKey::A, InputModifiers::default()))
        .unwrap();

    assert_eq!(emulator.selection_text().unwrap(), None);
}

#[test]
fn non_modifier_key_release_preserves_selection() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world");
    select_first_five(&mut emulator, false);
    let mut released = key(PhysicalKey::A, InputModifiers::default());
    released.action = KeyAction::Release;

    emulator.key(released).unwrap();

    assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    emulator
        .key(key(PhysicalKey::A, InputModifiers::default()))
        .unwrap();
    assert_eq!(emulator.selection_text().unwrap(), None);
}

#[test]
fn selection_copy_distinguishes_soft_wraps_and_hard_lines() {
    let mut emulator = emulator(5, 3);
    emulator.feed(b"abcdefgh\r\nxy");
    select_all(&mut emulator);

    let copy = emulator
        .selection_copy(SelectionCopyOptions::default())
        .unwrap()
        .unwrap();

    assert_eq!(copy.plain_text, "abcdefgh\nxy");
    let html = copy.html.unwrap();
    assert!(html.contains("abcdefgh"));
    assert!(html.contains("xy"));
    assert!(!html.contains("CellSnapshot"));
}

#[test]
fn selection_copy_wide_cells_and_trailing_space_policy_are_deterministic() {
    let mut emulator = emulator(6, 2);
    emulator.feed("😀  \r\nx".as_bytes());
    select_all(&mut emulator);

    let trimmed = emulator
        .selection_copy(SelectionCopyOptions::default())
        .unwrap()
        .unwrap();
    let preserved = emulator
        .selection_copy(SelectionCopyOptions {
            trailing_spaces: TrailingSpacePolicy::Preserve,
            include_html: false,
            ..SelectionCopyOptions::default()
        })
        .unwrap()
        .unwrap();

    assert_eq!(trimmed.plain_text.matches('😀').count(), 1);
    assert_eq!(trimmed.plain_text, "😀\nx");
    assert!(preserved.plain_text.starts_with("😀  \n"));
    assert_eq!(preserved.plain_text.matches('😀').count(), 1);
    assert_eq!(preserved.html, None);
}

#[test]
fn repeat_clicks_select_cells_words_and_lines_with_injected_time() {
    let mut emulator = emulator(12, 2);
    emulator.feed(b"alpha beta");

    for (time, expected) in [
        (Duration::ZERO, None),
        (Duration::from_millis(100), Some("beta")),
        (Duration::from_millis(200), Some("alpha beta")),
    ] {
        emulator.set_gesture_time_for_test(time);
        _ = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                61.0,
                1.0,
                false,
            ))
            .unwrap();
        _ = emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                61.0,
                1.0,
                false,
            ))
            .unwrap();

        assert_eq!(emulator.selection_text().unwrap().as_deref(), expected);
    }
}

#[test]
fn wide_tail_and_soft_wrapped_word_select_complete_graphemes() {
    for x in [11.0, 21.0] {
        let mut wide = emulator(8, 2);
        wide.feed("A😀B".as_bytes());
        for time in [Duration::ZERO, Duration::from_millis(100)] {
            wide.set_gesture_time_for_test(time);
            _ = wide
                .pointer(pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    x,
                    1.0,
                    false,
                ))
                .unwrap();
            _ = wide
                .pointer(pointer(
                    PointerPhase::Release,
                    Some(PointerButton::Left),
                    x,
                    1.0,
                    false,
                ))
                .unwrap();
        }
        assert_eq!(wide.selection_text().unwrap().as_deref(), Some("A😀"));
    }

    let mut wrapped = emulator(5, 2);
    wrapped.feed(b"abcdefgh");
    for time in [Duration::ZERO, Duration::from_millis(100)] {
        wrapped.set_gesture_time_for_test(time);
        _ = wrapped
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap();
        _ = wrapped
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap();
    }
    assert_eq!(
        wrapped.selection_text().unwrap().as_deref(),
        Some("abcdefgh")
    );
}

#[test]
fn selection_autoscroll_rate_is_bounded_by_offscreen_depth() {
    assert_eq!(
        selection_autoscroll_interval_for_position(SurfacePosition { x: 1.0, y: 10.0 }, 40, 20,),
        None
    );
    assert_eq!(
        selection_autoscroll_interval_for_position(SurfacePosition { x: 1.0, y: -1.0 }, 40, 20,),
        Some(MAX_SELECTION_AUTOSCROLL_INTERVAL)
    );
    assert_eq!(
        selection_autoscroll_interval_for_position(SurfacePosition { x: 1.0, y: -200.0 }, 40, 20,),
        Some(MIN_SELECTION_AUTOSCROLL_INTERVAL)
    );
}

#[test]
fn autoscroll_tick_moves_the_viewport_and_rejects_a_stale_generation() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"one\r\ntwo\r\nthree\r\nfour");
    let initial = emulator.snapshot().unwrap().unwrap();
    let press = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            1.0,
            21.0,
            false,
        ),
    );
    assert!(emulator.pointer(press).unwrap().screen_changed);
    let drag = current_pointer(
        &emulator,
        pointer(PointerPhase::Motion, None, 1.0, -41.0, false),
    );
    assert!(emulator.pointer(drag).unwrap().screen_changed);
    let dragged = emulator.snapshot().unwrap().unwrap();
    assert_eq!(
        emulator.selection_autoscroll_interval().unwrap(),
        Some(Duration::from_millis(100))
    );

    let action = emulator
        .selection_autoscroll_tick(dragged.generation)
        .unwrap();
    assert!(action.screen_changed);
    let scrolled = emulator.snapshot().unwrap().unwrap();
    assert!(scrolled.scrollbar.offset_rows < dragged.scrollbar.offset_rows);
    assert!(
        emulator
            .selection_autoscroll_tick(initial.generation)
            .unwrap()
            .bytes
            .is_empty()
    );
    assert_eq!(emulator.selection_autoscroll_interval().unwrap(), None);
}

#[test]
fn scrollback_wheel_changes_visible_rows() {
    let mut emulator = emulator(10, 2);
    let _ = emulator.snapshot().unwrap();
    emulator.feed(b"one\r\ntwo\r\nthree");
    let bottom = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&bottom, 0).starts_with("two"));
    assert!(row_text(&bottom, 1).starts_with("three"));
    assert!(bottom.damage.scrollbar);
    assert!(!bottom.damage.title);
    assert!(!bottom.damage.active_screen);
    assert!(!bottom.damage.resize);
    assert_eq!(
        bottom
            .scrollbar
            .offset_rows
            .saturating_add(bottom.scrollbar.visible_rows),
        bottom.scrollbar.total_rows
    );

    let input = current_wheel(&emulator, wheel(1, false));
    let action = emulator.wheel(input).unwrap();
    assert!(action.bytes.is_empty());
    assert!(action.screen_changed);

    let scrolled = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&scrolled, 0).starts_with("one"));
    assert!(row_text(&scrolled, 1).starts_with("two"));
    assert!(
        scrolled
            .scrollbar
            .offset_rows
            .saturating_add(scrolled.scrollbar.visible_rows)
            < scrolled.scrollbar.total_rows
    );
    assert!(scrolled.scrollbar.offset_rows < bottom.scrollbar.offset_rows);
    assert!(scrolled.damage.viewport);
    assert!(!scrolled.damage.scrollbar);
    assert!(!scrolled.damage.title);
    assert_eq!(scrolled.cursor.position, None);

    let action = emulator.scroll_to(bottom.scrollbar.offset_rows);
    assert!(action.bytes.is_empty());
    assert!(action.screen_changed);
    let restored = emulator.snapshot().unwrap().unwrap();
    assert!(row_text(&restored, 0).starts_with("two"));
    assert_eq!(restored.cursor.position.unwrap().row, 1);
    assert!(row_text(&restored, 1).starts_with("three"));
}

#[test]
fn new_output_follows_the_bottom_only_from_a_following_viewport() {
    let mut following = emulator(10, 2);
    following.feed(b"one\r\ntwo\r\nthree");
    let _ = following.snapshot().unwrap().unwrap();
    following.feed(b"\r\nfour");
    let advanced = following.snapshot().unwrap().unwrap();
    assert!(row_text(&advanced, 0).starts_with("three"));
    assert!(row_text(&advanced, 1).starts_with("four"));
    assert_eq!(
        advanced.scrollbar.offset_rows + advanced.scrollbar.visible_rows,
        advanced.scrollbar.total_rows
    );

    let mut anchored = emulator(10, 2);
    anchored.feed(b"one\r\ntwo\r\nthree");
    let _ = anchored.snapshot().unwrap().unwrap();
    let input = current_wheel(&anchored, wheel(1, false));
    anchored.wheel(input).unwrap();
    let before = anchored.snapshot().unwrap().unwrap();
    assert!(row_text(&before, 0).starts_with("one"));
    anchored.feed(b"\r\nfour");
    let after = anchored.snapshot().unwrap().unwrap();
    assert!(row_text(&after, 0).starts_with("one"));
    assert!(row_text(&after, 1).starts_with("two"));
    assert!(
        after.scrollbar.offset_rows + after.scrollbar.visible_rows < after.scrollbar.total_rows
    );
}

#[test]
fn erase_saved_lines_clears_scrollback_without_discarding_the_visible_screen() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"one\r\ntwo\r\nthree");
    let before = emulator.snapshot().unwrap().unwrap();
    assert!(before.scrollbar.total_rows > before.scrollbar.visible_rows);

    emulator.feed(b"\x1b[3J");
    let cleared = emulator.snapshot().unwrap().unwrap();

    assert!(row_text(&cleared, 0).starts_with("two"));
    assert!(row_text(&cleared, 1).starts_with("three"));
    assert_eq!(cleared.scrollbar.total_rows, cleared.scrollbar.visible_rows);
    assert_eq!(cleared.scrollbar.offset_rows, 0);
}

#[test]
fn terminal_reset_returns_to_a_clean_primary_screen() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"primary\r\nscroll\r\nback\x1b[?1049halternate");
    let alternate = emulator.snapshot().unwrap().unwrap();
    assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);

    emulator.feed(b"\x1bc");
    let reset = emulator.snapshot().unwrap().unwrap();

    assert_eq!(reset.active_screen, ActiveScreenSnapshot::Primary);
    assert_eq!(reset.scrollbar.total_rows, reset.scrollbar.visible_rows);
    assert!((0..reset.rows.len()).all(|row| row_text(&reset, row).trim().is_empty()));
    assert_eq!(reset.cursor.position.unwrap().row, 0);
    assert_eq!(reset.cursor.position.unwrap().column, 0);
}

#[test]
fn primary_scrollback_is_bounded_to_the_configured_history() {
    let mut emulator = emulator(10, 2);
    let output = b"x\r\n".repeat(MAX_SCROLLBACK_ROWS + 100);
    emulator.feed(&output);

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot.scrollbar.total_rows
            <= u64::try_from(MAX_SCROLLBACK_ROWS).unwrap() + snapshot.scrollbar.visible_rows
    );
    assert_eq!(
        snapshot.scrollbar.offset_rows + snapshot.scrollbar.visible_rows,
        snapshot.scrollbar.total_rows
    );
}

#[test]
fn scrollback_rejects_a_viewport_request_from_an_older_presentation() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"one\r\ntwo\r\nthree");
    let stale = emulator.snapshot().unwrap().unwrap();
    emulator.resize(geometry(8, 2, 8.0, 18.0)).unwrap();
    let current = emulator.snapshot().unwrap().unwrap();
    assert!(current.generation > stale.generation);

    let action = emulator.scroll_to_at(0, stale.generation);
    assert!(!action.screen_changed);
    assert!(emulator.snapshot().unwrap().is_none());
}

#[test]
fn selection_rejects_coordinates_from_an_older_presentation() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"hello world");
    let stale = emulator.snapshot().unwrap().unwrap();
    emulator.resize(geometry(8, 2, 8.0, 18.0)).unwrap();
    let _ = emulator.snapshot().unwrap().unwrap();

    let action = emulator
        .pointer(PointerInput {
            generation: stale.generation,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 2.0, y: 10.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        })
        .unwrap();

    assert!(!action.screen_changed);
    assert_eq!(emulator.selection_text().unwrap(), None);
}

#[test]
fn selection_owned_presentations_do_not_stale_the_active_gesture() {
    let mut emulator = emulator(12, 2);
    emulator.feed(b"hello world");
    let presented = emulator.snapshot().unwrap().unwrap();
    let press = PointerInput {
        generation: presented.generation,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    _ = emulator.pointer(press).unwrap();
    let mut drag = press;
    drag.phase = PointerPhase::Motion;
    drag.button = None;
    drag.position.x = 41.0;
    _ = emulator.pointer(drag).unwrap();
    let selected = emulator.snapshot().unwrap().unwrap();
    assert!(selected.generation > presented.generation);

    drag.position.x = 108.0;
    let extended = emulator.pointer(drag).unwrap();

    assert!(extended.screen_changed);
    assert_eq!(
        emulator.selection_text().unwrap().as_deref(),
        Some("hello world")
    );
}

#[test]
fn selection_accepts_an_intermediate_selection_owned_presentation() {
    let mut emulator = emulator(12, 2);
    emulator.feed(b"hello world");
    let presented = emulator.snapshot().unwrap().unwrap();
    let mut pointer = PointerInput {
        generation: presented.generation,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    _ = emulator.pointer(pointer).unwrap();

    pointer.phase = PointerPhase::Motion;
    pointer.button = None;
    pointer.position.x = 25.0;
    _ = emulator.pointer(pointer).unwrap();
    let intermediate = emulator.snapshot().unwrap().unwrap();

    pointer.position.x = 41.0;
    _ = emulator.pointer(pointer).unwrap();
    _ = emulator.snapshot().unwrap().unwrap();

    pointer.generation = intermediate.generation;
    pointer.position.x = 108.0;
    _ = emulator.pointer(pointer).unwrap();

    assert_eq!(
        emulator.selection_text().unwrap().as_deref(),
        Some("hello world")
    );
}

#[test]
fn application_mouse_release_survives_output_generation_change() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"\x1b[?1000h\x1b[?1006h");
    let before = emulator.snapshot().unwrap().unwrap();
    let press = PointerInput {
        generation: before.generation,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    _ = emulator.pointer(press).unwrap();
    emulator.feed(b"redraw");
    _ = emulator.snapshot().unwrap().unwrap();

    let release = emulator
        .pointer(PointerInput {
            phase: PointerPhase::Release,
            ..press
        })
        .unwrap();

    assert_eq!(release.bytes, b"\x1b[<0;1;1m");
}

#[test]
fn output_mapping_changes_reject_active_gesture_coordinates() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"one\r\ntwo\r\nthree");
    let before = emulator.snapshot().unwrap().unwrap();
    let input = |phase, x| PointerInput {
        generation: before.generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position: SurfacePosition { x, y: 21.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    _ = emulator.pointer(input(PointerPhase::Press, 1.0)).unwrap();
    _ = emulator.pointer(input(PointerPhase::Motion, 28.0)).unwrap();
    emulator.feed(b"\r\nfour");
    let after = emulator.snapshot().unwrap().unwrap();
    assert!(after.generation > before.generation);

    let stale = emulator.pointer(input(PointerPhase::Motion, 68.0)).unwrap();

    assert!(!stale.screen_changed);
    assert_eq!(emulator.selection_autoscroll_interval().unwrap(), None);
}

#[test]
fn tracked_and_alternate_screen_wheel_events_encode_input() {
    let mut tracked = emulator(10, 2);
    tracked.feed(b"\x1b[?1000h\x1b[?1006h");
    let reported = tracked.wheel(wheel(2, false)).unwrap();
    assert_eq!(reported.bytes, b"\x1b[<64;1;1M\x1b[<64;1;1M");
    assert!(reported.screen_changed);
    let mut horizontal = wheel(0, false);
    horizontal.horizontal_steps = 1;
    assert_eq!(tracked.wheel(horizontal).unwrap().bytes, b"\x1b[<66;1;1M");

    let mut alternate = emulator(10, 2);
    alternate.feed(b"\x1b[?1049h\x1b[?1007h");
    assert_eq!(
        alternate.wheel(wheel(2, false)).unwrap().bytes,
        b"\x1b[A\x1b[A"
    );
    alternate.feed(b"\x1b[?1h");
    assert_eq!(alternate.wheel(wheel(-1, false)).unwrap().bytes, b"\x1bOB");
    let mut horizontal = wheel(0, false);
    horizontal.horizontal_steps = -2;
    let ignored = alternate.wheel(horizontal).unwrap();
    assert!(ignored.bytes.is_empty());
    assert!(!ignored.screen_changed);
}

#[test]
fn osc8_identity_survives_wrapping_in_immutable_snapshots() {
    let mut emulator = emulator(5, 2);
    emulator.feed(b"\x1b]8;;https://example.test/path\x07abcdefgh\x1b]8;;\x07");

    let snapshot = emulator.snapshot().unwrap().unwrap();
    let identities = snapshot
        .rows
        .iter()
        .flat_map(|row| row.iter())
        .filter_map(|cell| cell.hyperlink.as_ref().map(|link| link.identity))
        .collect::<Vec<_>>();

    assert_eq!(identities.len(), 8);
    assert!(identities.iter().all(|identity| *identity == identities[0]));
}

#[test]
fn remote_metadata_context_should_never_resolve_file_links_as_local_paths() {
    let metadata_context = TerminalMetadataContext::Remote(
        crate::terminal::metadata::RemoteTerminalMetadataContext::new(
            crate::domain::SshDestination::new("user@remote".to_owned()).unwrap(),
            crate::domain::RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
        ),
    );
    let mut emulator = TerminalEmulator::new_with_local_filesystem(
        geometry(16, 2, 10.0, 20.0),
        metadata_context,
        "project on remote",
        identity::TERM_FALLBACK,
        Instant::now(),
        LocalFilesystemAuthority::testing_without_access(),
    )
    .unwrap();

    emulator.feed(
        b"\x1b]8;;file:preview.txt\x07remote\x1b]8;;\x07 \
          \x1b]8;;https://example.test\x07web\x1b]8;;\x07",
    );

    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert!(
        snapshot.rows[0][..6]
            .iter()
            .all(|cell| cell.hyperlink.is_none())
    );
    assert!(snapshot.rows[0][7..10].iter().all(|cell| {
        cell.hyperlink
            .as_ref()
            .is_some_and(|link| link.kind == crate::terminal::hyperlink::HyperlinkKind::Url)
    }));
    assert!(!snapshot.metadata.context.is_local());
    assert_eq!(snapshot.metadata.directory.path.as_ref(), "~/project");
}

#[test]
fn ground_utf8_containing_9d_is_printed_without_starting_c1_osc() {
    let mut emulator = emulator(24, 2);

    emulator.feed("before\u{075d}after".as_bytes());
    let snapshot = emulator.snapshot().unwrap().unwrap();
    let text = snapshot.rows[0]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();

    assert!(text.starts_with("before\u{075d}after"));
    assert!(snapshot.rows[0].iter().all(|cell| cell.hyperlink.is_none()));
}

#[test]
fn raw_9d_in_ground_does_not_introduce_an_osc8_link() {
    let mut emulator = emulator(32, 2);

    emulator.feed(b"\x9d8;;file:/tmp/not-a-link\x07visible");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .all(|cell| cell.hyperlink.is_none())
    );
}

#[test]
fn rejected_local_osc8_start_ends_a_previously_active_link() {
    let mut emulator = emulator(16, 2);

    emulator.feed(b"\x1b]8;;https://example.test\x07web\x1b]8;;file:missing\x07plain\x1b]8;;\x07");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot.rows[0][..3]
            .iter()
            .all(|cell| cell.hyperlink.is_some())
    );
    assert!(
        snapshot.rows[0][3..8]
            .iter()
            .all(|cell| cell.hyperlink.is_none())
    );
}

#[test]
fn unsupported_terminal_uri_cannot_attach_resolver_only_local_metadata() {
    let mut emulator = emulator(16, 2);

    emulator.feed(b"\x1b]8;;unsupported:terminal-controlled-metadata\x07plain\x1b]8;;\x07");
    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert!(
        snapshot.rows[0][..5]
            .iter()
            .all(|cell| cell.hyperlink.is_none())
    );
}

#[test]
fn c1_st_inside_osc_is_payload_in_the_pinned_terminal_stream() {
    let mut emulator = emulator(24, 2);

    emulator.feed(b"\x1b]8;;file:missing\x9cNOT_VISIBLE\x07visible");
    let snapshot = emulator.snapshot().unwrap().unwrap();
    let text = snapshot.rows[0]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();

    assert!(text.starts_with("visible"));
    assert!(!text.contains("NOT_VISIBLE"));
}

#[test]
fn cell_mouse_motion_is_deduplicated_without_losing_encoder_state() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?1003h\x1b[?1006h");

    let first = emulator
        .pointer(pointer(PointerPhase::Motion, None, 1.0, 1.0, false))
        .unwrap();
    let same_cell = emulator
        .pointer(pointer(PointerPhase::Motion, None, 9.0, 19.0, false))
        .unwrap();
    let next_cell = emulator
        .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
        .unwrap();

    assert_eq!(first.bytes, b"\x1b[<35;1;1M");
    assert!(same_cell.bytes.is_empty());
    assert_eq!(next_cell.bytes, b"\x1b[<35;2;1M");
}

#[test]
fn paste_encoding_tracks_bracketed_paste_mode() {
    let mut emulator = emulator(10, 2);
    let plain = emulator.paste("one\ntwo".to_owned()).unwrap();
    assert_eq!(plain.bytes, b"one\rtwo");

    emulator.feed(b"\x1b[?2004h");
    let bracketed = emulator.paste("one\ntwo".to_owned()).unwrap();
    assert_eq!(bracketed.bytes, b"\x1b[200~one\ntwo\x1b[201~");
}

#[test]
fn bracketed_paste_encoder_neutralizes_embedded_closing_fences_and_controls() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?2004h");

    let action = emulator.paste("a\x1b[201~\x03b\n".to_owned()).unwrap();

    assert_eq!(action.bytes, b"\x1b[200~a [201~ b\n\x1b[201~");
    assert_eq!(
        action
            .bytes
            .windows(b"\x1b[201~".len())
            .filter(|window| *window == b"\x1b[201~")
            .count(),
        1
    );
}

#[test]
fn focus_encoding_obeys_dec_1004_mode() {
    let mut emulator = emulator(10, 2);

    assert_eq!(emulator.focus(true).unwrap().bytes, b"");
    emulator.feed(b"\x1b[?1004h");
    assert_eq!(emulator.focus(true).unwrap().bytes, b"\x1b[I");
    assert_eq!(emulator.focus(false).unwrap().bytes, b"\x1b[O");
    emulator.feed(b"\x1b[?1004l");
    assert_eq!(emulator.focus(false).unwrap().bytes, b"");
}

#[test]
fn mouse_tracking_reports_bytes_but_shift_overrides_with_selection() {
    let mut emulator = emulator(12, 3);
    emulator.feed(b"hello world\x1b[?1000h\x1b[?1006h");
    _ = emulator.snapshot().unwrap();

    let input = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            false,
        ),
    );
    let reported = emulator.pointer(input).unwrap();
    assert!(!reported.bytes.is_empty());
    let input = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            2.0,
            10.0,
            true,
        ),
    );
    let released = emulator.pointer(input).unwrap();
    assert!(!released.bytes.is_empty());
    assert!(!released.selection_completed);

    let input = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            true,
        ),
    );
    let selected_press = emulator.pointer(input).unwrap();
    let input = current_pointer(
        &emulator,
        pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
    );
    let selected_drag = emulator.pointer(input).unwrap();
    assert!(selected_press.bytes.is_empty());
    assert!(selected_drag.bytes.is_empty());
    assert!(selected_drag.screen_changed);
    let input = current_pointer(
        &emulator,
        pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            48.0,
            10.0,
            true,
        ),
    );
    let selected_release = emulator.pointer(input).unwrap();
    assert!(selected_release.selection_completed);

    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
}

#[test]
fn shift_selection_override_is_policy_driven() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?1000h\x1b[?1006h");

    let mut input = pointer(
        PointerPhase::Press,
        Some(PointerButton::Left),
        1.0,
        1.0,
        true,
    );
    input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

    let action = emulator.pointer(input).unwrap();

    assert_eq!(action.bytes, b"\x1b[<4;1;1M");
}

#[test]
fn shift_policy_applies_to_hover_and_wheel_reporting() {
    let mut hover = emulator(10, 2);
    hover.feed(b"\x1b[?1003h\x1b[?1006h");
    let mut hover_input = pointer(PointerPhase::Motion, None, 11.0, 1.0, true);
    hover_input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

    let hover_action = hover.pointer(hover_input).unwrap();

    assert_eq!(hover_action.bytes, b"\x1b[<39;2;1M");

    let mut wheel_emulator = emulator(10, 2);
    wheel_emulator.feed(b"\x1b[?1000h\x1b[?1006h");
    let mut wheel_input = wheel(1, true);
    wheel_input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

    let wheel_action = wheel_emulator.wheel(wheel_input).unwrap();

    assert_eq!(wheel_action.bytes, b"\x1b[<68;1;1M");
}

#[test]
fn mouse_tracking_changes_publish_pointer_routing_metadata() {
    let mut emulator = emulator(10, 2);
    let initial = emulator.snapshot().unwrap().unwrap();
    assert!(!initial.mouse_tracking);

    emulator.feed(b"\x1b[?1000h");
    let tracked = emulator.snapshot().unwrap().unwrap();

    assert!(tracked.mouse_tracking);
    assert!(tracked.damage.mouse_tracking);
}

#[test]
fn application_mouse_routes_clear_selection_but_hover_does_not() {
    let mut pressed = emulator(12, 3);
    pressed.feed(b"hello world");
    select_first_five(&mut pressed, false);
    pressed.feed(b"\x1b[?1000h\x1b[?1006h");
    let action = pressed
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            false,
        ))
        .unwrap();
    assert!(action.screen_changed);
    assert_eq!(pressed.selection_text().unwrap(), None);

    let mut tracked_wheel = emulator(12, 3);
    tracked_wheel.feed(b"hello world\x1b[?1000h\x1b[?1006h");
    select_first_five(&mut tracked_wheel, true);
    let action = tracked_wheel.wheel(wheel(1, false)).unwrap();
    assert!(action.screen_changed);
    assert_eq!(tracked_wheel.selection_text().unwrap(), None);

    let mut alternate_wheel = emulator(12, 3);
    alternate_wheel.feed(b"\x1b[?1049hhello world\x1b[?1007h");
    select_first_five(&mut alternate_wheel, false);
    let action = alternate_wheel.wheel(wheel(1, false)).unwrap();
    assert!(action.screen_changed);
    assert_eq!(alternate_wheel.selection_text().unwrap(), None);

    let mut hover = emulator(12, 3);
    hover.feed(b"hello world");
    select_first_five(&mut hover, false);
    hover.feed(b"\x1b[?1003h\x1b[?1006h");
    let action = hover
        .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
        .unwrap();
    assert!(!action.screen_changed);
    assert_eq!(hover.selection_text().unwrap(), Some("hello".to_owned()));
}

#[test]
fn additional_presses_and_mismatched_releases_do_not_replace_active_route() {
    let mut emulator = emulator(10, 2);
    emulator.feed(b"\x1b[?1002h\x1b[?1006h");

    let first = emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            1.0,
            1.0,
            false,
        ))
        .unwrap();
    let additional = emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Right),
            1.0,
            1.0,
            false,
        ))
        .unwrap();
    let mismatched_release = emulator
        .pointer(pointer(
            PointerPhase::Release,
            Some(PointerButton::Right),
            1.0,
            1.0,
            false,
        ))
        .unwrap();
    let motion = emulator
        .pointer(pointer(
            PointerPhase::Motion,
            Some(PointerButton::Left),
            11.0,
            1.0,
            false,
        ))
        .unwrap();
    let release = emulator
        .pointer(pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            11.0,
            1.0,
            false,
        ))
        .unwrap();

    assert_eq!(first.bytes, b"\x1b[<0;1;1M");
    assert!(additional.bytes.is_empty());
    assert!(mismatched_release.bytes.is_empty());
    assert_eq!(motion.bytes, b"\x1b[<32;2;1M");
    assert_eq!(release.bytes, b"\x1b[<0;2;1m");
}

#[test]
fn auxiliary_buttons_and_hover_only_report_when_tracking_allows() {
    let mut emulator = emulator(10, 2);
    let ignored = emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Middle),
            1.0,
            1.0,
            false,
        ))
        .unwrap();
    assert!(ignored.bytes.is_empty());

    emulator.feed(b"\x1b[?1003h\x1b[?1006h");
    let reported = emulator
        .pointer(pointer(
            PointerPhase::Press,
            Some(PointerButton::Right),
            1.0,
            1.0,
            true,
        ))
        .unwrap();
    assert!(!reported.bytes.is_empty());
    _ = emulator
        .pointer(pointer(
            PointerPhase::Release,
            Some(PointerButton::Right),
            1.0,
            1.0,
            true,
        ))
        .unwrap();

    let hover = emulator
        .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
        .unwrap();
    assert!(!hover.bytes.is_empty());
    let shifted_hover = emulator
        .pointer(pointer(PointerPhase::Motion, None, 21.0, 1.0, true))
        .unwrap();
    assert!(shifted_hover.bytes.is_empty());
}

#[test]
fn screen_snapshots_are_owned_and_safe_to_share_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<ScreenSnapshot>();
}

#[test]
fn kitty_query_is_truthful_and_precedes_device_attributes() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");

    let replies = emulator.take_pty_responses();
    let graphics = replies
        .windows(b"\x1b_Gi=31;OK\x1b\\".len())
        .position(|window| window == b"\x1b_Gi=31;OK\x1b\\")
        .expect("a supported direct RGB query must receive an OK response");
    let attributes = replies
        .windows(b"\x1b[?62;22;52c".len())
        .position(|window| window == b"\x1b[?62;22;52c")
        .unwrap_or_else(|| panic!("device attributes missing from {replies:?}"));
    assert!(graphics < attributes);
}

#[test]
fn kitty_rgb_transmission_publishes_owned_graphics_damage() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    let initial = emulator.snapshot().unwrap().unwrap();
    emulator.feed(b"\x1b_Ga=T,t=d,f=24,i=7,p=3,s=1,v=1,c=2,r=1;/wAA\x1b\\");

    let snapshot = emulator.snapshot().unwrap().unwrap();

    assert_eq!(snapshot.graphics.images.len(), 1);
    assert_eq!(snapshot.graphics.images[0].rgba.as_ref(), &[255, 0, 0, 255]);
    assert_eq!(snapshot.graphics.placements.len(), 1);
    assert_eq!(snapshot.graphics.placements[0].image.image_id, 7);
    assert_eq!(snapshot.graphics.placements[0].placement_id, 3);
    assert_eq!(snapshot.graphics.placements[0].destination_width, 20);
    assert_eq!(snapshot.graphics.placements[0].destination_height, 20);
    assert!(snapshot.damage.graphics_content);
    assert!(snapshot.damage.graphics_geometry);
    assert_eq!(initial.rows, snapshot.rows);

    assert!(emulator.snapshot().unwrap().is_none());
}

#[test]
fn kitty_retransmission_replaces_content_and_deletion_releases_it() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=9,p=4,s=1,v=1;AQIDBA==\x1b\\");
    let first = emulator.snapshot().unwrap().unwrap();
    let first_generation = first.graphics.images[0].key.generation;

    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=9,p=4,s=1,v=1;BQYHCA==\x1b\\");
    let replaced = emulator.snapshot().unwrap().unwrap();
    assert_ne!(replaced.graphics.images[0].key.generation, first_generation);
    assert_eq!(replaced.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);

    emulator.feed(b"\x1b_Ga=d,d=i,i=9\x1b\\");
    let deleted = emulator.snapshot().unwrap().unwrap();
    assert!(deleted.graphics.images.is_empty());
    assert!(deleted.graphics.placements.is_empty());
    assert!(deleted.damage.graphics_content);
}

#[test]
fn kitty_png_and_chunked_zlib_transmissions_decode_on_the_worker() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(
        b"\x1b_Ga=T,t=d,f=100,i=20,p=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
    );
    let png = emulator.snapshot().unwrap().unwrap();
    assert_eq!(
        png.graphics.images.len(),
        1,
        "PNG response: {:?}",
        emulator.take_pty_responses()
    );
    assert_eq!(png.graphics.images[0].rgba.as_ref(), &[255, 0, 0, 255]);

    emulator.feed(b"\x1b_Ga=T,t=d,f=32,o=z,i=21,p=2,s=1,v=1,m=1;eJz7z8Dw\x1b\\");
    assert!(emulator.snapshot().unwrap().is_none());
    emulator.feed(b"\x1b_Gm=0;HwAE/wH/\x1b\\");
    let zlib = emulator.snapshot().unwrap().unwrap();
    assert_eq!(zlib.graphics.images.len(), 2);
    assert_eq!(zlib.graphics.images[1].rgba.as_ref(), &[255, 0, 0, 255]);
}

#[test]
fn kitty_later_display_resolves_crop_offsets_size_and_z() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=t,t=d,f=32,i=30,s=2,v=1;AQIDBAUGBwg=\x1b\\");
    let transmitted = emulator.snapshot().unwrap().unwrap();
    assert!(transmitted.graphics.images.is_empty());
    assert!(transmitted.damage.graphics_content);

    emulator.feed(b"\x1b_Ga=p,i=30,p=7,x=1,y=0,w=1,h=1,c=2,r=3,X=3,Y=4,C=1,z=-1073741825\x1b\\");
    let displayed = emulator.snapshot().unwrap().unwrap();
    let placement = &displayed.graphics.placements[0];
    assert_eq!(placement.placement_id, 7);
    assert_eq!(placement.source_x, 1);
    assert_eq!(placement.source_width, 1);
    assert_eq!(placement.cell_offset_x, 3);
    assert_eq!(placement.cell_offset_y, 4);
    assert_eq!(placement.destination_width, 17);
    assert_eq!(placement.destination_height, 56);
    assert_eq!(placement.z, -1_073_741_825);
    assert_eq!(displayed.cursor.position.unwrap().column, 0);
}

#[test]
fn kitty_graphics_follow_screen_sync_scroll_and_resize_lifecycle() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=40,p=1,s=1,v=1,C=1;AQIDBA==\x1b\\");
    let primary = emulator.snapshot().unwrap().unwrap();
    let primary_image = Arc::clone(&primary.graphics.images[0]);

    emulator.feed(b"\x1b[?1049h");
    let alternate = emulator.snapshot().unwrap().unwrap();
    assert!(alternate.graphics.placements.is_empty());
    emulator.feed(b"\x1b[?1049l");
    let restored = emulator.snapshot().unwrap().unwrap();
    assert!(Arc::ptr_eq(&primary_image, &restored.graphics.images[0]));

    emulator.feed(b"\x1b[?2026h\x1b_Ga=p,i=40,p=2,C=1\x1b\\");
    assert!(emulator.snapshot().unwrap().is_none());
    emulator.feed(b"\x1b[?2026l");
    let synchronized = emulator.snapshot().unwrap().unwrap();
    assert_eq!(synchronized.graphics.placements.len(), 2);

    emulator.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    let scrolled = emulator.snapshot().unwrap().unwrap();
    assert!(scrolled.damage.graphics_geometry);
    emulator.resize(geometry(10, 5, 10.0, 20.0)).unwrap();
    let resized = emulator.snapshot().unwrap().unwrap();
    assert!(resized.damage.graphics_geometry || resized.damage.resize);
}

#[test]
fn kitty_unicode_placeholder_is_resolved_by_ghostty() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=t,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
    emulator.feed(b"\x1b_Ga=p,i=1,U=1,c=1,r=1\x1b\\");
    emulator.feed("\x1b[38;5;1m\u{10eeee}\x1b[39m".as_bytes());

    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(snapshot.graphics.placements.len(), 1);
    let placement = &snapshot.graphics.placements[0];
    assert!(placement.unicode_placeholder);
    assert_eq!(placement.image.image_id, 1);
    assert_eq!(placement.viewport_col, 0);
    assert_eq!(placement.viewport_row, 0);
}

#[test]
fn kitty_q_policy_and_unsupported_media_remain_safe() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);

    emulator.feed(b"\x1b_Ga=q,t=d,f=24,i=50,s=1,v=1,q=1;AAAA\x1b\\");
    assert!(emulator.take_pty_responses().is_empty());

    emulator.feed(b"\x1b_Ga=q,t=f,f=24,i=51,s=1,v=1;L2V0Yy9wYXNzd2Q=\x1b\\");
    let file_error = emulator.take_pty_responses();
    assert!(file_error.starts_with(b"\x1b_Gi=51;"));
    assert!(!file_error.windows(2).any(|window| window == b"OK"));

    emulator.feed(b"\x1b_Ga=f,i=50\x1b\\");
    let animation_error = emulator.take_pty_responses();
    assert!(!animation_error.windows(2).any(|window| window == b"OK"));

    emulator.feed(b"\x1b_Ga=q,t=d,f=32,i=52,s=8193,v=1,q=2;AAAA\x1b\\alive");
    assert!(emulator.take_pty_responses().is_empty());
    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert!(snapshot.rows[0].iter().any(|cell| cell.text == "a"));
}

#[test]
fn kitty_probe_stays_responsive_when_application_budget_is_exhausted() {
    let _guard = crate::terminal::graphics::test_lock();
    let _reservation =
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT)
            .unwrap();
    let mut emulator = emulator(8, 4);

    emulator.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");

    assert_eq!(emulator.take_pty_responses(), b"\x1b_Gi=31;OK\x1b\\");
    assert!(
        emulator
            .snapshot()
            .unwrap()
            .unwrap()
            .graphics
            .images
            .is_empty()
    );
}

#[test]
fn kitty_small_images_work_in_more_than_two_terminal_sessions() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulators: Vec<_> = (0..12).map(|_| emulator(8, 4)).collect();
    let snapshots: Vec<_> = emulators
        .iter_mut()
        .map(|emulator| {
            emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
            emulator.snapshot().unwrap().unwrap()
        })
        .collect();
    for snapshot in snapshots {
        assert_eq!(snapshot.graphics.images.len(), 1);
        assert_eq!(snapshot.graphics.images[0].rgba.as_ref(), &[1, 2, 3, 4]);
    }
}

#[test]
fn kitty_full_global_budget_preserves_an_uneven_existing_screen_allocation() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=2,v=1;AQIDBAUGBwg=\x1b\\");
    assert_eq!(
        emulator.terminal.kitty_image_storage_bytes().unwrap(),
        [8, 0]
    );
    let reservation =
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT - 8)
            .unwrap();

    emulator.feed(b"still running");

    assert_eq!(
        emulator.terminal.kitty_image_storage_bytes().unwrap(),
        [8, 0]
    );
    drop(reservation);
    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(
        snapshot.graphics.images[0].rgba.as_ref(),
        &[1, 2, 3, 4, 5, 6, 7, 8]
    );
}

#[test]
fn kitty_snapshots_retain_their_allocation_after_the_terminal_session_closes() {
    let _guard = crate::terminal::graphics::test_lock();
    let snapshot = {
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
        emulator.snapshot().unwrap().unwrap()
    };
    assert!(
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT,)
            .is_none()
    );
    let remaining =
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT - 4)
            .unwrap();
    drop(snapshot);
    assert!(GraphicsReservation::try_acquire(4).is_some());
    drop(remaining);
}

#[test]
fn kitty_replacement_releases_superseded_cache_before_requesting_pixel_capacity() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
    drop(emulator.snapshot().unwrap().unwrap());
    let _pressure =
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT - 8)
            .unwrap();

    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;BQYHCA==\x1b\\");
    let replacement = emulator.snapshot().unwrap().unwrap();

    assert_eq!(replacement.graphics.images.len(), 1);
    assert_eq!(replacement.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);
}

#[test]
fn kitty_reset_reclaims_inactive_screen_pixels_without_revisiting_that_screen() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b[?1049h\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
    let old_ui_snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(old_ui_snapshot.graphics.images.len(), 1);

    emulator.feed(b"\x1bc");
    let primary = emulator.snapshot().unwrap().unwrap();

    assert_eq!(primary.active_screen, ActiveScreenSnapshot::Primary);
    assert!(primary.graphics.images.is_empty());
    assert_eq!(
        emulator.terminal.kitty_image_storage_bytes().unwrap(),
        [0, 0]
    );
    assert!(
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT,)
            .is_none()
    );
    drop(old_ui_snapshot);
    assert!(
        GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT,)
            .is_some()
    );
}

#[test]
fn kitty_remote_graphics_cannot_enable_local_file_or_shared_memory_transports() {
    let _guard = crate::terminal::graphics::test_lock();
    let metadata_context = TerminalMetadataContext::Remote(
        crate::terminal::metadata::RemoteTerminalMetadataContext::new(
            crate::domain::SshDestination::new("user@remote".to_owned()).unwrap(),
            crate::domain::RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
        ),
    );
    let mut emulator = TerminalEmulator::new_with_local_filesystem(
        geometry(8, 4, 10.0, 20.0),
        metadata_context,
        "remote",
        identity::TERM_FALLBACK,
        Instant::now(),
        LocalFilesystemAuthority::testing_without_access(),
    )
    .unwrap();
    for medium in ["f", "t", "s"] {
        emulator.feed(
            format!("\x1b_Ga=q,t={medium},f=32,i=1,s=1,v=1;L3RtcC9raXR0eS1maXh0dXJl\x1b\\")
                .as_bytes(),
        );
        let reply = emulator.take_pty_responses();
        assert!(reply.starts_with(b"\x1b_Gi=1;"));
        assert!(!reply.windows(2).any(|window| window == b"OK"));
    }
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=2,s=1,v=1;AQIDBA==\x1b\\");
    let snapshot = emulator.snapshot().unwrap().unwrap();
    assert_eq!(snapshot.graphics.images[0].rgba.as_ref(), &[1, 2, 3, 4]);
}

#[test]
fn kitty_animation_advances_without_output_and_stops_scheduling_when_stopped() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    let start = Instant::now();
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
    emulator.feed(b"\x1b_Ga=f,t=d,f=32,i=1,s=1,v=1,z=40;BQYHCA==\x1b\\");
    emulator.feed(b"\x1b_Ga=a,i=1,r=1,z=40,s=3\x1b\\");
    let first = emulator.snapshot_at(start).unwrap().unwrap();
    assert_eq!(first.graphics.images[0].rgba.as_ref(), &[1, 2, 3, 4]);
    assert_eq!(
        emulator.graphics_animation_deadline(),
        Some(start + Duration::from_millis(40))
    );
    assert!(
        emulator
            .snapshot_at(start + Duration::from_millis(39))
            .unwrap()
            .is_none()
    );

    let next = emulator
        .snapshot_at(start + Duration::from_millis(40))
        .unwrap()
        .unwrap();
    assert_eq!(next.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);
    assert_ne!(next.graphics.images[0].key, first.graphics.images[0].key);
    assert!(next.damage.graphics_content);

    emulator.feed(b"\x1b_Ga=a,i=1,s=1\x1b\\");
    let _ = emulator
        .snapshot_at(start + Duration::from_millis(50))
        .unwrap();
    assert_eq!(emulator.graphics_animation_deadline(), None);
}
#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
mod macos_adapter_tests {
    include!("../../platform/macos_adapter_tests/emulator.rs");
}
