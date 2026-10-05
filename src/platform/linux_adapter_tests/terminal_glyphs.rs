use super::*;
use gpui::PlatformTextSystem as _;

fn linux_native_text() -> Arc<gpui_wgpu::CosmicTextSystem> {
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        crate::bundled_font::FAMILY,
    ));
    text.add_fonts(
        crate::bundled_font::FACES
            .iter()
            .map(|bytes| Cow::Borrowed(*bytes))
            .collect(),
    )
    .unwrap();
    let font = text
        .font_id(&gpui::font(crate::bundled_font::FAMILY))
        .unwrap();
    // These samples deliberately need no installed CJK, emoji, or combining-mark fallback.
    for character in [
        'A', 'Á', 'q', 'é', 'λ', '\u{307}', '\u{302}', '\u{323}', '﹢', '｛',
    ] {
        assert!(
            text.glyph_for_char(font, character).is_some(),
            "missing fixture glyph {character}"
        );
    }
    text
}

fn linux_text_system() -> gpui::WindowTextSystem {
    gpui::WindowTextSystem::new(Arc::new(gpui::TextSystem::new(linux_native_text())))
}

fn linux_painted_cluster(cluster: &'static str, wide: bool) -> Vec<PaintedGlyph> {
    let mut cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        linux_native_text(),
    );
    let cx = cx.add_empty_window();
    let capture = PaintCapture::default();
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(120.0), px(80.0)),
        move |window, _| {
            let mut cells = vec![cell(cluster)];
            if wide {
                let mut tail = cell(" ");
                tail.spacer_tail = true;
                cells.push(tail);
            }
            let input = prepare_row(
                &Arc::from(cells),
                &colors(),
                &crate::bundled_font::FAMILY.into(),
            );
            let shaped = prepare_row_text(&input, px(18.0), window.text_system());
            let mut key = prepared_row_key();
            key.grid_left = px(20.0);
            key.grid_right = px(100.0);
            key.row_top = px(28.0);
            key.row_bottom = px(52.0);
            key.font_size = px(18.0);
            key.cell_width = px(12.0);
            key.line_height = px(24.0);
            let stable = prepare_stable_row(&input, &shaped, key, &mut SymbolPlanCache::default());
            PaintBatches {
                batches: vec![TerminalPaintBatch {
                    surface: None,
                    padding_backgrounds: Vec::new(),
                    corners: BottomCorners::default(),
                    grid_bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(120.0), px(80.0))),
                    line_height: key.line_height,
                    rows: vec![PreparedFrameRow::new(Arc::new(stable))],
                    cursor_text_overlay: None,
                    graphics: GraphicsPaintPlan::default(),
                    blink_phase_visible: true,
                    quad_paint_calls: None,
                }],
            }
        },
    );
    capture_frame(cx, &capture);
    capture.glyphs.borrow().clone()
}

#[test]
fn linux_combining_dot_paints_above_its_base() {
    let glyphs = linux_painted_cluster("q\u{307}", false);
    assert_eq!(glyphs.len(), 2);
    assert!(glyphs[1].visible_bounds.top() < glyphs[0].visible_bounds.top());
    assert!(glyphs[1].visible_bounds.size.height > gpui::ScaledPixels(0.0));
}

#[test]
fn linux_stacked_mark_paints_above_precomposed_base() {
    let glyphs = linux_painted_cluster("A\u{301}\u{302}", false);
    assert_eq!(glyphs.len(), 2);
    assert!(glyphs[1].visible_bounds.top() < glyphs[0].visible_bounds.top());
    assert!(glyphs[1].visible_bounds.size.height > gpui::ScaledPixels(0.0));
}

#[test]
fn linux_wide_cell_combining_dot_paints_above_its_base() {
    // U+FE62 is two cells wide, with a bundled outline below the combining dot.
    let glyphs = linux_painted_cluster("﹢\u{307}", true);
    assert_eq!(glyphs.len(), 2);
    assert!(glyphs[1].visible_bounds.top() < glyphs[0].visible_bounds.top());
    assert!(glyphs[1].visible_bounds.size.height > gpui::ScaledPixels(0.0));
}

#[test]
fn linux_scope_guide_redraw_keeps_native_glyphs_on_exact_columns() {
    let text_system = linux_text_system();
    let family = crate::bundled_font::FAMILY.into();
    let fonts = test_terminal_fonts(&family);
    let sample = prepare_row(&Arc::from([cell("a"), cell("a")]), &colors(), &family);
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
fn linux_cell_anchoring_preserves_native_cluster_offsets_and_wide_tail() {
    let text_system = linux_text_system();
    for (cluster, width) in [
        ("e\u{301}\u{307}", 1),
        ("q\u{307}", 1),
        ("q\u{323}", 1),
        ("｛", 2),
        ("｛\u{307}", 2),
    ] {
        let mut cells = vec![cell(" "); 5];
        cells.push(cell(cluster));
        if width == 2 {
            let mut tail = cell(" ");
            tail.spacer_tail = true;
            cells.push(tail);
        }
        cells.push(cell("z"));
        let input = prepare_row(
            &Arc::from(cells),
            &colors(),
            &crate::bundled_font::FAMILY.into(),
        );
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
        assert_eq!(
            glyphs.len(),
            if cluster.chars().count() == 1 { 1 } else { 2 }
        );
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
        assert_eq!(
            next[0].x,
            px(0.17) + px(8.41) * (5 + width) as f32,
            "cluster={cluster:?} must advance past its complete terminal cell width"
        );
    }
}
