use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, FontFeatures, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Style,
    TextRun, Window, fill, font, outline, point, px, relative, rgba, size,
};

use crate::terminal::{
    CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, RowSnapshot,
    ScreenSnapshot, TerminalColor, TerminalColorsSnapshot,
};
use crate::theme::{ACTIVE_THEME, Color};

pub(crate) struct TerminalGridCache {
    source_rows: Vec<RowSnapshot>,
    prepared_rows: Arc<[Arc<RowPaintInput>]>,
    font_family: Option<SharedString>,
    colors: Option<TerminalColorsSnapshot>,
}

impl TerminalGridCache {
    pub(crate) fn new() -> Self {
        Self {
            source_rows: Vec::new(),
            prepared_rows: Arc::from([]),
            font_family: None,
            colors: None,
        }
    }

    pub(crate) fn invalidate_scale_dependent(&mut self) {
        self.source_rows.clear();
        self.prepared_rows = Arc::from([]);
        self.font_family = None;
        self.colors = None;
    }

    fn prepare(
        &mut self,
        rows: &Arc<[RowSnapshot]>,
        colors: &TerminalColorsSnapshot,
        font_family: &SharedString,
    ) -> Arc<[Arc<RowPaintInput>]> {
        let style_changed =
            self.font_family.as_ref() != Some(font_family) || self.colors.as_ref() != Some(colors);
        let rows_unchanged = !style_changed
            && rows.len() == self.source_rows.len()
            && rows
                .iter()
                .zip(&self.source_rows)
                .all(|(current, cached)| Arc::ptr_eq(current, cached));
        if rows_unchanged {
            return Arc::clone(&self.prepared_rows);
        }

        let prepared_rows = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                if !style_changed
                    && self
                        .source_rows
                        .get(index)
                        .is_some_and(|cached| Arc::ptr_eq(row, cached))
                {
                    Arc::clone(&self.prepared_rows[index])
                } else {
                    Arc::new(prepare_row(row, colors, font_family))
                }
            })
            .collect::<Vec<_>>();

        self.source_rows = rows.iter().cloned().collect();
        self.prepared_rows = Arc::from(prepared_rows);
        self.font_family = Some(font_family.clone());
        self.colors = Some(colors.clone());
        Arc::clone(&self.prepared_rows)
    }
}

pub(crate) struct TerminalGridElement {
    background: Color,
    rows: Arc<[Arc<RowPaintInput>]>,
    columns: usize,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    cursor: Option<(CursorPositionSnapshot, CellSnapshot)>,
    cursor_style: CursorSnapshot,
    font_family: SharedString,
}

impl TerminalGridElement {
    pub(crate) fn new(
        screen: &Arc<ScreenSnapshot>,
        terminal_input_focused: bool,
        font_family: &SharedString,
        font_size: Pixels,
        line_height: Pixels,
        cell_width: Pixels,
        cache: &mut TerminalGridCache,
    ) -> Self {
        let cursor = screen.cursor.position.and_then(|position| {
            screen
                .rows
                .get(usize::from(position.row))
                .and_then(|row| row.get(usize::from(position.column)))
                .cloned()
                .map(|cell| (position, cell))
        });
        let cursor_style = presented_cursor_style(screen.cursor, terminal_input_focused);
        Self {
            background: screen.background,
            rows: cache.prepare(&screen.rows, &screen.colors, font_family),
            columns: screen.rows.first().map_or(0, |row| row.len()),
            font_size,
            line_height,
            cell_width,
            cursor,
            cursor_style,
            font_family: font_family.clone(),
        }
    }
}

fn presented_cursor_style(
    mut negotiated: CursorSnapshot,
    terminal_input_focused: bool,
) -> CursorSnapshot {
    if negotiated.visible && !terminal_input_focused {
        negotiated.shape = CursorShapeSnapshot::BlockHollow;
        negotiated.blinking = false;
    }
    negotiated
}

struct PreparedText {
    line: ShapedLine,
    origin: gpui::Point<Pixels>,
}

struct PreparedRow {
    text: Vec<PreparedText>,
    backgrounds: Vec<PaintQuad>,
}

pub(crate) struct PrepaintState {
    surface: Option<PaintQuad>,
    rows: Vec<PreparedRow>,
}

impl IntoElement for TerminalGridElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalGridElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let visible_rows = ((f32::from(bounds.size.height) / f32::from(self.line_height)).ceil()
            as usize)
            .min(self.rows.len());
        let mut prepared_rows = Vec::with_capacity(visible_rows);
        let grid_left = terminal_grid_content_bounds(bounds, self.columns, self.cell_width).left();

        for (row_index, row) in self.rows.iter().take(visible_rows).enumerate() {
            let row_top = bounds.top() + self.line_height * row_index as f32;
            let text = row
                .fragments
                .iter()
                .map(|fragment| PreparedText {
                    line: window.text_system().shape_line(
                        fragment.text.clone(),
                        self.font_size,
                        &fragment.runs,
                        fragment.force_cell_width.then_some(self.cell_width),
                    ),
                    origin: point(grid_left + self.cell_width * fragment.start as f32, row_top),
                })
                .collect();
            let backgrounds = row
                .backgrounds
                .iter()
                .chain(&row.selections)
                .map(|span| {
                    fill(
                        Bounds::new(
                            point(grid_left + self.cell_width * span.start as f32, row_top),
                            size(self.cell_width * span.len as f32, self.line_height),
                        ),
                        gpui_color(span.color),
                    )
                })
                .collect::<Vec<_>>();

            let mut text: Vec<PreparedText> = text;
            let mut backgrounds = backgrounds;
            if self.cursor_style.visible
                && let Some((position, cell)) = &self.cursor
                && usize::from(position.row) == row_index
            {
                let cursor_left = grid_left + self.cell_width * f32::from(position.column);
                let plan = cursor_paint_plan(
                    true,
                    self.cursor_style.shape,
                    point(cursor_left, row_top),
                    self.cell_width,
                    self.line_height,
                    position.width_cells,
                )
                .expect("a visible cursor always produces a paint plan");
                backgrounds.push(match plan.paint {
                    CursorPaint::Fill => fill(plan.bounds, gpui_color(self.cursor_style.color)),
                    CursorPaint::Outline => outline(
                        plan.bounds,
                        gpui_color(self.cursor_style.color),
                        BorderStyle::default(),
                    ),
                });

                if plan.recolor_text && !cell.spacer_tail {
                    let mut cursor_font = font(self.font_family.clone());
                    cursor_font.features = FontFeatures::disable_ligatures();
                    if cell.bold {
                        cursor_font = cursor_font.bold();
                    }
                    if cell.italic {
                        cursor_font = cursor_font.italic();
                    }
                    text.push(PreparedText {
                        line: window.text_system().shape_line(
                            cell.text.clone().into(),
                            self.font_size,
                            &[TextRun {
                                len: cell.text.len(),
                                font: cursor_font,
                                color: gpui_color(self.cursor_style.text_color).into(),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            Some(self.cell_width * f32::from(position.width_cells)),
                        ),
                        origin: point(cursor_left, row_top),
                    });
                }
            }

            prepared_rows.push(PreparedRow { text, backgrounds });
        }

        PrepaintState {
            surface: Some(fill(bounds, gpui_color(self.background))),
            rows: prepared_rows,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(surface) = prepaint.surface.take() {
                window.paint_quad(surface);
            }

            for row in &mut prepaint.rows {
                for background in row.backgrounds.drain(..) {
                    window.paint_quad(background);
                }
                for text in row.text.drain(..) {
                    if let Err(error) = text.line.paint(text.origin, self.line_height, window, cx) {
                        eprintln!("failed to paint terminal row: {error:#}");
                    }
                }
            }
        });
    }
}

pub(super) fn terminal_grid_content_bounds(
    bounds: Bounds<Pixels>,
    columns: usize,
    cell_width: Pixels,
) -> Bounds<Pixels> {
    if columns == 0 {
        return bounds;
    }

    let grid_width = (cell_width * columns as f32).min(bounds.size.width);
    let horizontal_remainder = (bounds.size.width - grid_width).max(px(0.0));
    Bounds::new(
        point(bounds.left() + horizontal_remainder / 2.0, bounds.top()),
        size(grid_width, bounds.size.height),
    )
}

struct RowPaintInput {
    fragments: Vec<TextFragment>,
    backgrounds: Vec<BackgroundSpan>,
    selections: Vec<BackgroundSpan>,
}

struct TextFragment {
    start: usize,
    text: SharedString,
    runs: Vec<TextRun>,
    force_cell_width: bool,
}

struct FragmentBuilder {
    start: usize,
    text: String,
    runs: Vec<TextRun>,
}

impl FragmentBuilder {
    fn new(start: usize) -> Self {
        Self {
            start,
            text: String::new(),
            runs: Vec::new(),
        }
    }

    fn push(
        &mut self,
        cell: &CellSnapshot,
        colors: &TerminalColorsSnapshot,
        font_family: &SharedString,
    ) {
        let start = self.text.len();
        self.text.push_str(&cell.text);
        let len = self.text.len() - start;
        if len == 0 {
            return;
        }

        let (foreground, _) = effective_colors(cell, colors);
        let mut cell_font = font(font_family.clone());
        cell_font.features = FontFeatures::disable_ligatures();
        if cell.bold {
            cell_font = cell_font.bold();
        }
        if cell.italic {
            cell_font = cell_font.italic();
        }
        let color = gpui_color(foreground).into();

        if let Some(previous) = self.runs.last_mut()
            && previous.font == cell_font
            && previous.color == color
            && previous.background_color.is_none()
            && previous.underline.is_none()
            && previous.strikethrough.is_none()
        {
            previous.len += len;
        } else {
            self.runs.push(TextRun {
                len,
                font: cell_font,
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }

    fn finish(self, force_cell_width: bool) -> TextFragment {
        TextFragment {
            start: self.start,
            text: self.text.into(),
            runs: self.runs,
            force_cell_width,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackgroundSpan {
    start: usize,
    len: usize,
    color: Color,
}

fn prepare_row(
    row: &RowSnapshot,
    colors: &TerminalColorsSnapshot,
    font_family: &SharedString,
) -> RowPaintInput {
    let mut fragments = Vec::new();
    let mut regular_fragment: Option<FragmentBuilder> = None;
    let mut backgrounds: Vec<BackgroundSpan> = Vec::new();
    let mut selections: Vec<BackgroundSpan> = Vec::new();

    for (column, cell) in row.iter().enumerate() {
        let (_, background) = effective_colors(cell, colors);
        if background != colors.effective_background() {
            if let Some(previous) = backgrounds.last_mut()
                && previous.color == background
                && previous.start + previous.len == column
            {
                previous.len += 1;
            } else {
                backgrounds.push(BackgroundSpan {
                    start: column,
                    len: 1,
                    color: background,
                });
            }
        }

        if cell.selected {
            let selection = ACTIVE_THEME.players[0].selection;
            if let Some(previous) = selections.last_mut()
                && previous.start + previous.len == column
            {
                previous.len += 1;
            } else {
                selections.push(BackgroundSpan {
                    start: column,
                    len: 1,
                    color: selection,
                });
            }
        }

        if cell.spacer_tail {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            continue;
        }

        let is_wide_head = row.get(column + 1).is_some_and(|next| next.spacer_tail);
        let is_single_scalar = cell.text.chars().count() == 1;
        if is_wide_head || !is_single_scalar {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            let mut fragment = FragmentBuilder::new(column);
            fragment.push(cell, colors, font_family);
            fragments.push(fragment.finish(false));
        } else {
            regular_fragment
                .get_or_insert_with(|| FragmentBuilder::new(column))
                .push(cell, colors, font_family);
        }
    }

    if let Some(fragment) = regular_fragment {
        fragments.push(fragment.finish(true));
    }

    RowPaintInput {
        fragments,
        backgrounds,
        selections,
    }
}

fn effective_colors(cell: &CellSnapshot, colors: &TerminalColorsSnapshot) -> (Color, Color) {
    let foreground_source = match (cell.foreground_source, cell.bold) {
        (TerminalColor::Palette(index @ 0..=7), true) => TerminalColor::Palette(index + 8),
        (source, _) => source,
    };
    let resolve = |source| match source {
        TerminalColor::Default => colors.foreground,
        TerminalColor::Palette(index) => colors.palette[usize::from(index)],
        TerminalColor::Rgb(color) => color,
    };
    let mut foreground = resolve(foreground_source);
    let mut background = match cell.background_source {
        TerminalColor::Default => colors.background,
        TerminalColor::Palette(index) => colors.palette[usize::from(index)],
        TerminalColor::Rgb(color) => color,
    };
    if cell.inverse ^ colors.reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    (foreground, background)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CursorPaintPlan {
    bounds: Bounds<Pixels>,
    recolor_text: bool,
    paint: CursorPaint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorPaint {
    Fill,
    Outline,
}

fn cursor_paint_plan(
    visible: bool,
    shape: CursorShapeSnapshot,
    origin: gpui::Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    width_cells: u8,
) -> Option<CursorPaintPlan> {
    if !visible {
        return None;
    }

    let cursor_width = cell_width * f32::from(width_cells.max(1));
    let (bounds, recolor_text, paint) = match shape {
        CursorShapeSnapshot::Block => (
            Bounds::new(origin, size(cursor_width, line_height)),
            true,
            CursorPaint::Fill,
        ),
        CursorShapeSnapshot::Bar => (
            Bounds::new(origin, size((cell_width * 0.12).max(px(1.0)), line_height)),
            false,
            CursorPaint::Fill,
        ),
        CursorShapeSnapshot::Underline => {
            let thickness = (line_height * 0.10).max(px(1.0));
            (
                Bounds::new(
                    point(origin.x, origin.y + line_height - thickness),
                    size(cursor_width, thickness),
                ),
                false,
                CursorPaint::Fill,
            )
        }
        CursorShapeSnapshot::BlockHollow => (
            Bounds::new(origin, size(cursor_width, line_height)),
            false,
            CursorPaint::Outline,
        ),
    };
    Some(CursorPaintPlan {
        bounds,
        recolor_text,
        paint,
    })
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> crate::terminal::TerminalColorsSnapshot {
        let mut palette = [Color::rgb(0); 256];
        palette[1] = Color::rgb(0x11_11_11);
        palette[9] = Color::rgb(0x99_99_99);
        palette[200] = Color::rgb(0x20_02_00);
        crate::terminal::TerminalColorsSnapshot {
            foreground: Color::rgb(0xaa_aa_aa),
            background: Color::rgb(0x0b_0b_0b),
            palette: Arc::new(palette),
            reversed: false,
        }
    }

    fn cell(text: &str) -> CellSnapshot {
        CellSnapshot {
            text: text.to_owned(),
            foreground_source: crate::terminal::TerminalColor::Default,
            background_source: crate::terminal::TerminalColor::Default,
            inverse: false,
            bold: false,
            italic: false,
            selected: false,
            spacer_tail: false,
        }
    }

    #[test]
    fn effective_colors_resolve_sources_bold_and_reverse_precedence() {
        let colors = colors();
        let mut subject = cell("x");

        assert_eq!(
            effective_colors(&subject, &colors),
            (colors.foreground, colors.background)
        );

        subject.foreground_source = crate::terminal::TerminalColor::Palette(1);
        subject.background_source = crate::terminal::TerminalColor::Palette(200);
        assert_eq!(
            effective_colors(&subject, &colors),
            (colors.palette[1], colors.palette[200])
        );

        subject.bold = true;
        assert_eq!(
            effective_colors(&subject, &colors),
            (colors.palette[9], colors.palette[200])
        );

        subject.foreground_source = crate::terminal::TerminalColor::Rgb(Color::rgb(0x12_34_56));
        assert_eq!(
            effective_colors(&subject, &colors).0,
            Color::rgb(0x12_34_56)
        );

        subject.inverse = true;
        assert_eq!(
            effective_colors(&subject, &colors),
            (colors.palette[200], Color::rgb(0x12_34_56))
        );

        let mut reversed = colors.clone();
        reversed.reversed = true;
        assert_eq!(
            effective_colors(&subject, &reversed),
            (Color::rgb(0x12_34_56), colors.palette[200])
        );
    }

    #[test]
    fn cursor_shape_geometry_and_text_layering_are_shape_specific() {
        let origin = point(px(10.0), px(20.0));

        let block = cursor_paint_plan(
            true,
            crate::terminal::CursorShapeSnapshot::Block,
            origin,
            px(9.0),
            px(20.0),
            2,
        )
        .unwrap();
        assert_eq!(block.bounds, Bounds::new(origin, size(px(18.0), px(20.0))));
        assert!(block.recolor_text);

        let bar = cursor_paint_plan(
            true,
            crate::terminal::CursorShapeSnapshot::Bar,
            origin,
            px(9.0),
            px(20.0),
            2,
        )
        .unwrap();
        assert_eq!(bar.bounds.size, size(px(9.0) * 0.12, px(20.0)));
        assert!(!bar.recolor_text);

        let underline = cursor_paint_plan(
            true,
            crate::terminal::CursorShapeSnapshot::Underline,
            origin,
            px(9.0),
            px(20.0),
            2,
        )
        .unwrap();
        assert_eq!(underline.bounds.size, size(px(18.0), px(2.0)));
        assert_eq!(underline.bounds.bottom(), px(40.0));
        assert!(!underline.recolor_text);

        assert!(
            cursor_paint_plan(
                false,
                crate::terminal::CursorShapeSnapshot::Block,
                origin,
                px(9.0),
                px(20.0),
                1,
            )
            .is_none()
        );
    }

    #[test]
    fn hollow_cursor_is_outline_only_and_preserves_covered_text() {
        let plan = cursor_paint_plan(
            true,
            crate::terminal::CursorShapeSnapshot::BlockHollow,
            point(px(0.0), px(0.0)),
            px(9.0),
            px(20.0),
            2,
        )
        .unwrap();

        assert_eq!(plan.paint, CursorPaint::Outline);
        assert_eq!(plan.bounds.size, size(px(18.0), px(20.0)));
        assert!(!plan.recolor_text);
    }

    #[test]
    fn terminal_focus_alone_selects_negotiated_or_steady_hollow_cursor() {
        let negotiated = CursorSnapshot {
            visible: true,
            blinking: true,
            shape: crate::terminal::CursorShapeSnapshot::Bar,
            ..CursorSnapshot::default()
        };

        assert_eq!(presented_cursor_style(negotiated, true), negotiated);
        assert_eq!(
            presented_cursor_style(negotiated, false),
            CursorSnapshot {
                blinking: false,
                shape: crate::terminal::CursorShapeSnapshot::BlockHollow,
                ..negotiated
            }
        );

        let hidden = CursorSnapshot {
            visible: false,
            ..negotiated
        };
        assert_eq!(presented_cursor_style(hidden, false), hidden);
    }

    #[test]
    fn terminal_grid_content_bounds_should_balance_horizontal_remainder() {
        let outer = Bounds::new(point(px(10.0), px(20.0)), size(px(101.0), px(40.0)));
        let content = terminal_grid_content_bounds(outer, 10, px(9.0));

        assert_eq!(
            (
                content.origin.x - outer.origin.x,
                outer.right() - content.right(),
                content.size.width,
            ),
            (px(5.5), px(5.5), px(90.0))
        );
    }

    #[test]
    fn text_runs_cover_utf8_bytes_and_coalesce_matching_styles() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("é"), cell("b")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(input.fragments.len(), 1);
        assert_eq!(input.fragments[0].text.as_ref(), "aéb");
        assert_eq!(input.fragments[0].runs.len(), 1);
        assert_eq!(
            input.fragments[0]
                .runs
                .iter()
                .map(|run| run.len)
                .sum::<usize>(),
            input.fragments[0].text.len()
        );
    }

    #[test]
    fn matching_cell_backgrounds_coalesce() {
        let accent = ACTIVE_THEME.terminal_normal()[1];
        let mut first = cell("a");
        first.background_source = crate::terminal::TerminalColor::Rgb(accent);
        let mut second = cell("b");
        second.background_source = crate::terminal::TerminalColor::Rgb(accent);
        let row = Arc::<[CellSnapshot]>::from([first, second, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(
            input.backgrounds,
            vec![BackgroundSpan {
                start: 0,
                len: 2,
                color: accent,
            }]
        );
    }

    #[test]
    fn selected_cells_coalesce_into_themed_overlay_spans() {
        let mut first = cell("a");
        first.selected = true;
        let mut second = cell("b");
        second.selected = true;
        let row = Arc::<[CellSnapshot]>::from([first, second, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(
            input.selections,
            vec![BackgroundSpan {
                start: 0,
                len: 2,
                color: ACTIVE_THEME.players[0].selection,
            }]
        );
    }

    #[test]
    fn wide_and_combining_cells_anchor_following_text_to_columns() {
        let mut tail = cell(" ");
        tail.spacer_tail = true;
        let row =
            Arc::<[CellSnapshot]>::from([cell("界"), tail, cell("x"), cell("e\u{301}"), cell("y")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(input.fragments.len(), 4);
        assert_eq!(input.fragments[0].start, 0);
        assert_eq!(input.fragments[0].text.as_ref(), "界");
        assert!(!input.fragments[0].force_cell_width);
        assert_eq!(input.fragments[1].start, 2);
        assert_eq!(input.fragments[1].text.as_ref(), "x");
        assert_eq!(input.fragments[2].start, 3);
        assert_eq!(input.fragments[2].text.as_ref(), "e\u{301}");
        assert_eq!(input.fragments[3].start, 4);
        assert_eq!(input.fragments[3].text.as_ref(), "y");
    }

    #[test]
    fn render_cache_reuses_unchanged_prepared_rows() {
        let first_row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let second_row = Arc::<[CellSnapshot]>::from([cell("b")]);
        let rows = Arc::<[RowSnapshot]>::from([Arc::clone(&first_row), Arc::clone(&second_row)]);
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(&rows, &colors(), &"Menlo".into());

        let changed_row = Arc::<[CellSnapshot]>::from([cell("c")]);
        let changed_rows = Arc::<[RowSnapshot]>::from([first_row, changed_row]);
        let second = cache.prepare(&changed_rows, &colors(), &"Menlo".into());

        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert!(!Arc::ptr_eq(&first[1], &second[1]));
    }

    #[test]
    fn render_cache_invalidates_rows_when_color_semantics_change() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let first_colors = colors();
        let first = cache.prepare(&rows, &first_colors, &"Menlo".into());

        let mut changed_colors = first_colors.clone();
        Arc::make_mut(&mut changed_colors.palette)[1] = Color::rgb(0xff_00_00);
        let second = cache.prepare(&rows, &changed_colors, &"Menlo".into());

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }

    #[test]
    fn scale_invalidation_should_discard_prepared_terminal_rows() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(&rows, &colors(), &"Menlo".into());

        cache.invalidate_scale_dependent();
        let second = cache.prepare(&rows, &colors(), &"Menlo".into());

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }
}
