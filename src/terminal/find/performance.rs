//! Opt-in release measurements with the previous corpus algorithm retained as a baseline.

use super::*;
use std::hint::black_box;
use std::mem::size_of;
use std::time::Instant;

const COLS: u16 = 120;
const ROWS: u16 = 40;

/// Preserves the previous String-per-grapheme implementation independently of production.
fn legacy_corpus(terminal: &Terminal<'_, '_>, cols: u16) -> Result<SearchCorpus, Error> {
    let total_rows = u32::try_from(terminal.scrollbar()?.total).unwrap_or(u32::MAX);
    let mut corpus = SearchCorpus::default();
    let mut graphemes = vec!['\0'; 8];
    for y in 0..total_rows {
        let row_ref = terminal.grid_ref(Point::Screen(PointCoordinate { x: 0, y }))?;
        let wrapped = row_ref.row()?.is_wrapped()?;
        let mut pending_spaces = Vec::new();
        for x in 0..cols {
            let point = PointCoordinate { x, y };
            let reference = terminal.grid_ref(Point::Screen(point))?;
            let cell = reference.cell()?;
            if cell.wide()? == CellWide::SpacerTail {
                continue;
            }
            if !cell.has_text()? {
                pending_spaces.push(point);
                continue;
            }
            for pending in pending_spaces.drain(..) {
                corpus.push_grapheme(" ", pending, 1);
            }
            let count = match reference.graphemes(&mut graphemes) {
                Ok(count) => count,
                Err(Error::OutOfSpace { required }) => {
                    graphemes.resize(required, '\0');
                    reference.graphemes(&mut graphemes)?
                }
                Err(error) => return Err(error),
            };
            let text = graphemes[..count].iter().collect::<String>();
            corpus.push_grapheme(
                &text,
                point,
                if cell.wide()? == CellWide::Wide { 2 } else { 1 },
            );
        }
        if !wrapped && y + 1 < total_rows {
            corpus.bytes.push(b'\n');
            corpus.cells.push(None);
        }
    }
    Ok(corpus)
}

fn measure<T>(iterations: usize, mut operation: impl FnMut() -> T) -> u128 {
    black_box(operation());
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(operation());
    }
    started.elapsed().as_nanos() / iterations as u128
}

#[test]
#[ignore = "explicit optimized Find fixture; run mise run bench:one performance_find_pipeline"]
fn performance_find_pipeline() {
    assert!(!black_box(cfg!(debug_assertions)), "run in release mode");
    for (case, cols, line) in [
        ("ascii", COLS, format!("needle {}\r\n", "a".repeat(80))),
        ("sparse", COLS, "\x1b[21Gneedle\r\n".to_owned()),
        (
            "unicode",
            COLS,
            format!("needle e{} 界 😀\r\n", "\u{301}".repeat(20)),
        ),
        // Narrow rows fit more history in native pages without changing either limit.
        ("narrow_history", 8, "needle\r\n".to_owned()),
    ] {
        for input_rows in [40, 2_000, 10_000] {
            let mut terminal = Terminal::new(libghostty_vt::TerminalOptions {
                cols,
                rows: ROWS,
                max_scrollback: 10_000,
            })
            .unwrap();
            terminal.vt_write(line.repeat(input_rows).as_bytes());
            let scrollbar = terminal.scrollbar().unwrap();
            let retained_rows = scrollbar.total as usize;
            let corpus = SearchCorpus::from_terminal(&terminal, cols).unwrap();
            let legacy = legacy_corpus(&terminal, cols).unwrap();
            assert_eq!(
                legacy.bytes, corpus.bytes,
                "production changed corpus bytes"
            );
            assert_eq!(
                legacy.cells, corpus.cells,
                "production changed cell mappings"
            );
            assert_eq!(legacy.bytes.capacity(), corpus.bytes.capacity());
            assert_eq!(legacy.cells.capacity(), corpus.cells.capacity());
            let legacy_capacity_bytes = legacy.bytes.capacity()
                + legacy.cells.capacity() * size_of::<Option<CellMapping>>();
            let expected_matches = retained_rows - 1;
            assert_eq!(corpus.literal_matches("needle").len(), expected_matches);
            drop(legacy);

            let mut find = TerminalFindState::default();
            find.set_query(FindQueryGeneration::test(1), "needle".to_owned());
            find.refresh(&terminal, cols).unwrap();
            assert_eq!(find.matches.len(), expected_matches);
            let iterations = (10_000 / retained_rows).clamp(3, 40);
            println!(
                "{{\"fixture\":\"find_pipeline\",\"version\":3,\"case\":\"{case}\",\"input_rows\":{input_rows},\"retained_rows\":{retained_rows},\"cols\":{cols},\"corpus_bytes\":{},\"mapping_entries\":{},\"mapping_entry_bytes\":{},\"corpus_capacity_bytes\":{},\"legacy_capacity_bytes\":{legacy_capacity_bytes},\"matches\":{expected_matches},\"iterations\":{iterations}}}",
                corpus.bytes.len(),
                corpus.cells.len(),
                size_of::<Option<CellMapping>>(),
                corpus.bytes.capacity()
                    + corpus.cells.capacity() * size_of::<Option<CellMapping>>(),
            );
            for repetition in 0..3 {
                // Reverse order to expose first-run and allocator reuse bias.
                for use_legacy in if repetition % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                } {
                    let ns = if use_legacy {
                        measure(iterations, || {
                            legacy_corpus(black_box(&terminal), cols).unwrap()
                        })
                    } else {
                        measure(iterations, || {
                            SearchCorpus::from_terminal(black_box(&terminal), cols).unwrap()
                        })
                    };
                    let strategy = if use_legacy {
                        "legacy_baseline"
                    } else {
                        "production"
                    };
                    println!(
                        "{{\"fixture\":\"find_pipeline\",\"version\":3,\"case\":\"{case}\",\"input_rows\":{input_rows},\"repetition\":{repetition},\"phase\":\"corpus\",\"strategy\":\"{strategy}\",\"ns_per_operation\":{ns}}}"
                    );
                }
                let lookup = measure(100, || corpus.literal_matches(black_box("needle")));
                let refresh = measure(iterations, || {
                    find.invalidate();
                    find.refresh(black_box(&terminal), cols).unwrap();
                    black_box(find.matches.len())
                });
                let snapshot = measure(1_000, || {
                    find.snapshot(cols, scrollbar.offset, scrollbar.len)
                });
                assert_eq!(find.matches.len(), expected_matches);
                println!(
                    "{{\"fixture\":\"find_pipeline\",\"version\":3,\"case\":\"{case}\",\"input_rows\":{input_rows},\"repetition\":{repetition},\"lookup_ns\":{lookup},\"refresh_ns\":{refresh},\"snapshot_ns\":{snapshot}}}"
                );
            }
        }
    }
}
