use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, Font, FontFallbacks, FontFeatures, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, PaintQuad, Path, PathBuilder, Pixels, ShapedLine, SharedString, Style,
    TextRun, UnderlineStyle, Window, fill, font, outline, point, px, relative, rgba, size,
};
use unicode_bidi::{BidiClass, bidi_class};

use crate::terminal::{
    CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, RowSnapshot,
    ScreenSnapshot, TerminalColor, TerminalColorsSnapshot, TerminalUnderlineSnapshot,
};
use crate::theme::{ACTIVE_THEME, Color};

use super::terminal_ime::PreeditLayout;
use super::terminal_pane::TerminalPane;
use super::terminal_symbols::{SymbolPlan, SymbolPlanCache, SymbolPrimitive, terminal_symbol};

#[derive(Clone, Copy)]
struct TerminalGridMetrics {
    cell_width: Pixels,
    line_height: Pixels,
    scale_factor: f32,
}

pub(crate) struct TerminalGridCache {
    source_rows: Vec<RowSnapshot>,
    prepared_rows: Arc<[Arc<RowPaintInput>]>,
    font_family: Option<SharedString>,
    colors: Option<TerminalColorsSnapshot>,
    cursor: Option<CursorPositionSnapshot>,
    cell_width: Option<Pixels>,
    line_height: Option<Pixels>,
    scale_factor_bits: Option<u32>,
    symbol_plans: SymbolPlanCache,
}

impl TerminalGridCache {
    pub(crate) fn new() -> Self {
        Self {
            source_rows: Vec::new(),
            prepared_rows: Arc::from([]),
            font_family: None,
            colors: None,
            cursor: None,
            cell_width: None,
            line_height: None,
            scale_factor_bits: None,
            symbol_plans: SymbolPlanCache::default(),
        }
    }

    pub(crate) fn invalidate_scale_dependent(&mut self) {
        self.source_rows.clear();
        self.prepared_rows = Arc::from([]);
        self.font_family = None;
        self.colors = None;
        self.cursor = None;
        self.cell_width = None;
        self.line_height = None;
        self.scale_factor_bits = None;
        self.symbol_plans.invalidate_scale_dependent();
    }

    fn prepare(
        &mut self,
        rows: &Arc<[RowSnapshot]>,
        colors: &TerminalColorsSnapshot,
        font_family: &SharedString,
        cursor: Option<CursorPositionSnapshot>,
        metrics: TerminalGridMetrics,
    ) -> Arc<[Arc<RowPaintInput>]> {
        let style_changed = self.font_family.as_ref() != Some(font_family)
            || self.colors.as_ref() != Some(colors)
            || self.cell_width != Some(metrics.cell_width)
            || self.line_height != Some(metrics.line_height)
            || self.scale_factor_bits != Some(metrics.scale_factor.to_bits());
        let rows_unchanged = !style_changed
            && self.cursor == cursor
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
                let cursor_column = cursor_column_for_row(cursor, index);
                if !style_changed
                    && self
                        .source_rows
                        .get(index)
                        .is_some_and(|cached| Arc::ptr_eq(row, cached))
                    && cursor_column_for_row(self.cursor, index) == cursor_column
                {
                    Arc::clone(&self.prepared_rows[index])
                } else {
                    Arc::new(prepare_row_cached(
                        row,
                        colors,
                        font_family,
                        cursor_column,
                        &mut self.symbol_plans,
                        metrics,
                    ))
                }
            })
            .collect::<Vec<_>>();

        self.source_rows = rows.iter().cloned().collect();
        self.prepared_rows = Arc::from(prepared_rows);
        self.font_family = Some(font_family.clone());
        self.colors = Some(colors.clone());
        self.cursor = cursor;
        self.cell_width = Some(metrics.cell_width);
        self.line_height = Some(metrics.line_height);
        self.scale_factor_bits = Some(metrics.scale_factor.to_bits());
        Arc::clone(&self.prepared_rows)
    }
}

pub(crate) struct TerminalGridElement {
    background: Color,
    foreground: Color,
    rows: Arc<[Arc<RowPaintInput>]>,
    columns: usize,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    cursor: Option<(CursorPositionSnapshot, CellSnapshot)>,
    cursor_style: CursorSnapshot,
    font_family: SharedString,
    preedit: Option<PreeditLayout>,
    focus_handle: FocusHandle,
    input: Entity<TerminalPane>,
    text_blink_visible: bool,
}

pub(crate) struct TerminalGridConfiguration {
    pub(crate) terminal_input_focused: bool,
    pub(crate) font_family: SharedString,
    pub(crate) font_size: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) cell_width: Pixels,
    pub(crate) preedit: Option<PreeditLayout>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) input: Entity<TerminalPane>,
    pub(crate) text_blink_visible: bool,
    pub(crate) scale_factor: f32,
}

impl TerminalGridElement {
    pub(crate) fn new(
        screen: &Arc<ScreenSnapshot>,
        cache: &mut TerminalGridCache,
        configuration: TerminalGridConfiguration,
    ) -> Self {
        let cursor = screen.cursor.position.and_then(|position| {
            screen
                .rows
                .get(usize::from(position.row))
                .and_then(|row| row.get(usize::from(position.column)))
                .cloned()
                .map(|cell| (position, cell))
        });
        let cursor_style =
            presented_cursor_style(screen.cursor, configuration.terminal_input_focused);
        Self {
            background: screen.background,
            foreground: if screen.colors.reversed {
                screen.colors.background
            } else {
                screen.colors.foreground
            },
            rows: cache.prepare(
                &screen.rows,
                &screen.colors,
                &configuration.font_family,
                screen.cursor.position,
                TerminalGridMetrics {
                    cell_width: configuration.cell_width,
                    line_height: configuration.line_height,
                    scale_factor: configuration.scale_factor,
                },
            ),
            columns: screen.rows.first().map_or(0, |row| row.len()),
            font_size: configuration.font_size,
            line_height: configuration.line_height,
            cell_width: configuration.cell_width,
            cursor,
            cursor_style,
            font_family: configuration.font_family,
            preedit: configuration.preedit,
            focus_handle: configuration.focus_handle,
            input: configuration.input,
            text_blink_visible: configuration.text_blink_visible,
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
    symbols: PreparedDecorations,
    overlay_symbols: PreparedDecorations,
    backgrounds: Vec<PaintQuad>,
    under_text_decorations: PreparedDecorations,
    over_text_decorations: PreparedDecorations,
    overlay_text: Vec<PreparedText>,
    overlay_backgrounds: Vec<PaintQuad>,
    overlay_caret: Option<PaintQuad>,
}

pub(crate) struct PrepaintState {
    surface: Option<PaintQuad>,
    rows: Vec<PreparedRow>,
}

#[derive(Default)]
struct PreparedDecorations {
    quads: Vec<PaintQuad>,
    paths: Vec<(Path<Pixels>, Color)>,
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
        let base_font = terminal_cell_font(&self.font_family, false, false);
        let font_id = window.text_system().resolve_font(&base_font);
        let baseline =
            window
                .text_system()
                .baseline_offset(font_id, self.font_size, self.line_height);
        let ascent = window.text_system().ascent(font_id, self.font_size);
        let x_height = window.text_system().x_height(font_id, self.font_size);
        let decoration_metrics =
            decoration_metrics(baseline, ascent, x_height, window.scale_factor());

        for (row_index, row) in self.rows.iter().take(visible_rows).enumerate() {
            let row_top = bounds.top() + self.line_height * row_index as f32;
            let text = row
                .fragments
                .iter()
                .filter(|fragment| {
                    text_fragment_visible(fragment.blinking, self.text_blink_visible)
                })
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
            let under_text_decorations = prepare_decoration_geometry(
                &row.under_text_decorations,
                row_top,
                grid_left,
                self.cell_width,
                decoration_metrics,
                self.text_blink_visible,
            );
            let over_text_decorations = prepare_decoration_geometry(
                &row.over_text_decorations,
                row_top,
                grid_left,
                self.cell_width,
                decoration_metrics,
                self.text_blink_visible,
            );
            let symbols = prepare_symbol_geometry(
                &row.symbols,
                row_top,
                grid_left,
                self.cell_width,
                self.text_blink_visible,
            );

            let mut text: Vec<PreparedText> = text;
            let mut backgrounds = backgrounds;
            let mut overlay_text = Vec::new();
            let mut overlay_symbols = PreparedDecorations::default();
            let mut overlay_backgrounds = Vec::new();
            let mut overlay_caret = None;
            if let Some(preedit) = &self.preedit {
                for cluster in preedit
                    .clusters
                    .iter()
                    .filter(|cluster| cluster.row == row_index)
                {
                    let cluster_left = grid_left + self.cell_width * cluster.column as f32;
                    let width_cells = usize::from(cluster.width).max(1);
                    overlay_backgrounds.push(fill(
                        Bounds::new(
                            point(cluster_left, row_top),
                            size(self.cell_width * width_cells as f32, self.line_height),
                        ),
                        gpui_color(self.background),
                    ));
                    let color = gpui_color(self.foreground).into();
                    overlay_text.push(PreparedText {
                        line: window.text_system().shape_line(
                            cluster.text.clone().into(),
                            self.font_size,
                            &[TextRun {
                                len: cluster.text.len(),
                                font: terminal_cell_font(&self.font_family, false, false),
                                color,
                                background_color: None,
                                underline: Some(UnderlineStyle {
                                    thickness: px(1.0),
                                    color: Some(color),
                                    wavy: false,
                                }),
                                strikethrough: None,
                            }],
                            None,
                        ),
                        origin: point(cluster_left, row_top),
                    });
                }
                if preedit.caret.row == row_index {
                    let caret_left = grid_left + self.cell_width * preedit.caret.column as f32;
                    overlay_caret = Some(fill(
                        Bounds::new(point(caret_left, row_top), size(px(1.0), self.line_height)),
                        gpui_color(self.cursor_style.color),
                    ));
                }
            } else if self.cursor_style.visible
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

                if plan.recolor_text
                    && !cell.spacer_tail
                    && !cell.invisible
                    && text_fragment_visible(cell.blinking, self.text_blink_visible)
                {
                    if let Some(symbol) = row
                        .symbols
                        .iter()
                        .find(|symbol| symbol.start == usize::from(position.column))
                    {
                        let mut symbol = symbol.clone();
                        symbol.color = self.cursor_style.text_color;
                        overlay_symbols = prepare_symbol_geometry(
                            &[symbol],
                            row_top,
                            grid_left,
                            self.cell_width,
                            self.text_blink_visible,
                        );
                    } else {
                        let cursor_font =
                            terminal_cell_font(&self.font_family, cell.bold, cell.italic);
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
                                force_cell_width_for_cell(&cell.text, position.width_cells)
                                    .then_some(self.cell_width),
                            ),
                            origin: point(cursor_left, row_top),
                        });
                    }
                }
            }

            prepared_rows.push(PreparedRow {
                text,
                symbols,
                overlay_symbols,
                backgrounds,
                under_text_decorations,
                over_text_decorations,
                overlay_text,
                overlay_backgrounds,
                overlay_caret,
            });
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
        if let Some(surface) = prepaint.surface.take() {
            window.paint_quad(surface);
        }
        let grid_bounds = terminal_grid_content_bounds(bounds, self.columns, self.cell_width);
        let grid_bounds = Bounds::new(
            grid_bounds.origin,
            size(
                grid_bounds.size.width,
                (self.line_height * prepaint.rows.len() as f32).min(grid_bounds.size.height),
            ),
        );
        window.with_content_mask(
            Some(ContentMask {
                bounds: grid_bounds,
            }),
            |window| {
                for row in &mut prepaint.rows {
                    for background in row.backgrounds.drain(..) {
                        window.paint_quad(background);
                    }
                    for quad in row.under_text_decorations.quads.drain(..) {
                        window.paint_quad(quad);
                    }
                    for (path, color) in row.under_text_decorations.paths.drain(..) {
                        window.paint_path(path, gpui_color(color));
                    }
                    for quad in row.symbols.quads.drain(..) {
                        window.paint_quad(quad);
                    }
                    for (path, color) in row.symbols.paths.drain(..) {
                        window.paint_path(path, gpui_color(color));
                    }
                    for text in row.text.drain(..) {
                        if let Err(error) =
                            text.line.paint(text.origin, self.line_height, window, cx)
                        {
                            eprintln!("failed to paint terminal row: {error:#}");
                        }
                    }
                    for quad in row.overlay_symbols.quads.drain(..) {
                        window.paint_quad(quad);
                    }
                    for (path, color) in row.overlay_symbols.paths.drain(..) {
                        window.paint_path(path, gpui_color(color));
                    }
                    for quad in row.over_text_decorations.quads.drain(..) {
                        window.paint_quad(quad);
                    }
                    for (path, color) in row.over_text_decorations.paths.drain(..) {
                        window.paint_path(path, gpui_color(color));
                    }
                    for background in row.overlay_backgrounds.drain(..) {
                        window.paint_quad(background);
                    }
                    for text in row.overlay_text.drain(..) {
                        if let Err(error) =
                            text.line.paint(text.origin, self.line_height, window, cx)
                        {
                            eprintln!("failed to paint marked terminal text: {error:#}");
                        }
                    }
                    if let Some(caret) = row.overlay_caret.take() {
                        window.paint_quad(caret);
                    }
                }
            },
        );
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
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
    symbols: Vec<SymbolPaintInput>,
    backgrounds: Vec<BackgroundSpan>,
    selections: Vec<BackgroundSpan>,
    under_text_decorations: Vec<DecorationSpan>,
    over_text_decorations: Vec<DecorationSpan>,
}

#[derive(Clone, Debug)]
struct SymbolPaintInput {
    start: usize,
    width_cells: u8,
    color: Color,
    blinking: bool,
    plan: Arc<SymbolPlan>,
}

struct TextFragment {
    start: usize,
    text: SharedString,
    runs: Vec<TextRun>,
    force_cell_width: bool,
    blinking: bool,
}

struct FragmentBuilder {
    start: usize,
    selected: bool,
    cursor: bool,
    blinking: bool,
    text: String,
    runs: Vec<TextRun>,
}

impl FragmentBuilder {
    fn new(start: usize, selected: bool, cursor: bool, blinking: bool) -> Self {
        Self {
            start,
            selected,
            cursor,
            blinking,
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
        let cell_font = terminal_cell_font(font_family, cell.bold, cell.italic);
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
            blinking: self.blinking,
        }
    }
}

pub(super) fn terminal_cell_font(family: &SharedString, bold: bool, italic: bool) -> Font {
    let mut cell_font = font(family.clone());
    cell_font.features = FontFeatures::disable_ligatures();
    cell_font.fallbacks = Some(FontFallbacks::from_fonts(
        ["Apple Color Emoji", "Menlo"]
            .into_iter()
            .filter(|fallback| !family.as_ref().eq_ignore_ascii_case(fallback))
            .map(str::to_owned)
            .collect(),
    ));
    if bold {
        cell_font = cell_font.bold();
    }
    if italic {
        cell_font = cell_font.italic();
    }
    cell_font
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackgroundSpan {
    start: usize,
    len: usize,
    color: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecorationKind {
    Underline(TerminalUnderlineSnapshot),
    Overline,
    Strikethrough,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DecorationSpan {
    start: usize,
    len: usize,
    kind: DecorationKind,
    color: Color,
    blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DecorationMetrics {
    device_pixel: Pixels,
    thickness: Pixels,
    underline_y: Pixels,
    double_underline_y: Pixels,
    strikethrough_y: Pixels,
    overline_y: Pixels,
    wave_amplitude: Pixels,
}

fn decoration_metrics(
    baseline: Pixels,
    ascent: Pixels,
    x_height: Pixels,
    scale_factor: f32,
) -> DecorationMetrics {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let device_pixel = px(1.0 / scale_factor);
    let snap = |value: Pixels| px((f32::from(value) * scale_factor).round() / scale_factor);
    let underline_y = snap(baseline + device_pixel * 2.0);
    DecorationMetrics {
        device_pixel,
        thickness: device_pixel,
        underline_y,
        double_underline_y: underline_y + device_pixel * 2.0,
        strikethrough_y: snap(baseline - x_height / 2.0),
        overline_y: snap(baseline - ascent),
        wave_amplitude: device_pixel * 2.0,
    }
}

fn prepare_decoration_geometry(
    spans: &[DecorationSpan],
    row_top: Pixels,
    grid_left: Pixels,
    cell_width: Pixels,
    metrics: DecorationMetrics,
    blink_phase_visible: bool,
) -> PreparedDecorations {
    let mut prepared = PreparedDecorations::default();
    for span in spans
        .iter()
        .filter(|span| text_fragment_visible(span.blinking, blink_phase_visible))
    {
        let left = grid_left + cell_width * span.start as f32;
        let right = left + cell_width * span.len as f32;
        let width = right - left;
        let mut push_line = |y: Pixels| {
            prepared.quads.push(fill(
                Bounds::new(point(left, row_top + y), size(width, metrics.thickness)),
                gpui_color(span.color),
            ));
        };

        match span.kind {
            DecorationKind::Underline(TerminalUnderlineSnapshot::Single) => {
                push_line(metrics.underline_y);
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Double) => {
                push_line(metrics.underline_y);
                push_line(metrics.double_underline_y);
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Dotted) => {
                let mut x = left;
                while x < right {
                    let dot_width = metrics.thickness.min(right - x);
                    prepared.quads.push(fill(
                        Bounds::new(
                            point(x, row_top + metrics.underline_y),
                            size(dot_width, metrics.thickness),
                        ),
                        gpui_color(span.color),
                    ));
                    x += metrics.device_pixel * 2.0;
                }
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Dashed) => {
                let dash_width = metrics.device_pixel * 3.0;
                let mut x = left;
                while x < right {
                    let width = dash_width.min(right - x);
                    prepared.quads.push(fill(
                        Bounds::new(
                            point(x, row_top + metrics.underline_y),
                            size(width, metrics.thickness),
                        ),
                        gpui_color(span.color),
                    ));
                    x += dash_width + metrics.device_pixel * 2.0;
                }
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Curly) => {
                let mut builder = PathBuilder::stroke(metrics.thickness);
                let center_y = row_top + metrics.underline_y + metrics.wave_amplitude;
                let half_wave = (cell_width / 4.0).max(metrics.device_pixel * 2.0);
                let mut x = left;
                let mut rise = true;
                builder.move_to(point(x, center_y));
                while x < right {
                    x = (x + half_wave).min(right);
                    let y = if rise {
                        center_y - metrics.wave_amplitude
                    } else {
                        center_y + metrics.wave_amplitude
                    };
                    builder.line_to(point(x, y));
                    rise = !rise;
                }
                if let Ok(path) = builder.build() {
                    prepared.paths.push((path, span.color));
                }
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::None) => {}
            DecorationKind::Overline => push_line(metrics.overline_y),
            DecorationKind::Strikethrough => push_line(metrics.strikethrough_y),
        }
    }
    prepared
}

fn prepare_symbol_geometry(
    symbols: &[SymbolPaintInput],
    row_top: Pixels,
    grid_left: Pixels,
    cell_width: Pixels,
    blink_phase_visible: bool,
) -> PreparedDecorations {
    let mut prepared = PreparedDecorations::default();
    for symbol in symbols
        .iter()
        .filter(|symbol| text_fragment_visible(symbol.blinking, blink_phase_visible))
    {
        debug_assert!(matches!(symbol.width_cells, 1 | 2));
        let scale = symbol.plan.scale_factor;
        let origin = point(grid_left + cell_width * symbol.start as f32, row_top);
        for primitive in &symbol.plan.primitives {
            match primitive {
                SymbolPrimitive::Rect {
                    x,
                    y,
                    width,
                    height,
                    alpha,
                } => prepared.quads.push(fill(
                    Bounds::new(
                        point(
                            origin.x + px(f32::from(*x) / scale),
                            origin.y + px(f32::from(*y) / scale),
                        ),
                        size(
                            px(f32::from(*width) / scale),
                            px(f32::from(*height) / scale),
                        ),
                    ),
                    gpui_color(symbol_color_with_alpha(symbol.color, *alpha)),
                )),
                SymbolPrimitive::Polygon { points, alpha } => {
                    let mut points = points.iter();
                    let Some(first) = points.next() else {
                        continue;
                    };
                    let mut builder = PathBuilder::fill();
                    builder.move_to(point(
                        origin.x + px(first.x / scale),
                        origin.y + px(first.y / scale),
                    ));
                    for vertex in points {
                        builder.line_to(point(
                            origin.x + px(vertex.x / scale),
                            origin.y + px(vertex.y / scale),
                        ));
                    }
                    builder.close();
                    if let Ok(path) = builder.build() {
                        prepared
                            .paths
                            .push((path, symbol_color_with_alpha(symbol.color, *alpha)));
                    }
                }
                SymbolPrimitive::Stroke {
                    points,
                    thickness,
                    alpha,
                } => {
                    let mut points = points.iter();
                    let Some(first) = points.next() else {
                        continue;
                    };
                    let mut builder = PathBuilder::stroke(px(f32::from(*thickness) / scale));
                    builder.move_to(point(
                        origin.x + px(first.x / scale),
                        origin.y + px(first.y / scale),
                    ));
                    for vertex in points {
                        builder.line_to(point(
                            origin.x + px(vertex.x / scale),
                            origin.y + px(vertex.y / scale),
                        ));
                    }
                    if let Ok(path) = builder.build() {
                        prepared
                            .paths
                            .push((path, symbol_color_with_alpha(symbol.color, *alpha)));
                    }
                }
            }
        }
    }
    prepared
}

fn symbol_color_with_alpha(mut color: Color, primitive_alpha: u8) -> Color {
    color.a = ((u16::from(color.a) * u16::from(primitive_alpha) + 127) / 255) as u8;
    color
}

fn push_decoration(spans: &mut Vec<DecorationSpan>, mut span: DecorationSpan) {
    if let Some(previous) = spans.last_mut()
        && previous.start + previous.len == span.start
        && previous.kind == span.kind
        && previous.color == span.color
        && previous.blinking == span.blinking
    {
        previous.len += span.len;
        return;
    }
    span.len = span.len.max(1);
    spans.push(span);
}

#[cfg(test)]
fn prepare_row(
    row: &RowSnapshot,
    colors: &TerminalColorsSnapshot,
    font_family: &SharedString,
    cursor_column: Option<usize>,
) -> RowPaintInput {
    prepare_row_cached(
        row,
        colors,
        font_family,
        cursor_column,
        &mut SymbolPlanCache::default(),
        TerminalGridMetrics {
            cell_width: px(8.0),
            line_height: px(20.0),
            scale_factor: 1.0,
        },
    )
}

fn prepare_row_cached(
    row: &RowSnapshot,
    colors: &TerminalColorsSnapshot,
    font_family: &SharedString,
    cursor_column: Option<usize>,
    symbol_plans: &mut SymbolPlanCache,
    metrics: TerminalGridMetrics,
) -> RowPaintInput {
    let mut fragments = Vec::new();
    let mut symbols = Vec::new();
    let mut regular_fragment: Option<FragmentBuilder> = None;
    let mut backgrounds: Vec<BackgroundSpan> = Vec::new();
    let mut selections: Vec<BackgroundSpan> = Vec::new();
    let mut underlines = Vec::new();
    let mut overlines = Vec::new();
    let mut strikethroughs = Vec::new();

    for (column, cell) in row.iter().enumerate() {
        let cursor = cursor_column == Some(column);
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

        if !cell.invisible {
            let (foreground, _) = effective_colors(cell, colors);
            if cell.underline != TerminalUnderlineSnapshot::None {
                push_decoration(
                    &mut underlines,
                    DecorationSpan {
                        start: column,
                        len: 1,
                        kind: DecorationKind::Underline(cell.underline),
                        color: effective_underline_color(cell, colors, foreground),
                        blinking: cell.blinking,
                    },
                );
            }
            if cell.overline {
                push_decoration(
                    &mut overlines,
                    DecorationSpan {
                        start: column,
                        len: 1,
                        kind: DecorationKind::Overline,
                        color: foreground,
                        blinking: cell.blinking,
                    },
                );
            }
            if cell.strikethrough {
                push_decoration(
                    &mut strikethroughs,
                    DecorationSpan {
                        start: column,
                        len: 1,
                        kind: DecorationKind::Strikethrough,
                        color: foreground,
                        blinking: cell.blinking,
                    },
                );
            }
        }

        if cell.spacer_tail || cell.invisible {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            continue;
        }

        let is_wide_head = row.get(column + 1).is_some_and(|next| next.spacer_tail);
        let width_cells = if is_wide_head { 2 } else { 1 };
        if let Some(symbol) = terminal_symbol(&cell.text) {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            let (color, _) = effective_colors(cell, colors);
            symbols.push(SymbolPaintInput {
                start: column,
                width_cells,
                color,
                blinking: cell.blinking,
                plan: symbol_plans.get(
                    symbol,
                    f32::from(metrics.cell_width),
                    f32::from(metrics.line_height),
                    width_cells,
                    metrics.scale_factor,
                ),
            });
            continue;
        }

        if regular_fragment.as_ref().is_some_and(|fragment| {
            fragment.selected != cell.selected
                || fragment.cursor != cursor
                || fragment.blinking != cell.blinking
        }) && let Some(fragment) = regular_fragment.take()
        {
            fragments.push(fragment.finish(true));
        }

        let requires_whole_cell_shaping = !force_cell_width_for_cell(&cell.text, width_cells);
        if requires_whole_cell_shaping {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            let mut fragment = FragmentBuilder::new(column, cell.selected, cursor, cell.blinking);
            fragment.push(cell, colors, font_family);
            fragments.push(fragment.finish(false));
        } else {
            regular_fragment
                .get_or_insert_with(|| {
                    FragmentBuilder::new(column, cell.selected, cursor, cell.blinking)
                })
                .push(cell, colors, font_family);
        }
    }

    if let Some(fragment) = regular_fragment {
        fragments.push(fragment.finish(true));
    }

    underlines.extend(overlines);
    RowPaintInput {
        fragments,
        symbols,
        backgrounds,
        selections,
        under_text_decorations: underlines,
        over_text_decorations: strikethroughs,
    }
}

fn is_bidi_sensitive(character: char) -> bool {
    matches!(
        bidi_class(character),
        BidiClass::R
            | BidiClass::AL
            | BidiClass::AN
            | BidiClass::NSM
            | BidiClass::RLE
            | BidiClass::RLO
            | BidiClass::RLI
            | BidiClass::LRE
            | BidiClass::LRO
            | BidiClass::LRI
            | BidiClass::FSI
            | BidiClass::PDI
            | BidiClass::PDF
    )
}

fn force_cell_width_for_cell(text: &str, width_cells: u8) -> bool {
    width_cells == 1 && text.chars().count() == 1 && !text.chars().any(is_bidi_sensitive)
}

fn text_fragment_visible(blinking: bool, blink_phase_visible: bool) -> bool {
    !blinking || blink_phase_visible
}

fn cursor_column_for_row(cursor: Option<CursorPositionSnapshot>, row: usize) -> Option<usize> {
    cursor
        .filter(|cursor| usize::from(cursor.row) == row)
        .map(|cursor| usize::from(cursor.column))
}

fn effective_colors(cell: &CellSnapshot, colors: &TerminalColorsSnapshot) -> (Color, Color) {
    let foreground_source = match (cell.foreground_source, cell.bold) {
        (TerminalColor::Palette(index @ 0..=7), true) => TerminalColor::Palette(index + 8),
        (source, _) => source,
    };
    let mut foreground = resolve_color_source(foreground_source, colors, colors.foreground);
    let mut background = resolve_color_source(cell.background_source, colors, colors.background);
    if cell.inverse ^ colors.reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.faint {
        foreground.a = foreground.a.div_ceil(2);
    }
    (foreground, background)
}

fn effective_underline_color(
    cell: &CellSnapshot,
    colors: &TerminalColorsSnapshot,
    effective_foreground: Color,
) -> Color {
    let mut color = resolve_color_source(cell.underline_source, colors, effective_foreground);
    if cell.faint && cell.underline_source != TerminalColor::Default {
        color.a = color.a.div_ceil(2);
    }
    color
}

fn resolve_color_source(
    source: TerminalColor,
    colors: &TerminalColorsSnapshot,
    default: Color,
) -> Color {
    match source {
        TerminalColor::Default => default,
        TerminalColor::Palette(index) => colors.palette[usize::from(index)],
        TerminalColor::Rgb(color) => color,
    }
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
            faint: false,
            italic: false,
            blinking: false,
            invisible: false,
            underline: crate::terminal::TerminalUnderlineSnapshot::None,
            underline_source: crate::terminal::TerminalColor::Default,
            strikethrough: false,
            overline: false,
            selected: false,
            spacer_tail: false,
        }
    }

    fn grid_metrics() -> TerminalGridMetrics {
        TerminalGridMetrics {
            cell_width: px(8.0),
            line_height: px(20.0),
            scale_factor: 1.0,
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
    fn faint_reduces_only_the_resolved_foreground_opacity() {
        let colors = colors();
        let mut subject = cell("x");
        subject.foreground_source = crate::terminal::TerminalColor::Rgb(Color::rgba(0x10_20_30_c0));
        subject.background_source = crate::terminal::TerminalColor::Rgb(Color::rgb(0x40_50_60));
        subject.inverse = true;
        subject.faint = true;

        let (foreground, background) = effective_colors(&subject, &colors);

        assert_eq!(foreground, Color::rgba(0x40_50_60_80));
        assert_eq!(background, Color::rgba(0x10_20_30_c0));
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
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

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
    fn terminal_text_runs_configure_emoji_and_system_fallbacks() {
        let row = Arc::<[CellSnapshot]>::from([cell("A")]);

        let input = prepare_row(&row, &colors(), &"JetBrains Mono".into(), None);

        assert_eq!(
            input.fragments[0].runs[0]
                .font
                .fallbacks
                .as_ref()
                .expect("terminal text must carry an explicit fallback cascade")
                .fallback_list(),
            ["Apple Color Emoji", "Menlo"]
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

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

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
    fn invisible_cells_keep_backgrounds_without_preparing_foreground_text() {
        let accent = ACTIVE_THEME.terminal_normal()[1];
        let mut invisible = cell("secret");
        invisible.invisible = true;
        invisible.background_source = crate::terminal::TerminalColor::Rgb(accent);
        let row = Arc::<[CellSnapshot]>::from([invisible]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert!(input.fragments.is_empty());
        assert_eq!(
            input.backgrounds,
            vec![BackgroundSpan {
                start: 0,
                len: 1,
                color: accent,
            }]
        );
    }

    #[test]
    fn prepared_decorations_preserve_kind_color_layer_and_cell_span() {
        let accent = ACTIVE_THEME.terminal_normal()[1];
        let mut decorated = cell("界");
        decorated.underline = crate::terminal::TerminalUnderlineSnapshot::Double;
        decorated.underline_source = crate::terminal::TerminalColor::Rgb(accent);
        decorated.overline = true;
        decorated.strikethrough = true;
        let mut tail = decorated.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([decorated, tail]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(
            input.under_text_decorations,
            vec![
                DecorationSpan {
                    start: 0,
                    len: 2,
                    kind: DecorationKind::Underline(
                        crate::terminal::TerminalUnderlineSnapshot::Double,
                    ),
                    color: accent,
                    blinking: false,
                },
                DecorationSpan {
                    start: 0,
                    len: 2,
                    kind: DecorationKind::Overline,
                    color: colors().foreground,
                    blinking: false,
                },
            ]
        );
        assert_eq!(
            input.over_text_decorations,
            vec![DecorationSpan {
                start: 0,
                len: 2,
                kind: DecorationKind::Strikethrough,
                color: colors().foreground,
                blinking: false,
            }]
        );
    }

    #[test]
    fn decorations_compose_with_inverse_selection_visibility_and_blink_phase() {
        let mut decorated = cell("x");
        decorated.inverse = true;
        decorated.selected = true;
        decorated.blinking = true;
        decorated.underline = crate::terminal::TerminalUnderlineSnapshot::Single;
        decorated.overline = true;
        decorated.strikethrough = true;
        let row = Arc::<[CellSnapshot]>::from([decorated.clone()]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(input.selections.len(), 1);
        assert!(
            input
                .under_text_decorations
                .iter()
                .chain(&input.over_text_decorations)
                .all(|span| span.color == colors().background && span.blinking)
        );
        let metrics = decoration_metrics(px(15.0), px(11.0), px(8.0), 2.0);
        let hidden = prepare_decoration_geometry(
            &input.under_text_decorations,
            px(0.0),
            px(0.0),
            px(8.0),
            metrics,
            false,
        );
        assert!(hidden.quads.is_empty() && hidden.paths.is_empty());

        decorated.invisible = true;
        let invisible = Arc::<[CellSnapshot]>::from([decorated]);
        let invisible = prepare_row(&invisible, &colors(), &"Menlo".into(), None);
        assert!(invisible.under_text_decorations.is_empty());
        assert!(invisible.over_text_decorations.is_empty());
        assert_eq!(invisible.selections.len(), 1);
    }

    #[test]
    fn decoration_metrics_follow_font_metrics_and_snap_to_retina_pixels() {
        let metrics = decoration_metrics(px(15.2), px(11.1), px(8.2), 2.0);

        assert_eq!(metrics.device_pixel, px(0.5));
        assert_eq!(metrics.thickness, px(0.5));
        assert_eq!(metrics.underline_y, px(16.0));
        assert_eq!(metrics.double_underline_y, px(17.0));
        assert_eq!(metrics.strikethrough_y, px(11.0));
        assert_eq!(metrics.overline_y, px(4.0));
        assert!(metrics.wave_amplitude >= metrics.device_pixel);
    }

    #[test]
    fn underline_variants_produce_distinct_cell_clipped_geometry() {
        let metrics = decoration_metrics(px(15.0), px(11.0), px(8.0), 2.0);
        let span = |kind| DecorationSpan {
            start: 0,
            len: 2,
            kind: DecorationKind::Underline(kind),
            color: Color::rgb(0x12_34_56),
            blinking: false,
        };
        let prepare = |kind| {
            prepare_decoration_geometry(&[span(kind)], px(0.0), px(0.0), px(8.0), metrics, true)
        };

        let single = prepare(crate::terminal::TerminalUnderlineSnapshot::Single);
        let double = prepare(crate::terminal::TerminalUnderlineSnapshot::Double);
        let curly = prepare(crate::terminal::TerminalUnderlineSnapshot::Curly);
        let dotted = prepare(crate::terminal::TerminalUnderlineSnapshot::Dotted);
        let dashed = prepare(crate::terminal::TerminalUnderlineSnapshot::Dashed);

        assert_eq!((single.quads.len(), single.paths.len()), (1, 0));
        assert_eq!((double.quads.len(), double.paths.len()), (2, 0));
        assert_eq!((curly.quads.len(), curly.paths.len()), (0, 1));
        assert!(dotted.quads.len() > dashed.quads.len());
        assert!(dashed.quads.len() > single.quads.len());
        assert!(
            single
                .quads
                .iter()
                .all(|quad| quad.bounds.right() <= px(16.0))
        );
        assert!(
            double
                .quads
                .iter()
                .all(|quad| quad.bounds.right() <= px(16.0))
        );
    }

    #[test]
    fn selected_cells_coalesce_into_themed_overlay_spans() {
        let mut first = cell("a");
        first.selected = true;
        let mut second = cell("b");
        second.selected = true;
        let row = Arc::<[CellSnapshot]>::from([first, second, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

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
    fn shaping_fragments_do_not_cross_selection_boundaries() {
        let mut selected = cell("b");
        selected.selected = true;
        let row = Arc::<[CellSnapshot]>::from([cell("a"), selected, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| fragment.text.as_ref())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn shaping_fragments_do_not_cross_cursor_boundaries() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("b"), cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), Some(1));

        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| fragment.text.as_ref())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn shaping_fragments_do_not_cross_text_blink_boundaries() {
        let mut blinking = cell("b");
        blinking.blinking = true;
        let row = Arc::<[CellSnapshot]>::from([cell("a"), blinking, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| (fragment.text.as_ref(), fragment.blinking))
                .collect::<Vec<_>>(),
            vec![("a", false), ("b", true), ("c", false)]
        );
    }

    #[test]
    fn text_blink_phase_hides_only_blinking_fragments() {
        assert!(text_fragment_visible(false, false));
        assert!(text_fragment_visible(false, true));
        assert!(!text_fragment_visible(true, false));
        assert!(text_fragment_visible(true, true));
    }

    #[test]
    fn wide_and_combining_cells_anchor_following_text_to_columns() {
        let mut tail = cell(" ");
        tail.spacer_tail = true;
        let row =
            Arc::<[CellSnapshot]>::from([cell("界"), tail, cell("x"), cell("e\u{301}"), cell("y")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

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
    fn supported_symbols_bypass_shaping_without_capturing_grapheme_sequences() {
        let box_line = cell("─");
        let variation_sequence = cell("─\u{fe0f}");
        let mut wide_block = cell("█");
        wide_block.inverse = true;
        wide_block.selected = true;
        let mut tail = wide_block.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([box_line, variation_sequence, wide_block, tail]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(
            input
                .symbols
                .iter()
                .map(|symbol| (symbol.start, symbol.width_cells, symbol.color))
                .collect::<Vec<_>>(),
            vec![(0, 1, colors().foreground), (2, 2, colors().background)]
        );
        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| (fragment.start, fragment.text.as_ref()))
                .collect::<Vec<_>>(),
            vec![(1, "─\u{fe0f}")]
        );
        assert_eq!(input.selections.len(), 1);
    }

    #[test]
    fn symbol_prepaint_obeys_blink_phase_and_preserves_terminal_width() {
        let mut block = cell("█");
        block.blinking = true;
        let mut tail = block.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([block, tail]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        let hidden = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.0), false);
        let visible = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.0), true);

        assert!(hidden.quads.is_empty() && hidden.paths.is_empty());
        assert_eq!(visible.quads.len(), 1);
        assert_eq!(
            visible.quads[0].bounds,
            Bounds::new(point(px(5.0), px(10.0)), size(px(16.0), px(20.0)))
        );
    }

    #[test]
    fn symbol_prepaint_uses_fractional_logical_cell_origins() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("a"), cell("█")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        let visible = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.25), true);

        assert_eq!(visible.quads[0].bounds.origin.x, px(21.5));
    }

    #[test]
    fn right_to_left_cells_keep_terminal_cell_order() {
        let row = Arc::<[CellSnapshot]>::from([cell("א"), cell("ב"), cell("ג")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| (fragment.start, fragment.text.as_ref()))
                .collect::<Vec<_>>(),
            vec![(0, "א"), (1, "ב"), (2, "ג")]
        );
        assert!(
            input
                .fragments
                .iter()
                .all(|fragment| !fragment.force_cell_width)
        );
    }

    #[test]
    fn width_constraints_only_apply_to_simple_narrow_cells() {
        assert!(force_cell_width_for_cell("a", 1));
        assert!(!force_cell_width_for_cell("界", 2));
        assert!(!force_cell_width_for_cell("e\u{301}", 1));
        assert!(!force_cell_width_for_cell("\u{2764}\u{fe0f}", 2));
        assert!(!force_cell_width_for_cell("👩\u{200d}💻", 2));
        assert!(!force_cell_width_for_cell("א", 1));
    }

    #[test]
    fn render_cache_reuses_unchanged_prepared_rows() {
        let first_row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let second_row = Arc::<[CellSnapshot]>::from([cell("b")]);
        let rows = Arc::<[RowSnapshot]>::from([Arc::clone(&first_row), Arc::clone(&second_row)]);
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(&rows, &colors(), &"Menlo".into(), None, grid_metrics());

        let changed_row = Arc::<[CellSnapshot]>::from([cell("c")]);
        let changed_rows = Arc::<[RowSnapshot]>::from([first_row, changed_row]);
        let second = cache.prepare(
            &changed_rows,
            &colors(),
            &"Menlo".into(),
            None,
            grid_metrics(),
        );

        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert!(!Arc::ptr_eq(&first[1], &second[1]));
    }

    #[test]
    fn cursor_movement_only_invalidates_affected_prepared_rows() {
        let rows = Arc::<[RowSnapshot]>::from([
            Arc::<[CellSnapshot]>::from([cell("a"), cell("b")]),
            Arc::<[CellSnapshot]>::from([cell("c"), cell("d")]),
            Arc::<[CellSnapshot]>::from([cell("e"), cell("f")]),
        ]);
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(
            &rows,
            &colors(),
            &"Menlo".into(),
            Some(CursorPositionSnapshot {
                row: 0,
                column: 1,
                width_cells: 1,
            }),
            grid_metrics(),
        );

        let second = cache.prepare(
            &rows,
            &colors(),
            &"Menlo".into(),
            Some(CursorPositionSnapshot {
                row: 1,
                column: 0,
                width_cells: 1,
            }),
            grid_metrics(),
        );

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
        assert!(!Arc::ptr_eq(&first[1], &second[1]));
        assert!(Arc::ptr_eq(&first[2], &second[2]));
    }

    #[test]
    fn render_cache_invalidates_rows_when_color_semantics_change() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let first_colors = colors();
        let first = cache.prepare(&rows, &first_colors, &"Menlo".into(), None, grid_metrics());

        let mut changed_colors = first_colors.clone();
        Arc::make_mut(&mut changed_colors.palette)[1] = Color::rgb(0xff_00_00);
        let second = cache.prepare(
            &rows,
            &changed_colors,
            &"Menlo".into(),
            None,
            grid_metrics(),
        );

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }

    #[test]
    fn scale_invalidation_should_discard_prepared_terminal_rows() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(&rows, &colors(), &"Menlo".into(), None, grid_metrics());

        cache.invalidate_scale_dependent();
        let second = cache.prepare(&rows, &colors(), &"Menlo".into(), None, grid_metrics());

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }
}
