use spaceterm_core::authority::{ContentRevision, ServerEpoch, SessionGeneration, SessionKey};
use spaceterm_core::content::{
    CellContent, CellStyle, CellWidth, ContentBase, ContentError, ContentPage, ContentPageParts,
    ContentRow, MAX_PAGE_CELLS, MAX_PAGE_ROWS, RowId, Screen, StyledCell,
};
use spaceterm_core::identity::{PaneId, ServerId};

fn session() -> SessionKey {
    SessionKey {
        epoch: ServerEpoch::new(1),
        pane: PaneId::new(ServerId::new([1; 16]), 1).unwrap(),
        generation: SessionGeneration::new(1),
    }
}

fn base() -> ContentBase {
    ContentBase {
        session: session(),
        revision: ContentRevision::new(3),
        screen: Screen::Primary,
        screen_generation: 7,
        topology_revision: 11,
    }
}

fn row_id(index: u16) -> RowId {
    RowId {
        screen: Screen::Primary,
        screen_generation: 7,
        node_serial: 19,
        page_row: index,
    }
}

fn cell(text: &str) -> StyledCell {
    StyledCell::new(
        CellContent::grapheme(text.to_owned(), CellWidth::Single).unwrap(),
        CellStyle::default(),
        None,
    )
}

fn row(index: u16, columns: usize) -> ContentRow {
    ContentRow::new(row_id(index), 5, false, vec![cell("a"); columns]).unwrap()
}

fn parts(columns: u16) -> ContentPageParts {
    ContentPageParts::new(base(), columns)
}

#[test]
fn page_rejects_more_than_256_rows() {
    let mut input = parts(1);
    input.rows = (0..=MAX_PAGE_ROWS)
        .map(|index| row(index as u16, 1))
        .collect();
    assert!(matches!(
        ContentPage::new(input),
        Err(ContentError::TooManyRows { .. })
    ));
}

#[test]
fn page_rejects_more_than_16384_cells() {
    let mut input = parts(65);
    input.rows = (0..MAX_PAGE_ROWS)
        .map(|index| row(index as u16, 65))
        .collect();
    assert!(matches!(
        ContentPage::new(input),
        Err(ContentError::TooManyCells { .. })
    ));
    assert_eq!(MAX_PAGE_CELLS, 16_384);
}

#[test]
fn page_rejects_duplicate_stable_row_ids() {
    let mut input = parts(1);
    input.rows = vec![row(0, 1), row(0, 1)];
    assert_eq!(ContentPage::new(input), Err(ContentError::DuplicateRow));
}

#[test]
fn page_rejects_rows_from_another_screen_generation() {
    let mut input = parts(1);
    let mut id = row_id(0);
    id.screen_generation += 1;
    input
        .rows
        .push(ContentRow::new(id, 5, false, vec![cell("a")]).unwrap());
    assert_eq!(ContentPage::new(input), Err(ContentError::WrongScreen));
}

#[test]
fn page_rejects_incomplete_physical_rows() {
    let mut input = parts(2);
    input.rows.push(row(0, 1));
    assert!(matches!(
        ContentPage::new(input),
        Err(ContentError::WrongRowWidth { .. })
    ));
}

#[test]
fn maximum_plain_page_is_admitted_without_truncation() {
    let mut input = parts(64);
    input.rows = (0..MAX_PAGE_ROWS)
        .map(|index| row(index as u16, 64))
        .collect();
    let page = ContentPage::new(input).unwrap();
    assert_eq!(page.parts().rows.len(), 256);
    assert_eq!(
        page.parts()
            .rows
            .iter()
            .map(|row| row.cells().len())
            .sum::<usize>(),
        16_384
    );
}

#[test]
fn complete_emoji_and_combining_graphemes_are_preserved_but_multiple_clusters_fail() {
    for text in ["👩‍💻", "e\u{301}"] {
        let content = CellContent::grapheme(text.to_owned(), CellWidth::Wide).unwrap();
        assert_eq!(content.text(), text);
    }
    assert_eq!(
        CellContent::grapheme("ab".to_owned(), CellWidth::Single),
        Err(ContentError::InvalidGrapheme)
    );
    assert_eq!(
        CellContent::grapheme("\n".to_owned(), CellWidth::Single),
        Err(ContentError::InvalidGrapheme)
    );
}

#[test]
fn byte_limit_rejects_large_complete_graphemes_even_below_cell_limit() {
    let text = format!("a{}", "\u{301}".repeat(400));
    let mut input = parts(64);
    input.rows = (0..6)
        .map(|index| ContentRow::new(row_id(index), 1, false, vec![cell(&text); 64]).unwrap())
        .collect();
    assert_eq!(ContentPage::new(input), Err(ContentError::TooManyBytes));
}

#[test]
fn oversized_physical_row_is_reconstructed_from_bounded_pages() {
    use spaceterm_core::content::{MAX_PAGE_BYTES, PageContinuation, PagePosition};
    use spaceterm_core::geometry::{CellPixels, GridSize, SUPPORTED_GRID_LIMITS};
    let grid = GridSize::new(
        1,
        2_048,
        CellPixels::new(4, 8).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let text = format!("a{}", "\u{20d0}".repeat(64));
    let source = ContentRow::new(row_id(0), 5, true, vec![cell(&text); 2_048]).unwrap();
    let mut whole = parts(grid.columns());
    whole.rows.push(source.clone());
    assert_eq!(ContentPage::new(whole), Err(ContentError::TooManyBytes));

    let mut reconstructed = Vec::new();
    let mut previous: Option<PageContinuation> = None;
    for (index, cells) in source.cells().chunks(1_024).enumerate() {
        let fragment = ContentRow::fragment(
            source.id(),
            source.revision(),
            source.soft_wrapped(),
            grid.columns(),
            (index * 1_024) as u16,
            cells.to_vec(),
        )
        .unwrap();
        if let Some(continuation) = previous {
            assert!(continuation.continues_row(base(), &fragment));
        }
        let mut input = parts(grid.columns());
        if index == 0 {
            input.continuation = Some(PageContinuation::new(base(), &fragment).unwrap());
            assert_eq!(
                input.continuation.unwrap().position,
                PagePosition::WithinRow {
                    row: source.id(),
                    row_revision: source.revision(),
                    columns: grid.columns(),
                    next_column: 1_024,
                }
            );
        }
        input.rows.push(fragment);
        let page = ContentPage::new(input).unwrap();
        assert!(page.parts().rows.len() <= MAX_PAGE_ROWS);
        assert!(page.parts().rows[0].cells().len() <= MAX_PAGE_CELLS);
        assert!(page.accounted_bytes() <= MAX_PAGE_BYTES);
        assert_eq!(
            usize::from(page.parts().rows[0].start_column()),
            reconstructed.len()
        );
        reconstructed.extend_from_slice(page.parts().rows[0].cells());
        previous = page.parts().continuation;
    }
    assert!(previous.is_none());
    assert_eq!(
        ContentRow::new(source.id(), source.revision(), true, reconstructed).unwrap(),
        source
    );
}

#[test]
fn descriptor_heavy_physical_row_is_reconstructed_from_bounded_pages() {
    use spaceterm_core::content::{
        HyperlinkDescriptor, HyperlinkId, MAX_PAGE_BYTES, MAX_PAGE_HYPERLINKS, PageContinuation,
    };
    let links: Vec<_> = (1..=2_048)
        .map(|id| {
            HyperlinkDescriptor::new(
                HyperlinkId::new(id).unwrap(),
                format!("https://example.com/{id}"),
            )
            .unwrap()
        })
        .collect();
    let cells = links
        .iter()
        .map(|link| {
            StyledCell::new(
                CellContent::grapheme("a".to_owned(), CellWidth::Single).unwrap(),
                CellStyle::default(),
                Some(link.id()),
            )
        })
        .collect();
    let source = ContentRow::new(row_id(0), 5, false, cells).unwrap();
    let mut whole = parts(2_048);
    whole.rows.push(source.clone());
    whole.hyperlinks = links.clone();
    assert_eq!(
        ContentPage::new(whole),
        Err(ContentError::TooManyDescriptors)
    );

    let mut reconstructed_cells = Vec::new();
    let mut reconstructed_links = Vec::new();
    let mut previous: Option<PageContinuation> = None;
    for (index, (cells, descriptors)) in source
        .cells()
        .chunks(1_024)
        .zip(links.chunks(1_024))
        .enumerate()
    {
        let fragment = ContentRow::fragment(
            source.id(),
            source.revision(),
            source.soft_wrapped(),
            2_048,
            (index * 1_024) as u16,
            cells.to_vec(),
        )
        .unwrap();
        if let Some(continuation) = previous {
            assert!(continuation.continues_row(base(), &fragment));
        }
        let mut input = parts(2_048);
        input.hyperlinks = descriptors.to_vec();
        if index == 0 {
            input.continuation = Some(PageContinuation::new(base(), &fragment).unwrap());
        }
        input.rows.push(fragment);
        let page = ContentPage::new(input).unwrap();
        assert!(page.parts().rows.len() <= MAX_PAGE_ROWS);
        assert!(page.parts().rows[0].cells().len() <= MAX_PAGE_CELLS);
        assert!(page.parts().hyperlinks.len() <= MAX_PAGE_HYPERLINKS);
        assert!(page.accounted_bytes() <= MAX_PAGE_BYTES);
        assert_eq!(
            usize::from(page.parts().rows[0].start_column()),
            reconstructed_cells.len()
        );
        reconstructed_cells.extend_from_slice(page.parts().rows[0].cells());
        reconstructed_links.extend_from_slice(&page.parts().hyperlinks);
        previous = page.parts().continuation;
    }
    assert!(previous.is_none());
    assert_eq!(
        ContentRow::new(source.id(), source.revision(), false, reconstructed_cells).unwrap(),
        source
    );
    assert_eq!(reconstructed_links, links);
}

#[test]
fn wide_cells_require_exact_spacer_semantics() {
    let wide = StyledCell::new(
        CellContent::grapheme("界".to_owned(), CellWidth::Wide).unwrap(),
        CellStyle::default(),
        None,
    );
    let tail = StyledCell::new(CellContent::spacer_tail(), CellStyle::default(), None);
    assert!(ContentRow::new(row_id(0), 1, false, vec![wide.clone(), tail.clone()]).is_ok());
    assert_eq!(
        ContentRow::new(row_id(0), 1, false, vec![wide]),
        Err(ContentError::InvalidSpacer)
    );
    assert_eq!(
        ContentRow::new(row_id(0), 1, false, vec![tail, cell("a")]),
        Err(ContentError::InvalidSpacer)
    );
    let head = StyledCell::new(CellContent::spacer_head(), CellStyle::default(), None);
    assert!(ContentRow::new(row_id(0), 1, true, vec![cell("a"), head.clone()]).is_ok());
    assert_eq!(
        ContentRow::new(row_id(0), 1, false, vec![head]),
        Err(ContentError::InvalidSpacer)
    );
}

#[test]
fn hyperlink_references_require_bounded_page_descriptors_and_do_not_leak_text() {
    use spaceterm_core::content::{HyperlinkDescriptor, HyperlinkId};
    let id = HyperlinkId::new(1).unwrap();
    let link = HyperlinkDescriptor::new(id, "file:///private/secret".to_owned()).unwrap();
    let mut input = parts(1);
    input.rows.push(
        ContentRow::new(
            row_id(0),
            1,
            false,
            vec![StyledCell::new(
                CellContent::grapheme(
                    "secret".chars().next().unwrap().to_string(),
                    CellWidth::Single,
                )
                .unwrap(),
                CellStyle::default(),
                Some(id),
            )],
        )
        .unwrap(),
    );
    assert_eq!(
        ContentPage::new(input.clone()),
        Err(ContentError::InvalidHyperlink)
    );
    input.hyperlinks.push(link);
    let page = ContentPage::new(input).unwrap();
    assert!(!format!("{page:?}").contains("/private/secret"));
    assert!(!format!("{:?}", cell("z")).contains("\"z\""));
}

#[test]
fn row_fragments_keep_a_wide_grapheme_with_its_spacer_tail() {
    use spaceterm_core::content::PageContinuation;
    let wide = StyledCell::new(
        CellContent::grapheme("界".to_owned(), CellWidth::Wide).unwrap(),
        CellStyle::default(),
        None,
    );
    let tail = StyledCell::new(CellContent::spacer_tail(), CellStyle::default(), None);
    let mut cells = vec![cell("a"); 2_048];
    cells[1_023] = wide;
    cells[1_024] = tail;
    cells[2_047] = StyledCell::new(CellContent::spacer_head(), CellStyle::default(), None);
    let source = ContentRow::new(row_id(0), 5, true, cells).unwrap();
    assert_eq!(
        ContentRow::fragment(
            source.id(),
            5,
            true,
            2_048,
            0,
            source.cells()[..1_024].to_vec()
        ),
        Err(ContentError::InvalidSpacer)
    );
    assert_eq!(
        ContentRow::fragment(
            source.id(),
            5,
            true,
            2_048,
            1_024,
            source.cells()[1_024..].to_vec()
        ),
        Err(ContentError::InvalidSpacer)
    );

    let mut first = parts(2_048);
    first.rows.push(
        ContentRow::fragment(
            source.id(),
            5,
            true,
            2_048,
            0,
            source.cells()[..1_023].to_vec(),
        )
        .unwrap(),
    );
    first.continuation = Some(PageContinuation::new(base(), &first.rows[0]).unwrap());
    let first = ContentPage::new(first).unwrap();
    let mut last = parts(2_048);
    last.rows.push(
        ContentRow::fragment(
            source.id(),
            5,
            true,
            2_048,
            1_023,
            source.cells()[1_023..].to_vec(),
        )
        .unwrap(),
    );
    assert!(
        first
            .parts()
            .continuation
            .unwrap()
            .continues_row(base(), &last.rows[0])
    );
    let last = ContentPage::new(last).unwrap();
    let reconstructed = first.parts().rows[0]
        .cells()
        .iter()
        .chain(last.parts().rows[0].cells())
        .cloned()
        .collect();
    assert_eq!(
        ContentRow::new(source.id(), 5, true, reconstructed).unwrap(),
        source
    );
}

#[test]
fn spacer_head_requires_the_last_physical_column_not_just_the_last_fragment_cell() {
    let head = StyledCell::new(CellContent::spacer_head(), CellStyle::default(), None);
    assert_eq!(
        ContentRow::fragment(row_id(0), 5, true, 4, 1, vec![head.clone()]),
        Err(ContentError::InvalidSpacer)
    );
    assert!(ContentRow::fragment(row_id(0), 5, true, 4, 3, vec![head]).is_ok());
}

#[test]
fn row_fragments_reject_ranges_outside_the_physical_row_including_overflow() {
    for (columns, start_column, count) in [(4, 3, 2), (4, 4, 1), (4, u16::MAX, 2)] {
        assert_eq!(
            ContentRow::fragment(
                row_id(0),
                5,
                false,
                columns,
                start_column,
                vec![cell("a"); count]
            ),
            Err(ContentError::InvalidRowFragment)
        );
    }
    for columns in [0, 2_049] {
        assert_eq!(
            ContentRow::fragment(row_id(0), 5, false, columns, 0, vec![cell("a")]),
            Err(ContentError::InvalidRow)
        );
    }
    assert_eq!(
        ContentRow::fragment(row_id(0), 5, false, 4, 0, Vec::new()),
        Err(ContentError::InvalidRow)
    );
}

#[test]
fn page_cannot_drop_or_skip_an_unfinished_physical_row() {
    use spaceterm_core::content::{PageContinuation, PagePosition};
    let mut input = parts(4);
    input
        .rows
        .push(ContentRow::fragment(row_id(0), 5, false, 4, 0, vec![cell("a"); 2]).unwrap());
    assert_eq!(
        ContentPage::new(input.clone()),
        Err(ContentError::InvalidContinuation)
    );
    input.continuation = Some(PageContinuation {
        base: base(),
        position: PagePosition::AfterRow {
            row: row_id(0),
            row_revision: 5,
        },
    });
    assert_eq!(
        ContentPage::new(input.clone()),
        Err(ContentError::InvalidContinuation)
    );
    input.continuation = None;
    input.rows.push(row(1, 4));
    assert_eq!(
        ContentPage::new(input),
        Err(ContentError::InvalidRowFragment)
    );

    let mut input = parts(4);
    input.rows.push(row(0, 4));
    input
        .rows
        .push(ContentRow::fragment(row_id(1), 5, false, 4, 2, vec![cell("a"); 2]).unwrap());
    assert_eq!(
        ContentPage::new(input),
        Err(ContentError::InvalidRowFragment)
    );
}

#[test]
fn row_continuation_binds_the_exact_row_revision_geometry_and_column() {
    use spaceterm_core::content::{PageContinuation, PagePosition};
    let first = ContentRow::fragment(row_id(0), 5, false, 4, 0, vec![cell("a"); 2]).unwrap();
    let continuation = PageContinuation::new(base(), &first).unwrap();
    let next = ContentRow::fragment(row_id(0), 5, false, 4, 2, vec![cell("a"); 2]).unwrap();
    assert!(continuation.continues_row(base(), &next));
    for (id, revision, columns, start_column, count) in [
        (row_id(1), 5, 4, 2, 2),
        (row_id(0), 6, 4, 2, 2),
        (row_id(0), 5, 5, 2, 3),
        (row_id(0), 5, 4, 3, 1),
        (row_id(0), 5, 4, 1, 3),
    ] {
        let changed = ContentRow::fragment(
            id,
            revision,
            false,
            columns,
            start_column,
            vec![cell("a"); count],
        )
        .unwrap();
        assert!(!continuation.continues_row(base(), &changed));
    }
    let mut changed = base();
    changed.topology_revision += 1;
    assert!(!continuation.continues_row(changed, &next));
    changed = base();
    changed.session.epoch = ServerEpoch::new(2);
    assert!(!continuation.continues_row(changed, &next));
    changed = base();
    changed.revision = ContentRevision::new(4);
    assert!(!continuation.continues_row(changed, &next));

    for (id, revision, columns, next_column) in [
        (row_id(1), 5, 4, 2),
        (row_id(0), 6, 4, 2),
        (row_id(0), 5, 5, 2),
        (row_id(0), 5, 4, 1),
        (row_id(0), 5, 4, 0),
        (row_id(0), 5, 4, 4),
    ] {
        let mut input = parts(4);
        input.rows.push(first.clone());
        input.continuation = Some(PageContinuation {
            base: base(),
            position: PagePosition::WithinRow {
                row: id,
                row_revision: revision,
                columns,
                next_column,
            },
        });
        assert_eq!(
            ContentPage::new(input),
            Err(ContentError::InvalidContinuation)
        );
    }
}

#[test]
fn page_can_finish_one_fragmented_row_and_start_another() {
    use spaceterm_core::content::{PageContinuation, PagePosition};
    let mut input = parts(4);
    input
        .rows
        .push(ContentRow::fragment(row_id(0), 5, false, 4, 2, vec![cell("a"); 2]).unwrap());
    input
        .rows
        .push(ContentRow::fragment(row_id(1), 6, true, 4, 0, vec![cell("b"); 2]).unwrap());
    input.continuation = Some(PageContinuation::new(base(), &input.rows[1]).unwrap());
    let page = ContentPage::new(input).unwrap();
    assert_eq!(
        page.parts().continuation.unwrap().position,
        PagePosition::WithinRow {
            row: row_id(1),
            row_revision: 6,
            columns: 4,
            next_column: 2,
        }
    );
    let next = ContentRow::fragment(row_id(1), 6, true, 4, 2, vec![cell("b"); 2]).unwrap();
    assert!(
        page.parts()
            .continuation
            .unwrap()
            .continues_row(base(), &next)
    );
    assert_eq!(
        PageContinuation::new(base(), &next).unwrap().position,
        PagePosition::AfterRow {
            row: row_id(1),
            row_revision: 6,
        }
    );
}

#[test]
fn continuation_cannot_mix_content_revisions_or_replacement_sessions() {
    use spaceterm_core::content::PageContinuation;
    let mut input = parts(1);
    input.rows.push(row(0, 1));
    let mut previous_base = base();
    previous_base.revision = ContentRevision::new(2);
    input.continuation = Some(PageContinuation::new(previous_base, &input.rows[0]).unwrap());
    assert_eq!(
        ContentPage::new(input.clone()),
        Err(ContentError::InvalidContinuation)
    );
    previous_base = base();
    previous_base.session.generation = SessionGeneration::new(2);
    input.continuation = Some(PageContinuation::new(previous_base, &input.rows[0]).unwrap());
    assert_eq!(
        ContentPage::new(input),
        Err(ContentError::InvalidContinuation)
    );
}

#[test]
fn graphics_references_are_session_qualified_and_crop_arithmetic_is_checked() {
    use spaceterm_core::content::{
        GraphicsPlacement, GraphicsResourceId, GraphicsResourceKey, GraphicsResourceRef, ImageRect,
    };
    let key = GraphicsResourceKey {
        session: session(),
        id: GraphicsResourceId::new(1).unwrap(),
        generation: 1,
    };
    let mut input = parts(1);
    input.rows.push(row(0, 1));
    input.resources.push(GraphicsResourceRef {
        key,
        width: 4,
        height: 4,
        rgba_bytes: 64,
        digest: [0; 32],
    });
    input.placements.push(GraphicsPlacement {
        resource: key,
        placement_id: 1,
        anchor: row_id(0),
        column: 0,
        columns: 1,
        rows: 1,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        source: ImageRect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        },
        destination_width: 4,
        destination_height: 4,
        unicode_placeholder: false,
        z_index: 0,
    });
    assert!(ContentPage::new(input.clone()).is_ok());
    input.placements[0].source.x = u32::MAX;
    assert_eq!(
        ContentPage::new(input.clone()),
        Err(ContentError::InvalidGraphics)
    );
    input.placements.clear();
    input.resources[0].key.session.generation = SessionGeneration::new(2);
    assert_eq!(ContentPage::new(input), Err(ContentError::InvalidGraphics));
}

#[test]
fn graphics_page_admits_rgba_expansion_of_an_image_within_native_rgb_storage_limit() {
    use spaceterm_core::content::{GraphicsResourceId, GraphicsResourceKey, GraphicsResourceRef};
    let mut input = parts(1);
    input.resources.push(GraphicsResourceRef {
        key: GraphicsResourceKey {
            session: session(),
            id: GraphicsResourceId::new(1).unwrap(),
            generation: 1,
        },
        width: 8_000,
        height: 4_000,
        // The native RGB source occupies 96,000,000 bytes, below 96 MiB.
        rgba_bytes: 128_000_000,
        digest: [0; 32],
    });
    let page = ContentPage::new(input).unwrap();
    assert_eq!(page.parts().resources[0].rgba_bytes, 128_000_000);
}

#[test]
fn graphics_page_enforces_a_separate_128_mib_rgba_resource_ceiling() {
    use spaceterm_core::content::{GraphicsResourceId, GraphicsResourceKey, GraphicsResourceRef};
    let mut input = parts(1);
    input.resources.push(GraphicsResourceRef {
        key: GraphicsResourceKey {
            session: session(),
            id: GraphicsResourceId::new(1).unwrap(),
            generation: 1,
        },
        width: 8_192,
        height: 4_096,
        rgba_bytes: 128 * 1024 * 1024,
        digest: [0; 32],
    });
    assert!(ContentPage::new(input.clone()).is_ok());
    input.resources[0].height += 1;
    input.resources[0].rgba_bytes = 8_192 * 4_097 * 4;
    assert_eq!(ContentPage::new(input), Err(ContentError::InvalidGraphics));
}

#[test]
fn graphics_anchor_may_precede_the_page_while_its_image_intersects_visible_rows() {
    use spaceterm_core::content::{
        GraphicsPlacement, GraphicsResourceId, GraphicsResourceKey, GraphicsResourceRef, ImageRect,
    };
    let key = GraphicsResourceKey {
        session: session(),
        id: GraphicsResourceId::new(1).unwrap(),
        generation: 1,
    };
    let mut input = parts(1);
    input.rows.push(row(1, 1));
    input.resources.push(GraphicsResourceRef {
        key,
        width: 4,
        height: 32,
        rgba_bytes: 512,
        digest: [0; 32],
    });
    input.placements.push(GraphicsPlacement {
        resource: key,
        placement_id: 1,
        anchor: row_id(0),
        column: 0,
        columns: 1,
        rows: 2,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        source: ImageRect {
            x: 0,
            y: 0,
            width: 4,
            height: 32,
        },
        destination_width: 4,
        destination_height: 32,
        unicode_placeholder: false,
        z_index: 0,
    });
    assert!(ContentPage::new(input).is_ok());
}
