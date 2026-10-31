//! Production emulator, server protocol and PTY dimension boundaries.
use super::tests::{
    attach_sized, await_screen, await_text, command, frame_bytes, host, lines, size,
};
use super::*;

#[test]
fn large_grid_dimensions_reach_the_emulator_screen_and_pty_together() {
    let mut host = host();
    let mut size = PaneSize {
        columns: 320,
        rows: 120,
        ..size()
    };
    let lease = attach_sized(&mut host, 1, true, size);
    // Each command prints on a new line below the last, without clearing the screen. A debug
    // build takes about a second to present a whole 512×256 grid again, so a clear adds one such
    // redraw to the resize's, and a loaded host stretched the two past the wait for the output.
    for (columns, rows) in [(320, 120), (512, 256)] {
        size.columns = columns;
        size.rows = rows;
        assert!(matches!(
            host.request([1; 32], [1; 32], Request::Resize { lease, size }),
            Reply::Unchanged
        ));
        assert!(matches!(
            command(&mut host, 1, lease, "stty -echo; echo; stty size"),
            Reply::Unchanged
        ));
        await_text(&mut host, lease, &format!("{rows} {columns}"));
        let done = format!("GRID-DONE-{columns}");
        command(&mut host, 1, lease, &format!("printf '{done}\\n'"));
        let update = await_screen(&mut host, [1; 32], lease, |update| {
            lines(update).iter().any(|line| line.contains(&done))
        });
        let geometry = update.geometry.unwrap();
        assert_eq!((geometry.columns, geometry.rows), (columns, rows));
        update.validate().unwrap();
        assert!(frame_bytes(update).is_some());
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
    // Every cell starts its own run: neighbors differ in link, and every style and link table
    // entry is used. Each cell carries the longest text a cell sends.
    let links = (0..spaceterm_server::protocol::limits::MAX_LINKS)
        .map(|index| {
            let value = format!("https://example.test/{index}/");
            let value = value.clone()
                + &"a".repeat(crate::terminal::hyperlink::MAX_LINK_BYTES - value.len());
            Arc::new(HyperlinkTarget::url(&value).unwrap())
        })
        .collect::<Vec<_>>();
    let mut index = 0_usize;
    for row in Arc::make_mut(&mut screen.rows) {
        for cell in Arc::make_mut(row) {
            cell.text = "\u{10ffff}".repeat(32);
            cell.foreground_source = TerminalColor::Rgb(crate::appearance::color::Color::rgb(
                0xff0000 | (index % 4095) as u32,
            ));
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
            cell.selected = true;
            cell.underline = TerminalUnderlineSnapshot::Dashed;
            cell.hyperlink = Some(Arc::clone(&links[index % links.len()]));
            index += 1;
        }
    }
    let image = pane::graphics::ImageKey {
        image_id: u32::MAX,
        generation: u64::MAX,
    };
    screen.graphics.generation = u64::MAX;
    screen.graphics.placement_generation = u64::MAX;
    screen.graphics.images = Arc::from([Arc::new(crate::terminal::ImageSnapshot {
        key: image,
        width: 1,
        height: 1,
        rgba: Arc::from([0_u8; 4]),
        reservation: None,
    })]);
    screen.graphics.placements = (0..pane::graphics::MAX_PLACEMENTS)
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
        .collect();
    screen.find = Some(Arc::new(crate::terminal::TerminalFindSnapshot {
        generation: crate::terminal::FindQueryGeneration::test(u64::MAX),
        total_matches: 131072,
        current_match: Some(131072),
        visible_spans: (0..256)
            .flat_map(|row| {
                (0..512).map(move |column| crate::terminal::FindHighlightSpan {
                    row,
                    start_column: column,
                    end_column: column + 1,
                    current: true,
                })
            })
            .collect(),
    }));
    let capture = screen_encoder::ScreenCapture {
        screen: Arc::new(screen),
        generation: u64::MAX,
        accessibility_revision: u64::MAX,
    };
    let update = screen_encoder::ScreenEncoder::default()
        .encode(&capture, u64::MAX, Some(u64::MAX - 1))
        .unwrap();
    assert_eq!(update.rows.len(), 256);
    assert!(update.rows.iter().all(|row| row.runs.len() == 512));
    assert_eq!(
        update.links.len(),
        spaceterm_server::protocol::limits::MAX_LINKS
    );
    assert!(matches!(&update.graphics, Some(Ok(_))));
    assert!(
        frame_bytes(update).is_some(),
        "a full screen fits one protocol frame"
    );
}
