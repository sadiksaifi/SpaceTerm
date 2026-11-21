use super::*;

const WHITE: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};
const BLACK: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};

fn colors() -> Colors<'static> {
    Colors {
        foreground: WHITE,
        background: BLACK,
        foreground_overridden: false,
        palette: &[WHITE; 256],
        palette_overrides: &[false; 256],
        reversed: false,
        bold_as_bright: true,
        bright_foreground: WHITE,
        dim_foreground: WHITE,
        dim: [WHITE; 8],
        selection: HighlightColors {
            background: WHITE,
            foreground: None,
        },
        find_match: HighlightColors {
            background: WHITE,
            foreground: None,
        },
        active_match: HighlightColors {
            background: WHITE,
            foreground: None,
        },
    }
}

#[test]
fn text_and_symbol_spans_follow_frame_columns() {
    let cells = [
        Cell {
            text: "界",
            ..Cell::default()
        },
        Cell::default(),
        Cell {
            text: "x",
            ..Cell::default()
        },
        Cell {
            text: "a",
            ..Cell::default()
        },
        Cell {
            spacer_tail: true,
            ..Cell::default()
        },
        Cell {
            text: "e\u{301}",
            ..Cell::default()
        },
        Cell {
            text: "א",
            ..Cell::default()
        },
        Cell {
            text: "β",
            ..Cell::default()
        },
        Cell {
            text: "é",
            ..Cell::default()
        },
        Cell {
            text: "█",
            ..Cell::default()
        },
        Cell {
            spacer_tail: true,
            ..Cell::default()
        },
        Cell {
            text: "!",
            ..Cell::default()
        },
    ];

    let row = plan_row(cells, &colors(), 0, &[]);

    assert_eq!(
        row.fragments
            .iter()
            .map(|fragment| (
                fragment.start,
                fragment.width_cells,
                fragment.text.as_str(),
                fragment.simple_cells
            ))
            .collect::<Vec<_>>(),
        [
            (0, 1, "界", true),
            (2, 1, "x", true),
            (3, 2, "a", false),
            (5, 1, "e\u{301}", false),
            (6, 1, "א", false),
            (7, 2, "βé", true),
            (11, 1, "!", true),
        ],
    );
    assert_eq!((row.symbols[0].start, row.symbols[0].width_cells), (9, 2));
}
