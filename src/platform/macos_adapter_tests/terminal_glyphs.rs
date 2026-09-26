use super::*;

fn macos_text_system() -> gpui::WindowTextSystem {
    gpui::WindowTextSystem::new(Arc::new(gpui::TextSystem::new(Arc::new(
        gpui_macos::MacTextSystem::new(),
    ))))
}

#[test]
fn macos_scope_guide_redraw_keeps_native_glyphs_on_exact_columns() {
    let text_system = macos_text_system();
    let fonts = test_terminal_fonts(&"Menlo".into());
    let sample = prepare_row(
        &Arc::from([cell("a"), cell("a")]),
        &colors(),
        &"Menlo".into(),
    );
    let shaped = prepare_row_text(&sample, px(14.0), &text_system);
    let natural_advance = shaped.text[0].line.runs[0].glyphs[1].position.x;
    assert!(natural_advance > px(0.0));
    let grid_left = px(0.17);
    for cell_width in [natural_advance + px(0.01), natural_advance + px(0.03125)] {
        let mut baseline = None;
        for guide in [" ", "│", " "] {
            let mut cells = vec![cell(" "); 4];
            cells[3] = cell(guide);
            cells.extend("abéλ".repeat(30).chars().map(|ch| cell(&ch.to_string())));
            cells[10].bold = true;
            cells[20].italic = true;
            let input = prepare_row_cached(&Arc::from(cells), &colors(), &fonts, 0, &[]);
            let shaped = prepare_row_text(&input, px(14.0), &text_system);
            let mut suffix = Vec::new();
            for (fragment, text) in input.fragments.iter().zip(&shaped.text) {
                let origins =
                    terminal_glyph_origins(fragment, &text.line, grid_left, px(3.25), cell_width);
                for ((font, glyph), origin) in text
                    .line
                    .runs
                    .iter()
                    .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)))
                    .zip(origins.iter().copied())
                {
                    let column = fragment.start + fragment.text[..glyph.index].chars().count();
                    if column >= 4 {
                        assert_eq!(
                            origin.x,
                            grid_left + cell_width * column as f32,
                            "guide={guide:?}, column={column}"
                        );
                        suffix.push((font, glyph.id, origin));
                    }
                }
            }
            assert_eq!(suffix.len(), 120);
            if let Some(baseline) = &baseline {
                assert_eq!(
                    &suffix, baseline,
                    "guide={guide:?}, cell_width={cell_width:?}"
                );
            } else {
                baseline = Some(suffix);
            }
        }
    }
}

#[test]
fn macos_cell_anchoring_preserves_complex_cluster_offsets() {
    let text_system = macos_text_system();
    for (cluster, width) in [
        ("e\u{301}\u{30d}", 1),
        ("界", 2),
        ("👩\u{200d}💻", 2),
        ("❤\u{fe0f}", 2),
        ("ש\u{5b8}", 1),
    ] {
        let mut cells = vec![cell(" "); 5];
        cells.push(cell(cluster));
        if width == 2 {
            let mut tail = cell(" ");
            tail.spacer_tail = true;
            cells.push(tail);
        }
        cells.push(cell("z"));
        let input = prepare_row(&Arc::from(cells), &colors(), &"Menlo".into());
        let shaped = prepare_row_text(&input, px(14.0), &text_system);
        let (fragment, text) = input
            .fragments
            .iter()
            .zip(&shaped.text)
            .find(|(fragment, _)| fragment.text.as_ref() == cluster)
            .unwrap();
        assert!(!fragment.simple_cells);
        let glyphs = text
            .line
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .collect::<Vec<_>>();
        assert!(!glyphs.is_empty());
        let anchor = point(px(0.17) + px(8.41) * 5.0, px(3.25));
        let origins = terminal_glyph_origins(fragment, &text.line, px(0.17), px(3.25), px(8.41));
        assert_eq!(
            origins.as_ref(),
            glyphs
                .iter()
                .map(|glyph| anchor + glyph.position)
                .collect::<Vec<_>>(),
            "cluster={cluster:?} must keep native offsets within its head cell"
        );
        let last = input.fragments.last().unwrap();
        let last_text = shaped.text.last().unwrap();
        let next = terminal_glyph_origins(last, &last_text.line, px(0.17), px(3.25), px(8.41));
        assert_eq!(next[0].x, px(0.17) + px(8.41) * (5 + width) as f32);
    }
}
