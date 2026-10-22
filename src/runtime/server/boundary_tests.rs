//! Production emulator, server protocol and PTY dimension boundaries.
use super::tests::{await_text, command, host, size};
use super::*;

#[test]
fn large_grid_dimensions_reach_the_emulator_screen_and_pty_together() {
    let mut host = host();
    let mut size = PaneSize {
        columns: 320,
        rows: 120,
        ..size()
    };
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::Attach {
            create: true,
            size,
            resume: None,
            style: None,
        },
    ) else {
        panic!("large grid must attach")
    };
    for (columns, rows) in [(320, 120), (512, 256)] {
        size.columns = columns;
        size.rows = rows;
        assert!(matches!(
            host.request([1; 32], [1; 32], Request::Resize { lease, size }),
            Reply::Unchanged
        ));
        assert!(matches!(
            command(
                &mut host,
                1,
                lease,
                "stty -echo; printf '\\033[2J\\033[H'; stty size; printf 'SIZE-DONE\\n'"
            ),
            Reply::Unchanged
        ));
        await_text(&mut host, 1, lease, &format!("{rows} {columns}"));
        command(&mut host, 1, lease, "printf 'GRID-DONE\\n'");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Reply::Screen(screen) = host.request([1; 32], [1; 32], Request::Poll { lease }) {
                assert_eq!(screen.columns, columns);
                assert_eq!(screen.rows.len(), usize::from(rows));
                assert!(
                    screen
                        .rows
                        .iter()
                        .all(|row| row.len() == usize::from(columns))
                );
                let mut decoder = pane::presentation::ScreenDecoder::default();
                assert!(matches!(
                    decoder.apply(Reply::Screen(screen)),
                    Ok(Reply::Screen(_))
                ));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "large screen did not arrive"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    host.shutdown().unwrap();
}

#[test]
fn the_maximum_grid_with_maximal_cells_and_metadata_fits_one_terminal_frame() {
    let geometry = geometry(PaneSize {
        columns: 512,
        rows: 256,
        ..size()
    });
    let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry).unwrap();
    emulator.feed(b"grid");
    let mut screen = (*emulator.snapshot().unwrap().unwrap()).clone();
    // Six JSON bytes per codepoint is the largest escaped representation. This bounds even
    // strings the emulator would normally sanitize before exposing a rendered cell.
    for row in Arc::make_mut(&mut screen.rows) {
        for cell in Arc::make_mut(row) {
            cell.text = "\0".repeat(64);
            cell.foreground_source =
                TerminalColor::Rgb(crate::appearance::color::Color::rgb(0xffffff));
            cell.background_source = cell.foreground_source;
            cell.underline_source = cell.foreground_source;
            cell.inverse = true;
            cell.bold = true;
            cell.faint = true;
            cell.italic = true;
            cell.blinking = true;
            cell.invisible = true;
            cell.strikethrough = true;
            cell.overline = true;
            cell.spacer_tail = true;
            cell.selected = true;
            cell.underline = TerminalUnderlineSnapshot::Dashed;
        }
    }
    let mut encoded = encode_screen(&screen);
    let link = "https://example.test/".to_owned() + &"a".repeat(235);
    encoded.hyperlinks = (0..pane::effects::MAX_SCREEN_HYPERLINKS)
        .map(|_| pane::effects::Hyperlink::Url(link.clone()))
        .collect();
    let image = pane::graphics::ImageKey {
        image_id: u32::MAX,
        generation: u64::MAX,
    };
    encoded.graphics = Some(Ok(pane::graphics::Snapshot {
        generation: u64::MAX,
        placement_generation: u64::MAX,
        images: vec![pane::graphics::Image {
            key: image,
            width: 1,
            height: 1,
        }],
        placements: (0..pane::graphics::MAX_PLACEMENTS)
            .map(|_| pane::graphics::Placement {
                image,
                placement_id: u32::MAX,
                z: i32::MIN,
                viewport_col: i32::MIN,
                viewport_row: i32::MIN,
                cell_offset_x: u32::MAX,
                cell_offset_y: u32::MAX,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                destination_width: u32::MAX,
                destination_height: u32::MAX,
                unicode_placeholder: true,
            })
            .collect(),
    }));
    encoded
        .graphics
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .validate()
        .unwrap();
    encoded.reading.as_mut().unwrap().find = Some(pane::reading::Find {
        generation: u64::MAX,
        total: 131072,
        current: Some(131072),
        spans: (0..256)
            .flat_map(|row| (0..512).map(move |col| (row, col, col + 1, true)))
            .collect(),
    });
    let bytes = serde_json::to_vec(&Reply::Screen(Box::new(encoded))).unwrap();
    assert!(
        bytes.len() < pane::MAX_TERMINAL_FRAME,
        "full frame has {} bytes",
        bytes.len()
    );
}
