//! Deterministic tests for the Scrollback Cache and the reading that runs over it.
use super::route::{Modes, PointerButton, PointerPhase, PointerRoute, PointerRouter, WheelRoute};
use super::*;
use crate::terminal::{CellSnapshot, FindHighlightSpan, RowSnapshot, TerminalColor};
use std::time::Duration;

const SESSION: [u8; 32] = [7; 32];
const COLUMNS: u16 = 8;
const ROWS: u16 = 4;
const GRID: Grid = Grid {
    columns: COLUMNS,
    rows: ROWS,
    cell_width: 10.0,
    cell_height: 20.0,
};

fn wide(character: char) -> bool {
    matches!(character, '界' | '世' | '😀')
}

/// A row of `COLUMNS` cells. Wide characters take a cell and its spacer.
fn row(text: &str) -> RowSnapshot {
    let mut cells = Vec::new();
    for character in text.chars() {
        cells.push(CellSnapshot {
            text: character.to_string(),
            ..row::blank_cell()
        });
        if wide(character) {
            cells.push(CellSnapshot {
                spacer_tail: true,
                ..row::blank_cell()
            });
        }
    }
    cells.resize(usize::from(COLUMNS), row::blank_cell());
    cells.into()
}

fn screen_of(rows: &[(&str, bool)], alternate: bool) -> Arc<ScreenSnapshot> {
    let mut screen = (*ScreenSnapshot::empty_with_appearance(
        crate::local_path::LocalPathSemantics::Posix,
        &crate::terminal::session::test_terminal_appearance_update(),
    ))
    .clone();
    screen.rows = rows.iter().map(|(text, _)| row(text)).collect();
    screen.row_soft_wrapped = rows.iter().map(|(_, wrapped)| *wrapped).collect();
    screen.size = crate::terminal::emulator::ScreenSizeSnapshot {
        cols: COLUMNS,
        rows: rows.len() as u16,
    };
    screen.active_screen = if alternate {
        ActiveScreenSnapshot::Alternate
    } else {
        ActiveScreenSnapshot::Primary
    };
    Arc::new(screen)
}

fn screen(texts: &[&str], history: History) -> Arc<ScreenSnapshot> {
    let mut screen = Arc::unwrap_or_clone(screen_of(
        &texts.iter().map(|text| (*text, false)).collect::<Vec<_>>(),
        false,
    ));
    screen.history = history;
    Arc::new(screen)
}

/// A server's retained history: the text and soft wrap of each absolute position.
struct Server {
    epoch: u64,
    oldest: u64,
    rows: Vec<(String, bool)>,
}

impl Server {
    fn new(count: u64) -> Self {
        Self {
            epoch: 1,
            oldest: 0,
            rows: (0..count)
                .map(|line| (format!("line{line}"), false))
                .collect(),
        }
    }

    fn history(&self) -> History {
        History {
            epoch: self.epoch,
            oldest: self.oldest,
            total: self.rows.len() as u64,
        }
    }

    fn answer(&self, request: PageRequest) -> Option<(u64, u64, Vec<CachedRow>)> {
        let start = request.start.max(self.oldest);
        let end = (request.start + u64::from(request.rows)).min(self.rows.len() as u64);
        Some((
            self.epoch,
            start,
            (start..end)
                .map(|position| {
                    let (text, wrapped) = &self.rows[position as usize];
                    CachedRow::new(&row(text), *wrapped)
                })
                .collect(),
        ))
    }

    /// Answers every request until none remain, returning how many pages were sent.
    fn serve(&self, scrollback: &mut Scrollback) -> usize {
        let mut pages = 0;
        loop {
            let requests = scrollback.requests();
            if requests.is_empty() {
                return pages;
            }
            for request in requests {
                pages += 1;
                scrollback.page(request, self.answer(request));
            }
        }
    }
}

fn attached(server: &Server, live: &[&str]) -> Scrollback {
    let mut scrollback = Scrollback::default();
    scrollback.attach(SESSION);
    scrollback.screen(screen(live, server.history()));
    scrollback
}

fn presented(scrollback: &mut Scrollback) -> Vec<String> {
    let screen = scrollback
        .present(PresentationGeneration::test(1))
        .expect("a frame was applied");
    screen
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.spacer_tail)
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn ready(outcome: CopyOutcome) -> SelectionCopy {
    match outcome {
        CopyOutcome::Ready(copy) => copy,
        other => panic!("expected a prepared copy, got {other:?}"),
    }
}

fn press(scrollback: &mut Scrollback, x: f32, y: f32, at: Instant) {
    scrollback.pointer(PointerEvent::Press { x, y }, GRID, at);
}

#[test]
fn a_cached_row_presents_its_cells_again_including_styled_trailing_blanks() {
    let mut cells = row("a界b").to_vec();
    cells[1].bold = true;
    for cell in &mut cells[5..] {
        cell.background_source = TerminalColor::Palette(4);
    }
    let original: RowSnapshot = cells.into();
    assert_eq!(CachedRow::new(&original, true).snapshot(), original);
    let blank = row("");
    assert_eq!(CachedRow::new(&blank, false).snapshot(), blank);
}

#[test]
fn pages_start_small_continue_at_full_size_and_stay_within_four_in_flight() {
    let server = Server::new(3000);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    let warm = scrollback.requests();
    assert_eq!(
        warm,
        vec![PageRequest {
            epoch: 1,
            start: 2800,
            rows: 200
        }],
        "the rows just above the screen come first, in a small page"
    );
    scrollback.page(warm[0], server.answer(warm[0]));
    scrollback.set_find_query(FindQueryGeneration::test(1), "line".into());
    let find = scrollback.requests();
    assert_eq!(find.len(), 4, "at most four pages are in flight");
    assert!(find.iter().all(|request| request.rows == 500));
    assert_eq!(
        find.iter().map(|request| request.start).collect::<Vec<_>>(),
        vec![2300, 1800, 1300, 800],
        "pages nearest the view come first"
    );
    assert!(
        scrollback.requests().is_empty(),
        "nothing more until a page settles"
    );
    scrollback.page(find[0], server.answer(find[0]));
    assert_eq!(
        scrollback.requests(),
        vec![PageRequest {
            epoch: 1,
            start: 300,
            rows: 500
        }]
    );
}

#[test]
fn wheel_scrolling_within_cached_history_needs_no_page() {
    let server = Server::new(150);
    let mut scrollback = attached(&server, &["$ a", "", "", ""]);
    server.serve(&mut scrollback);
    for step in 1..=10 {
        assert!(scrollback.wheel(1));
        assert!(
            scrollback.requests().is_empty(),
            "step {step} reads cached rows only"
        );
    }
    assert_eq!(
        presented(&mut scrollback),
        ["line140", "line141", "line142", "line143"]
    );
    let shown = scrollback.present(PresentationGeneration::test(2)).unwrap();
    assert_eq!(shown.scrollbar.total_rows, 154);
    assert_eq!(shown.scrollbar.offset_rows, 140);
    assert_eq!(
        shown.cursor.position, None,
        "the cursor row is below the view"
    );
    assert!(scrollback.wheel(-7));
    assert_eq!(
        presented(&mut scrollback),
        ["line147", "line148", "line149", "$ a"]
    );
    assert!(scrollback.wheel(-100));
    assert_eq!(presented(&mut scrollback), ["$ a", "", "", ""]);
    assert!(!scrollback.wheel(-1), "the view already follows the screen");
}

#[test]
fn scroll_commands_move_by_pages_and_to_either_end() {
    let server = Server::new(50);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    server.serve(&mut scrollback);
    assert!(scrollback.scroll(ScrollbackMovement::PageUp));
    assert_eq!(presented(&mut scrollback)[0], "line46");
    assert!(scrollback.scroll(ScrollbackMovement::Top));
    assert_eq!(presented(&mut scrollback)[0], "line0");
    assert!(scrollback.scroll(ScrollbackMovement::PageDown));
    assert_eq!(presented(&mut scrollback)[0], "line4");
    assert!(scrollback.scroll_to(20));
    assert_eq!(presented(&mut scrollback)[0], "line20");
    assert!(scrollback.scroll(ScrollbackMovement::Bottom));
    assert_eq!(presented(&mut scrollback)[0], "$");
}

#[test]
fn an_uncached_view_shows_blank_rows_until_its_page_arrives() {
    let server = Server::new(5000);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.requests();
    assert!(scrollback.scroll(ScrollbackMovement::Top));
    assert_eq!(presented(&mut scrollback), ["", "", "", ""]);
    let requests = scrollback.requests();
    assert_eq!(requests[0].start, 0, "the view's own rows come first");
    scrollback.page(requests[0], server.answer(requests[0]));
    assert_eq!(
        presented(&mut scrollback),
        ["line0", "line1", "line2", "line3"]
    );
}

#[test]
fn a_scrolled_view_stays_on_its_rows_while_output_continues() {
    let mut server = Server::new(100);
    let mut scrollback = attached(&server, &["a", "b", "c", "$"]);
    server.serve(&mut scrollback);
    scrollback.wheel(3);
    assert_eq!(
        presented(&mut scrollback),
        ["line97", "line98", "line99", "a"]
    );
    // Two rows scroll into history. Until their page arrives, the view shows them as the last
    // frame did, never blank.
    server.rows.push(("a".into(), false));
    server.rows.push(("b".into(), false));
    scrollback.screen(screen(&["c", "$ ls", "x", "y"], server.history()));
    assert_eq!(
        presented(&mut scrollback),
        ["line97", "line98", "line99", "a"]
    );
    scrollback.set_find_query(FindQueryGeneration::test(1), "b".into());
    let find = scrollback
        .present(PresentationGeneration::test(1))
        .unwrap()
        .find
        .clone()
        .unwrap();
    assert_eq!(find.total_matches, 0, "a placeholder is not searched");
    scrollback.end_find(FindQueryGeneration::test(1));
    server.serve(&mut scrollback);
    assert_eq!(
        presented(&mut scrollback),
        ["line97", "line98", "line99", "a"]
    );
    assert!(scrollback.wheel(-5));
    assert_eq!(presented(&mut scrollback), ["c", "$ ls", "x", "y"]);
}

#[test]
fn discarded_history_leaves_the_cache_and_positions_stay_absolute() {
    let mut server = Server::new(20);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    server.serve(&mut scrollback);
    scrollback.scroll(ScrollbackMovement::Top);
    assert_eq!(presented(&mut scrollback)[0], "line0");
    // The server discards its oldest ten rows while ten new rows enter history.
    server.oldest = 10;
    for line in 20..30 {
        server.rows.push((format!("line{line}"), false));
    }
    scrollback.screen(screen(&["$", "", "", ""], server.history()));
    assert_eq!(
        presented(&mut scrollback),
        ["line10", "line11", "line12", "line13"],
        "the view keeps to the oldest retained row"
    );
    assert!(scrollback.cache.get(9).is_none());
    assert_eq!(
        scrollback
            .present(PresentationGeneration::test(3))
            .unwrap()
            .scrollbar
            .total_rows,
        24
    );
    server.serve(&mut scrollback);
    scrollback.scroll(ScrollbackMovement::PageDown);
    scrollback.scroll(ScrollbackMovement::PageDown);
    scrollback.scroll(ScrollbackMovement::PageDown);
    assert_eq!(presented(&mut scrollback)[0], "line22");
}

#[test]
fn the_cache_survives_a_reconnect_only_with_the_same_session_and_epoch() {
    let server = Server::new(300);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.wheel(10);
    server.serve(&mut scrollback);
    // The connection ends and the same Terminal Session is presented again.
    scrollback.attach(SESSION);
    scrollback.screen(screen(&["$", "", "", ""], server.history()));
    assert!(
        scrollback.requests().is_empty(),
        "every row is still cached"
    );
    assert_eq!(
        presented(&mut scrollback)[0],
        "line290",
        "the view is kept too"
    );

    // A cleared Scrollback starts a new epoch.
    let mut cleared = Server::new(0);
    cleared.epoch = 2;
    scrollback.attach(SESSION);
    scrollback.screen(screen(&["$", "", "", ""], cleared.history()));
    assert!(scrollback.cache.get(0).is_none());
    assert_eq!(presented(&mut scrollback)[0], "$");

    // Another Terminal Session never reuses rows, even with a matching epoch.
    let mut other = Scrollback::default();
    other.attach(SESSION);
    other.screen(screen(&["$"], server.history()));
    server.serve(&mut other);
    other.attach([8; 32]);
    assert!(other.cache.get(299).is_none());
}

#[test]
fn the_cache_keeps_the_rows_nearest_the_view_when_full() {
    let server = Server::new(12_000);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.set_find_query(FindQueryGeneration::test(1), "line".into());
    server.serve(&mut scrollback);
    assert!(scrollback.cache.get(11_999).is_some());
    assert!(scrollback.cache.get(2000).is_some());
    assert!(
        scrollback.cache.get(1999).is_none(),
        "rows farthest from the view leave first"
    );
}

#[test]
fn find_matches_across_page_boundaries_and_soft_wraps() {
    let mut server = Server::new(1000);
    // "needle" is soft-wrapped across positions 799 and 800, the boundary of two pages.
    server.rows[799] = ("xx nee".into(), true);
    server.rows[800] = ("dle yy".into(), false);
    server.rows[10] = ("NEEDLE".into(), false);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.set_find_query(FindQueryGeneration::test(1), "needle".into());
    // Pages arrive one at a time, newest first; each match appears once its rows are cached.
    let requests = scrollback.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.start, request.rows))
            .collect::<Vec<_>>(),
        [(800, 200), (300, 500), (0, 300)],
        "the soft-wrapped line straddles two pages"
    );
    let mut totals = Vec::new();
    for request in requests {
        scrollback.page(request, server.answer(request));
        let shown = scrollback.present(PresentationGeneration::test(1)).unwrap();
        totals.push(shown.find.as_ref().unwrap().total_matches);
    }
    assert_eq!(totals, [0, 1, 2], "including the case-folded match");
    assert!(scrollback.requests().is_empty());

    assert!(scrollback.navigate_find(FindQueryGeneration::test(1), FindDirection::Previous));
    let shown = scrollback.present(PresentationGeneration::test(2)).unwrap();
    let find = shown.find.clone().unwrap();
    assert_eq!(find.current_match, Some(2));
    assert_eq!(
        shown.viewport.offset_rows, 799,
        "the view moved to the match"
    );
    assert_eq!(
        find.visible_spans.as_ref(),
        [
            FindHighlightSpan {
                row: 0,
                start_column: 3,
                end_column: 7,
                current: true
            },
            FindHighlightSpan {
                row: 1,
                start_column: 0,
                end_column: 2,
                current: true
            },
        ]
    );
    assert!(scrollback.navigate_find(FindQueryGeneration::test(1), FindDirection::Next));
    let shown = scrollback.present(PresentationGeneration::test(3)).unwrap();
    assert_eq!(shown.find.as_ref().unwrap().current_match, Some(1));
    assert_eq!(shown.viewport.offset_rows, 10);
}

#[test]
fn find_spans_cover_wide_characters_whole() {
    let mut server = Server::new(10);
    server.rows[5] = ("a界b世".into(), false);
    let mut scrollback = attached(&server, &["界", "", "", ""]);
    scrollback.set_find_query(FindQueryGeneration::test(1), "b世".into());
    server.serve(&mut scrollback);
    scrollback.navigate_find(FindQueryGeneration::test(1), FindDirection::Next);
    let shown = scrollback.present(PresentationGeneration::test(1)).unwrap();
    let find = shown.find.clone().unwrap();
    assert_eq!(find.total_matches, 1);
    assert_eq!(
        find.visible_spans.as_ref(),
        [FindHighlightSpan {
            row: 0,
            start_column: 3,
            end_column: 5,
            current: true
        }]
    );
    scrollback.set_find_query(FindQueryGeneration::test(2), "界".into());
    let find = scrollback
        .present(PresentationGeneration::test(2))
        .unwrap()
        .find
        .clone()
        .unwrap();
    assert_eq!(
        find.total_matches, 2,
        "one in history and one on the screen"
    );
    assert!(
        !scrollback.end_find(FindQueryGeneration::test(1)),
        "an older query ends nothing"
    );
    assert!(scrollback.end_find(FindQueryGeneration::test(2)));
    assert!(
        scrollback
            .present(PresentationGeneration::test(3))
            .unwrap()
            .find
            .is_none()
    );
}

#[test]
fn a_selection_across_uncached_rows_copies_once_their_pages_arrive() {
    let server = Server::new(2000);
    let mut scrollback = attached(&server, &["$ end", "", "", ""]);
    let warm = scrollback.requests();
    for request in warm {
        scrollback.page(request, server.answer(request));
    }
    // Select from the screen up through history, then jump to the top while dragging.
    let start = Instant::now();
    press(&mut scrollback, 25.0, 5.0, start);
    scrollback.scroll(ScrollbackMovement::Top);
    for request in scrollback.requests() {
        scrollback.page(request, server.answer(request));
    }
    scrollback.pointer(PointerEvent::Drag { x: 0.0, y: 25.0 }, GRID, start);
    scrollback.pointer(PointerEvent::Release, GRID, start);
    assert_eq!(
        scrollback.copy(SelectionCopyOptions::default()),
        CopyOutcome::Pending,
        "rows between the view and the screen are not cached"
    );
    assert!(!scrollback.copy_ready());
    let pages = server.serve(&mut scrollback);
    assert!(pages >= 3);
    assert!(scrollback.copy_ready());
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    let text = copy.plain_text.as_ref();
    assert!(text.starts_with("line1\nline2\n"));
    assert!(text.ends_with("line1999\n$"), "{text:?}");
    assert_eq!(text.lines().count(), 2000);
}

#[test]
fn words_and_lines_select_whole_wide_characters_and_soft_wrapped_lines() {
    let mut server = Server::new(3);
    server.rows[0] = ("ab 界世c".into(), true);
    server.rows[1] = ("de f".into(), false);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    server.serve(&mut scrollback);
    scrollback.scroll(ScrollbackMovement::Top);
    let start = Instant::now();
    // Double-click on the spacer of 界 selects the wide word across the soft wrap.
    press(&mut scrollback, 45.0, 5.0, start);
    scrollback.pointer(PointerEvent::Release, GRID, start);
    press(
        &mut scrollback,
        45.0,
        5.0,
        start + Duration::from_millis(100),
    );
    scrollback.pointer(PointerEvent::Release, GRID, start);
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(copy.plain_text.as_ref(), "界世cde");
    let shown = scrollback.present(PresentationGeneration::test(1)).unwrap();
    assert!(
        shown.rows[0][3].selected && shown.rows[0][4].selected,
        "the spacer too"
    );
    assert!(!shown.rows[0][2].selected);
    // A third click selects the logical line.
    press(
        &mut scrollback,
        45.0,
        5.0,
        start + Duration::from_millis(200),
    );
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(copy.plain_text.as_ref(), "ab 界世cde f");
    // A late click starts over and clears the Selection.
    scrollback.pointer(PointerEvent::Release, GRID, start);
    press(&mut scrollback, 45.0, 5.0, start + Duration::from_secs(2));
    assert_eq!(
        scrollback.copy(SelectionCopyOptions::default()),
        CopyOutcome::Nothing
    );
}

#[test]
fn a_cell_drag_follows_ghosttys_threshold_and_copies_plain_text_and_html() {
    let mut server = Server::new(2);
    server.rows[0] = ("<b>&'\"".into(), false);
    server.rows[1] = ("é  x".into(), false);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    server.serve(&mut scrollback);
    scrollback.scroll(ScrollbackMovement::Top);
    let start = Instant::now();
    // Pressing past 60% of the first cell excludes it.
    press(&mut scrollback, 7.0, 5.0, start);
    scrollback.pointer(PointerEvent::Drag { x: 8.0, y: 5.0 }, GRID, start);
    assert!(
        !scrollback.selection_present(),
        "the drag has not crossed a threshold"
    );
    scrollback.pointer(PointerEvent::Drag { x: 36.0, y: 25.0 }, GRID, start);
    scrollback.pointer(PointerEvent::Release, GRID, start);
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(copy.plain_text.as_ref(), "b>&'\"\né  x");
    assert_eq!(
        copy.html.as_deref(),
        Some(
            "<div style=\"font-family: monospace; white-space: pre;\">\
             b&gt;&amp;&#39;&quot;\n&#233;  x</div>"
        )
    );
    // Typing returns to the screen and clears the Selection.
    assert!(scrollback.input());
    assert!(!scrollback.selection_present());
    assert_eq!(presented(&mut scrollback)[0], "$");
}

#[test]
fn a_selection_stays_on_its_content_while_output_scrolls() {
    let mut server = Server::new(0);
    let mut scrollback = attached(&server, &["one", "two", "three", "$"]);
    let start = Instant::now();
    press(&mut scrollback, 0.0, 25.0, start);
    scrollback.pointer(PointerEvent::Drag { x: 79.0, y: 45.0 }, GRID, start);
    scrollback.pointer(PointerEvent::Release, GRID, start);
    server.rows.push(("one".into(), false));
    server.rows.push(("two".into(), false));
    scrollback.screen(screen(&["three", "$ ls", "", ""], server.history()));
    assert_eq!(
        scrollback.copy(SelectionCopyOptions::default()),
        CopyOutcome::Pending,
        "the selected row that scrolled into history arrives by page"
    );
    server.serve(&mut scrollback);
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(copy.plain_text.as_ref(), "two\nthree");
    let shown = scrollback.present(PresentationGeneration::test(1)).unwrap();
    assert!(shown.rows[0][0].selected);
    assert!(!shown.rows[1][0].selected);
}

#[test]
fn dragging_past_the_top_edge_autoscrolls_and_extends_the_selection() {
    let server = Server::new(10);
    let mut scrollback = attached(&server, &["a", "b", "c", "$"]);
    server.serve(&mut scrollback);
    let start = Instant::now();
    press(&mut scrollback, 15.0, 45.0, start);
    scrollback.pointer(PointerEvent::Drag { x: 0.0, y: 0.0 }, GRID, start);
    assert!(scrollback.autoscrolling());
    assert!(scrollback.autoscroll(GRID));
    assert!(scrollback.autoscroll(GRID));
    assert_eq!(presented(&mut scrollback)[0], "line8");
    scrollback.pointer(PointerEvent::Release, GRID, start);
    assert!(!scrollback.autoscrolling());
    let copy = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(copy.plain_text.as_ref(), "line8\nline9\na\nb\nc");
}

#[test]
fn the_alternate_screen_has_no_scrollback_to_read() {
    let server = Server::new(100);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    server.serve(&mut scrollback);
    scrollback.wheel(5);
    let mut alternate = Arc::unwrap_or_clone(screen_of(
        &[("vim", false), ("", false), ("", false), ("", false)],
        true,
    ));
    alternate.history = server.history();
    scrollback.screen(Arc::new(alternate));
    assert!(!scrollback.wheel(1));
    assert!(!scrollback.scroll(ScrollbackMovement::Top));
    assert_eq!(presented(&mut scrollback)[0], "vim");
    let shown = scrollback.present(PresentationGeneration::test(1)).unwrap();
    assert_eq!(shown.scrollbar.total_rows, 4);
    scrollback.set_find_query(FindQueryGeneration::test(1), "line".into());
    assert!(scrollback.requests().is_empty());
    let find = scrollback
        .present(PresentationGeneration::test(2))
        .unwrap()
        .find
        .clone()
        .unwrap();
    assert_eq!(
        find.total_matches, 0,
        "history is not searched behind the alternate screen"
    );
}

#[test]
fn wheel_input_goes_to_the_program_under_todays_rules() {
    let modes = |mouse_tracking, alternate_screen, alternate_scroll, shift_override| Modes {
        mouse_tracking,
        alternate_screen,
        alternate_scroll,
        shift_override,
    };
    use WheelRoute::{Ignore, Program, Scrollback as Local};
    for (input, expected) in [
        (modes(false, false, false, false), Local),
        (modes(false, false, true, false), Local),
        (modes(true, false, false, false), Program),
        (modes(true, false, false, true), Local),
        (modes(true, true, true, false), Program),
        (modes(true, true, false, true), Ignore),
        (modes(false, true, true, false), Program),
        (modes(false, true, true, true), Program),
        (modes(false, true, false, false), Ignore),
    ] {
        assert_eq!(route::wheel_route(input), expected, "{input:?}");
    }
}

#[test]
fn a_press_decides_where_its_whole_pointer_gesture_goes() {
    use PointerPhase::{Motion, Press, Release};
    let tracking = Modes {
        mouse_tracking: true,
        ..Modes::default()
    };
    let shifted = Modes {
        shift_override: true,
        ..tracking
    };
    let plain = Modes::default();
    let left = Some(PointerButton::Left);
    let mut router = PointerRouter::default();
    assert_eq!(router.route(Press, left, tracking), PointerRoute::Program);
    // Releasing Shift or the program leaving mouse mode mid-gesture keeps the gesture's route.
    assert_eq!(router.route(Motion, left, plain), PointerRoute::Program);
    assert_eq!(router.route(Release, left, plain), PointerRoute::Program);
    assert_eq!(router.route(Press, left, shifted), PointerRoute::Selection);
    assert_eq!(
        router.route(Motion, left, tracking),
        PointerRoute::Selection
    );
    assert_eq!(
        router.route(Release, left, tracking),
        PointerRoute::Selection
    );
    assert_eq!(
        router.route(Motion, None, tracking),
        PointerRoute::Program,
        "hover reports"
    );
    assert_eq!(router.route(Motion, None, plain), PointerRoute::Ignore);
    assert_eq!(
        router.route(Press, Some(PointerButton::Other), plain),
        PointerRoute::Ignore
    );
    assert_eq!(
        router.route(Press, Some(PointerButton::Other), tracking),
        PointerRoute::Program
    );
}

#[test]
fn a_refused_or_shortened_page_is_not_asked_for_again_until_history_changes() {
    let mut server = Server::new(1000);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.scroll(ScrollbackMovement::Top);
    let requests = scrollback.requests();
    assert!(!requests.is_empty());
    // The server refuses every page, as for an ended epoch.
    for request in requests {
        scrollback.page(request, None);
    }
    // A hidden Pane receives no frames, so nothing may ask again however often it wakes.
    for _ in 0..10 {
        let again = scrollback.requests();
        assert!(
            again.iter().all(|request| request.start >= 1000 - 200),
            "refused rows were asked for again: {again:?}"
        );
        for request in again {
            scrollback.page(request, None);
        }
    }
    assert!(scrollback.requests().is_empty());
    // Output continues, and the server discarded the oldest rows before the frame reports it:
    // a page starts later than asked.
    server.rows.push(("line1000".into(), false));
    server.oldest = 300;
    scrollback.screen(screen(
        &["$", "", "", ""],
        History {
            oldest: 0,
            ..server.history()
        },
    ));
    let requests = scrollback.requests();
    assert_eq!(requests[0].start, 0, "a new frame allows asking again");
    for request in requests {
        scrollback.page(request, server.answer(request));
    }
    assert_eq!(
        scrollback.cache.history().oldest,
        300,
        "a later page start shows where history begins"
    );
    assert_eq!(presented(&mut scrollback)[0], "line300");
    assert!(
        scrollback
            .requests()
            .iter()
            .all(|request| request.start >= 300),
        "discarded rows are never asked for"
    );
}

#[test]
fn pages_of_an_earlier_connection_count_until_their_replies_settle_them() {
    let server = Server::new(5000);
    let mut scrollback = attached(&server, &["$", "", "", ""]);
    scrollback.set_find_query(FindQueryGeneration::test(1), "line".into());
    let first = scrollback.requests();
    assert_eq!(first.len(), 4);
    // The connection ends and the same Session is presented again.
    scrollback.attach(SESSION);
    scrollback.screen(screen(&["$", "", "", ""], server.history()));
    assert!(
        scrollback.requests().is_empty(),
        "unanswered pages still count toward the limit"
    );
    // The ended link fails each pending request.
    for request in first {
        scrollback.page(request, None);
    }
    assert_eq!(scrollback.requests().len(), 4);
}

#[test]
fn a_copy_survives_unrelated_output_and_follows_rewritten_selected_text() {
    let server = Server::new(0);
    let mut scrollback = attached(&server, &["selected", "other", "", "$"]);
    let start = Instant::now();
    press(&mut scrollback, 0.0, 5.0, start);
    scrollback.pointer(PointerEvent::Drag { x: 79.0, y: 5.0 }, GRID, start);
    scrollback.pointer(PointerEvent::Release, GRID, start);
    let first = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(first.plain_text.as_ref(), "selected");
    // Output elsewhere keeps the prepared text.
    scrollback.screen(screen(&["selected", "changed", "", "$"], server.history()));
    let unchanged = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert!(Arc::ptr_eq(&first.plain_text, &unchanged.plain_text));
    // A program rewrites the selected row.
    scrollback.screen(screen(&["replaced", "changed", "", "$"], server.history()));
    let replaced = ready(scrollback.copy(SelectionCopyOptions::default()));
    assert_eq!(replaced.plain_text.as_ref(), "replaced");
}
