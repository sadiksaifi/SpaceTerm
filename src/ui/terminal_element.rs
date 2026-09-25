use std::sync::Arc;

mod presentation;
pub(crate) use presentation::TerminalGridPresentation;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, Font, GlobalElementId, GlyphPaintRegion, Hsla, InspectorElementId, IntoElement,
    LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Style, TextRun, UnderlineStyle, Window,
    fill, outline, point, px, relative, rgba, size,
};
#[cfg(test)]
use gpui::{FontFallbacks, FontFeatures, font};
use unicode_bidi::{BidiClass, bidi_class};

use crate::appearance::{Color, ResolvedTerminalAppearance, TerminalColors};
use crate::terminal::geometry::{
    CellGridPosition, CellGridSize, LogicalCellSize, LogicalSize, TerminalGeometry,
};
use crate::terminal::{
    CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, FindHighlightSpan,
    RowSnapshot, ScreenSnapshot, TerminalColor, TerminalColorsSnapshot, TerminalDefaultColorSource,
    TerminalUnderlineSnapshot,
};

use super::appearance::TerminalFonts;
use super::terminal_graphics::{
    GraphicsAttemptToken, GraphicsLayer, GraphicsPaintPlan, PreparedGraphics, TerminalGraphicsCache,
};
use super::terminal_ime::PreeditLayout;
use super::terminal_pane::{OperationToken, TerminalPane};
use super::terminal_symbols::{
    DevicePoint, SymbolPlanCache, SymbolPrimitive, TerminalSymbol, terminal_symbol,
};

#[derive(Clone, Copy)]
struct TerminalGridMetrics {
    cell_width: Pixels,
    line_height: Pixels,
    scale_factor: f32,
}

pub(crate) struct TerminalGridCache {
    row_inputs: Vec<PreparedRowInputCacheEntry>,
    prepared_rows: Arc<[Arc<RowPaintInput>]>,
    terminal_fonts: Option<TerminalFonts>,
    colors: Option<TerminalColorsSnapshot>,
    find_spans: Arc<[FindHighlightSpan]>,
    cell_width: Option<Pixels>,
    line_height: Option<Pixels>,
    scale_factor_bits: Option<u32>,
    symbol_plans: SymbolPlanCache,
    prepared_text: Vec<PreparedRowTextCacheEntry>,
    prepared_geometry: Vec<Option<PreparedRowCacheEntry<PreparedRow>>>,
    prepared_visible_geometry: Option<PreparedVisibleGeometry>,
    preedit: Option<PreparedPreedit>,
}

impl TerminalGridCache {
    pub(crate) fn new() -> Self {
        Self {
            row_inputs: Vec::new(),
            prepared_rows: Arc::from([]),
            terminal_fonts: None,
            colors: None,
            find_spans: Arc::from([]),
            cell_width: None,
            line_height: None,
            scale_factor_bits: None,
            symbol_plans: SymbolPlanCache::default(),
            prepared_text: Vec::new(),
            prepared_geometry: Vec::new(),
            prepared_visible_geometry: None,
            preedit: None,
        }
    }

    pub(crate) fn invalidate_scale_dependent(&mut self) {
        self.row_inputs.clear();
        self.prepared_rows = Arc::from([]);
        self.terminal_fonts = None;
        self.colors = None;
        self.find_spans = Arc::from([]);
        self.cell_width = None;
        self.line_height = None;
        self.scale_factor_bits = None;
        self.symbol_plans.invalidate_scale_dependent();
        self.prepared_text.clear();
        self.prepared_geometry.clear();
        self.prepared_visible_geometry = None;
        self.preedit = None;
    }

    pub(crate) fn evict(&mut self) {
        *self = Self::new();
    }

    fn prepare(
        &mut self,
        rows: &Arc<[RowSnapshot]>,
        colors: &TerminalColorsSnapshot,
        terminal_fonts: &TerminalFonts,
        find_spans: &Arc<[FindHighlightSpan]>,
        metrics: TerminalGridMetrics,
    ) -> Arc<[Arc<RowPaintInput>]> {
        let style_changed = self.terminal_fonts.as_ref() != Some(terminal_fonts)
            || self.colors.as_ref() != Some(colors)
            || self.find_spans.as_ref() != find_spans.as_ref()
            || self.cell_width != Some(metrics.cell_width)
            || self.line_height != Some(metrics.line_height)
            || self.scale_factor_bits != Some(metrics.scale_factor.to_bits());
        let rows_unchanged = !style_changed
            && rows.len() == self.row_inputs.len()
            && rows
                .iter()
                .zip(&self.row_inputs)
                .all(|(current, cached)| Arc::ptr_eq(current, &cached.source));
        if rows_unchanged {
            return Arc::clone(&self.prepared_rows);
        }

        let previous = if style_changed {
            Vec::new()
        } else {
            std::mem::take(&mut self.row_inputs)
        };
        let alignment = find_row_alignment(rows, &previous, |_, row, cached| {
            Arc::ptr_eq(row, &cached.source) || row.as_ref() == cached.source.as_ref()
        });
        let mut previous = previous.into_iter().map(Some).collect::<Vec<_>>();
        let mut row_inputs = Vec::with_capacity(rows.len());
        let mut prepared_rows = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let prepared = if let Some(cached) =
                take_aligned_row(&mut previous, index, alignment, |cached| {
                    Arc::ptr_eq(row, &cached.source) || row.as_ref() == cached.source.as_ref()
                }) {
                cached.prepared
            } else {
                Arc::new(prepare_row_cached(
                    row,
                    colors,
                    terminal_fonts,
                    index,
                    find_spans,
                ))
            };
            prepared_rows.push(Arc::clone(&prepared));
            row_inputs.push(PreparedRowInputCacheEntry {
                source: Arc::clone(row),
                prepared,
            });
        }

        self.row_inputs = row_inputs;
        self.prepared_rows = Arc::from(prepared_rows);
        self.terminal_fonts = Some(terminal_fonts.clone());
        self.colors = Some(colors.clone());
        self.find_spans = Arc::clone(find_spans);
        self.cell_width = Some(metrics.cell_width);
        self.line_height = Some(metrics.line_height);
        self.scale_factor_bits = Some(metrics.scale_factor.to_bits());
        Arc::clone(&self.prepared_rows)
    }
}

struct PreparedRowInputCacheEntry {
    source: RowSnapshot,
    prepared: Arc<RowPaintInput>,
}

fn find_row_alignment<C, P>(
    current: &[C],
    previous: &[P],
    matches: impl Fn(usize, &C, &P) -> bool,
) -> Option<(usize, usize)> {
    current
        .first()
        .and_then(|row| {
            previous
                .iter()
                .position(|cached| matches(0, row, cached))
                .map(|index| (0, index))
        })
        .or_else(|| {
            previous.first().and_then(|cached| {
                current
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find(|(index, row)| matches(*index, row, cached))
                    .map(|(index, _)| (index, 0))
            })
        })
}

fn take_aligned_row<T>(
    previous: &mut [Option<T>],
    current_index: usize,
    alignment: Option<(usize, usize)>,
    matches: impl Fn(&T) -> bool,
) -> Option<T> {
    let aligned_index = alignment.and_then(|(anchor_current, anchor_previous)| {
        if current_index >= anchor_current {
            anchor_previous.checked_add(current_index - anchor_current)
        } else {
            anchor_previous.checked_sub(anchor_current - current_index)
        }
    });
    if let Some(index) = aligned_index
        && previous
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(&matches)
    {
        return previous[index].take();
    }
    if aligned_index != Some(current_index)
        && previous
            .get(current_index)
            .and_then(Option::as_ref)
            .is_some_and(matches)
    {
        return previous[current_index].take();
    }
    None
}

#[derive(Clone)]
pub(crate) struct TerminalGridElement {
    background: Color,
    foreground: Color,
    rows: Arc<[Arc<RowPaintInput>]>,
    cache: Entity<TerminalGridCache>,
    grid_size: CellGridSize,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    nominal_line_height: Pixels,
    nominal_cell_width: Pixels,
    cursor: Option<(CursorPositionSnapshot, CellSnapshot)>,
    cursor_style: CursorSnapshot,
    cursor_preparation_style: CursorSnapshot,
    terminal_fonts: TerminalFonts,
    preedit: Option<PreeditLayout>,
    focus_handle: FocusHandle,
    input: gpui::WeakEntity<TerminalPane>,
    blink_phase_visible: bool,
    find_spans: Arc<[FindHighlightSpan]>,
    graphics: PreparedGraphics,
    scale_factor: f32,
    source_presentation: Arc<ScreenSnapshot>,
    presentation: Arc<ScreenSnapshot>,
    presentation_operation: Option<OperationToken>,
    graphics_attempt: Option<GraphicsAttemptToken>,
    graphics_cache: Entity<TerminalGraphicsCache>,
    active_hyperlink: Option<(u64, CellGridPosition)>,
    fallback: Option<Box<TerminalGridElement>>,
    fallback_generation: Option<crate::terminal::PresentationGeneration>,
    paint_fault: Option<PaintPreflightFault>,
    cursor_layer: Option<presentation::CursorLayer>,
}

pub(crate) struct TerminalGridConfiguration {
    pub(crate) terminal_input_focused: bool,
    pub(crate) font_family: SharedString,
    pub(crate) terminal_fonts: TerminalFonts,
    pub(crate) terminal_appearance: Arc<ResolvedTerminalAppearance>,
    pub(crate) font_size: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) cell_width: Pixels,
    pub(crate) grid_size: CellGridSize,
    pub(crate) preedit: Option<PreeditLayout>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) input: Entity<TerminalPane>,
    pub(crate) blink_phase_visible: bool,
    pub(crate) scale_factor: f32,
    pub(crate) find_spans: Arc<[FindHighlightSpan]>,
    pub(crate) graphics: PreparedGraphics,
    pub(crate) presentation_operation: Option<OperationToken>,
    pub(crate) graphics_attempt: Option<GraphicsAttemptToken>,
    pub(crate) graphics_cache: Entity<TerminalGraphicsCache>,
    pub(crate) active_hyperlink: Option<(u64, CellGridPosition)>,
    pub(crate) fallback: Option<(
        Arc<ScreenSnapshot>,
        Entity<TerminalGridCache>,
        PreparedGraphics,
    )>,
    pub(crate) paint_fault: Option<PaintPreflightFault>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaintPreflightFault {
    #[cfg(test)]
    Row(usize),
    #[cfg(test)]
    Glyph(usize),
    #[cfg(test)]
    Image(usize),
}

impl TerminalGridElement {
    pub(crate) fn new(
        screen: &Arc<ScreenSnapshot>,
        cache: Entity<TerminalGridCache>,
        mut configuration: TerminalGridConfiguration,
        cx: &mut App,
    ) -> Self {
        let source_presentation = Arc::clone(screen);
        let screen =
            ScreenSnapshot::projected_for_renderer(screen, &configuration.terminal_appearance);
        let fallback = configuration
            .fallback
            .take()
            .map(|(screen, fallback_cache, graphics)| {
                Box::new(Self::new(
                    &screen,
                    fallback_cache,
                    TerminalGridConfiguration {
                        terminal_input_focused: configuration.terminal_input_focused,
                        font_family: configuration.font_family.clone(),
                        terminal_fonts: configuration.terminal_fonts.clone(),
                        terminal_appearance: Arc::clone(&configuration.terminal_appearance),
                        font_size: configuration.font_size,
                        line_height: configuration.line_height,
                        cell_width: configuration.cell_width,
                        grid_size: configuration.grid_size,
                        preedit: None,
                        focus_handle: configuration.focus_handle.clone(),
                        input: configuration.input.clone(),
                        blink_phase_visible: configuration.blink_phase_visible,
                        scale_factor: configuration.scale_factor,
                        find_spans: Arc::from([]),
                        graphics,
                        presentation_operation: None,
                        graphics_attempt: None,
                        graphics_cache: configuration.graphics_cache.clone(),
                        active_hyperlink: None,
                        fallback: None,
                        paint_fault: None,
                    },
                    cx,
                ))
            });
        let fallback_generation = fallback
            .as_ref()
            .map(|fallback| fallback.presentation.generation);
        let cursor = screen.cursor.position.and_then(|position| {
            screen
                .rows
                .get(usize::from(position.row))
                .and_then(|row| row.get(usize::from(position.column)))
                .cloned()
                .map(|cell| (position, cell))
        });
        let cursor_style = presented_cursor_style(
            screen.cursor,
            configuration.terminal_input_focused,
            configuration.blink_phase_visible,
        );
        let cursor_preparation_style =
            presented_cursor_style(screen.cursor, configuration.terminal_input_focused, true);
        let rows = cache.update(cx, |cache, _| {
            cache.prepare(
                &screen.rows,
                &screen.colors,
                &configuration.terminal_fonts,
                &configuration.find_spans,
                TerminalGridMetrics {
                    cell_width: configuration.cell_width,
                    line_height: configuration.line_height,
                    scale_factor: configuration.scale_factor,
                },
            )
        });
        Self {
            background: screen.background,
            foreground: if screen.colors.reversed {
                screen.colors.background
            } else {
                screen.colors.foreground
            },
            rows,
            cache,
            grid_size: configuration.grid_size,
            font_size: configuration.font_size,
            line_height: configuration.line_height,
            cell_width: configuration.cell_width,
            nominal_line_height: configuration.line_height,
            nominal_cell_width: configuration.cell_width,
            cursor,
            cursor_style,
            cursor_preparation_style,
            terminal_fonts: configuration.terminal_fonts,
            preedit: configuration.preedit,
            focus_handle: configuration.focus_handle,
            input: configuration.input.downgrade(),
            blink_phase_visible: configuration.blink_phase_visible,
            find_spans: configuration.find_spans,
            graphics: configuration.graphics,
            scale_factor: configuration.scale_factor,
            source_presentation,
            presentation: Arc::clone(&screen),
            presentation_operation: configuration.presentation_operation,
            graphics_attempt: configuration.graphics_attempt,
            graphics_cache: configuration.graphics_cache,
            active_hyperlink: configuration.active_hyperlink,
            fallback,
            fallback_generation,
            paint_fault: configuration.paint_fault,
            cursor_layer: None,
        }
    }
}

fn presented_cursor_style(
    mut negotiated: CursorSnapshot,
    terminal_input_focused: bool,
    blink_phase_visible: bool,
) -> CursorSnapshot {
    if negotiated.visible && !terminal_input_focused {
        negotiated.shape = CursorShapeSnapshot::BlockHollow;
        negotiated.blinking = false;
    } else if negotiated.visible && negotiated.blinking && !blink_phase_visible {
        negotiated.visible = false;
    }
    negotiated
}

#[derive(Clone)]
struct PreparedText {
    line: Arc<ShapedLine>,
    origin: gpui::Point<Pixels>,
    // Cached absolute origins in shaped-glyph order, shared by preflight and paint.
    glyph_origins: Arc<[gpui::Point<Pixels>]>,
    blinking: bool,
    paint_runs: Arc<[TextPaintRun]>,
}

impl PreparedText {
    #[inline]
    fn color_at(&self, glyph_index: usize) -> Hsla {
        if self.paint_runs.len() <= 8 {
            return self
                .paint_runs
                .iter()
                .find(|paint| glyph_index < paint.end)
                .map_or_else(Hsla::transparent_black, |paint| paint.color);
        }
        // Run ends follow UTF-8 byte order, independently of shaped glyph order.
        let index = self
            .paint_runs
            .partition_point(|paint| paint.end <= glyph_index);
        self.paint_runs
            .get(index)
            .map_or_else(Hsla::transparent_black, |paint| paint.color)
    }

    fn positioned_glyphs(
        &self,
    ) -> impl Iterator<Item = (gpui::FontId, &gpui::ShapedGlyph, gpui::Point<Pixels>)> {
        self.line
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)))
            .zip(self.glyph_origins.iter().copied())
            .map(|((font_id, glyph), origin)| (font_id, glyph, origin))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TextPaintRun {
    end: usize,
    color: Hsla,
}

struct PreparedRowText {
    text: Vec<PreparedShapedText>,
}

struct PreparedShapedText {
    line: Arc<ShapedLine>,
    start: usize,
    blinking: bool,
}

struct PreparedRow {
    text: Vec<PreparedText>,
    symbols: PreparedDecorations,
    backgrounds: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    under_text_decorations: PreparedDecorations,
    over_text_decorations: PreparedDecorations,
}

#[derive(Clone, Copy)]
struct CursorTextOverlay {
    row_index: usize,
    bounds: Bounds<Pixels>,
    color: Hsla,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CursorTextPaint {
    Unchanged,
    Exclude(Bounds<Pixels>),
    Recolor { bounds: Bounds<Pixels>, color: Hsla },
}

fn cursor_text_paint(overlay: Option<CursorTextOverlay>, row_index: usize) -> CursorTextPaint {
    match overlay {
        None => CursorTextPaint::Unchanged,
        Some(overlay) if overlay.row_index == row_index => CursorTextPaint::Recolor {
            bounds: overlay.bounds,
            color: overlay.color,
        },
        Some(overlay) => CursorTextPaint::Exclude(overlay.bounds),
    }
}

#[derive(Clone)]
struct PreparedFrameRow {
    stable: Arc<PreparedRow>,
    find_backgrounds: Vec<PaintQuad>,
    hyperlink_hover_decorations: PreparedDecorations,
    cursor_background: Option<PaintQuad>,
    cursor_symbols: PreparedDecorations,
    preedit: Option<PreparedPreeditRow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundPaintLayer {
    Terminal,
    Find,
    Selection,
    Cursor,
}

#[derive(Clone, Debug)]
struct PreparedPreeditKey {
    clusters: Arc<[super::terminal_ime::PreeditCluster]>,
    caret: super::terminal_ime::PreeditPosition,
    visible_rows: usize,
    grid_bounds: Bounds<Pixels>,
    font: Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    foreground: Color,
    background: Color,
    caret_color: Color,
    scale_factor_bits: u32,
}

impl PartialEq for PreparedPreeditKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.clusters, &other.clusters)
            && self.caret == other.caret
            && self.visible_rows == other.visible_rows
            && self.grid_bounds == other.grid_bounds
            && self.font == other.font
            && self.font_size == other.font_size
            && self.cell_width == other.cell_width
            && self.line_height == other.line_height
            && self.foreground == other.foreground
            && self.background == other.background
            && self.caret_color == other.caret_color
            && self.scale_factor_bits == other.scale_factor_bits
    }
}

struct PreparedPreedit {
    key: PreparedPreeditKey,
    rows: Arc<[PreparedPreeditRow]>,
}

#[derive(Clone, Default)]
struct PreparedPreeditRow {
    text: Arc<[PreparedText]>,
    backgrounds: Arc<[PaintQuad]>,
    caret: Option<PaintQuad>,
}

impl PreparedFrameRow {
    fn new(stable: Arc<PreparedRow>) -> Self {
        Self {
            stable,
            find_backgrounds: Vec::new(),
            hyperlink_hover_decorations: PreparedDecorations::default(),
            cursor_background: None,
            cursor_symbols: PreparedDecorations::default(),
            preedit: None,
        }
    }

    fn backgrounds_in_paint_order(
        &self,
    ) -> impl Iterator<Item = (BackgroundPaintLayer, &PaintQuad)> {
        self.stable
            .backgrounds
            .iter()
            .map(|quad| (BackgroundPaintLayer::Terminal, quad))
            .chain(
                self.find_backgrounds
                    .iter()
                    .map(|quad| (BackgroundPaintLayer::Find, quad)),
            )
            .chain(
                self.stable
                    .selections
                    .iter()
                    .map(|quad| (BackgroundPaintLayer::Selection, quad)),
            )
            .chain(
                self.cursor_background
                    .iter()
                    .map(|quad| (BackgroundPaintLayer::Cursor, quad)),
            )
    }
}

pub(crate) struct PrepaintState {
    candidate: TerminalPaintBatch,
    fallback: Option<TerminalPaintBatch>,
    cursor: Option<std::rc::Rc<TerminalPaintBatch>>,
}

struct TerminalPaintBatch {
    surface: Option<PaintQuad>,
    grid_bounds: Bounds<Pixels>,
    line_height: Pixels,
    rows: Vec<PreparedFrameRow>,
    cursor_text_overlay: Option<CursorTextOverlay>,
    graphics: GraphicsPaintPlan,
    blink_phase_visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaintBatchFailure {
    Presentation,
    RendererResources,
}

impl TerminalPaintBatch {
    fn preflight(
        &self,
        fault: Option<PaintPreflightFault>,
        window: &mut Window,
        _cx: &mut App,
    ) -> Result<(), PaintBatchFailure> {
        if let Some(failure) = self.injected_failure(fault) {
            return Err(failure);
        }
        // GPUI has no public paint transaction. Warming the exact glyph and image
        // resources through offscreen preparation geometry exercises every
        // fallible paint seam without submitting commands to the visible grid.
        let offscreen = px(-1_000_000.0);
        let preflight_mask = ContentMask {
            bounds: Bounds::new(point(offscreen, offscreen), size(px(2.0), px(2.0))),
        };
        window.with_content_mask(Some(preflight_mask), |window| {
            self.graphics
                .preflight_layer(GraphicsLayer::BelowBackground, window)
                .map_err(|_| PaintBatchFailure::RendererResources)?;
            self.graphics
                .preflight_layer(GraphicsLayer::BelowText, window)
                .map_err(|_| PaintBatchFailure::RendererResources)?;
            for row in &self.rows {
                for text in
                    row.stable.text.iter().filter(|text| {
                        text_fragment_visible(text.blinking, self.blink_phase_visible)
                    })
                {
                    preflight_text(text, self.line_height, window)
                        .map_err(|_| PaintBatchFailure::Presentation)?;
                }
            }
            self.graphics
                .preflight_layer(GraphicsLayer::AboveText, window)
                .map_err(|_| PaintBatchFailure::RendererResources)?;
            for row in &self.rows {
                if let Some(preedit) = &row.preedit {
                    for text in preedit.text.iter() {
                        preflight_text(text, self.line_height, window)
                            .map_err(|_| PaintBatchFailure::Presentation)?;
                    }
                }
            }
            Ok(())
        })
    }

    fn injected_failure(&self, fault: Option<PaintPreflightFault>) -> Option<PaintBatchFailure> {
        match fault? {
            #[cfg(test)]
            PaintPreflightFault::Row(index) => self
                .rows
                .get(index)
                .map(|_| PaintBatchFailure::Presentation),
            #[cfg(test)]
            PaintPreflightFault::Glyph(index) => self
                .rows
                .iter()
                .flat_map(|row| {
                    row.stable
                        .text
                        .iter()
                        .filter(|text| {
                            text_fragment_visible(text.blinking, self.blink_phase_visible)
                        })
                        .chain(
                            row.preedit
                                .as_ref()
                                .into_iter()
                                .flat_map(|preedit| preedit.text.iter()),
                        )
                })
                .flat_map(|text| text.line.text.chars())
                .nth(index)
                .map(|_| PaintBatchFailure::Presentation),
            #[cfg(test)]
            PaintPreflightFault::Image(index) => (index < self.graphics.image_count())
                .then_some(PaintBatchFailure::RendererResources),
        }
    }

    fn submit(
        &self,
        grid_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), PaintBatchFailure> {
        if let Some(surface) = &self.surface {
            window.paint_quad(surface.clone());
        }
        window.with_content_mask(
            Some(ContentMask {
                bounds: grid_bounds,
            }),
            |window| {
                self.graphics
                    .paint_layer(GraphicsLayer::BelowBackground, window)
                    .map_err(|_| PaintBatchFailure::RendererResources)?;
                for row in &self.rows {
                    for (_, background) in row.backgrounds_in_paint_order() {
                        window.paint_quad(background.clone());
                    }
                }
                self.graphics
                    .paint_layer(GraphicsLayer::BelowText, window)
                    .map_err(|_| PaintBatchFailure::RendererResources)?;
                for (row_index, row) in self.rows.iter().enumerate() {
                    paint_prepared_decorations(
                        &row.stable.under_text_decorations,
                        self.blink_phase_visible,
                        window,
                    );
                    paint_prepared_decorations(
                        &row.hyperlink_hover_decorations,
                        self.blink_phase_visible,
                        window,
                    );
                    let cursor_paint = cursor_text_paint(self.cursor_text_overlay, row_index);
                    match cursor_paint {
                        CursorTextPaint::Unchanged => paint_prepared_decorations(
                            &row.stable.symbols,
                            self.blink_phase_visible,
                            window,
                        ),
                        CursorTextPaint::Exclude(bounds)
                        | CursorTextPaint::Recolor { bounds, .. } => {
                            paint_prepared_symbols_excluding_region(
                                &row.stable.symbols,
                                self.blink_phase_visible,
                                bounds,
                                window,
                            );
                            if let CursorTextPaint::Recolor { bounds, .. } = cursor_paint {
                                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                    paint_prepared_decorations(
                                        &row.cursor_symbols,
                                        self.blink_phase_visible,
                                        window,
                                    );
                                });
                            }
                        }
                    }
                    paint_prepared_row_text(
                        row,
                        self.line_height,
                        self.blink_phase_visible,
                        cursor_paint,
                        window,
                    )?;
                    paint_prepared_decorations(
                        &row.stable.over_text_decorations,
                        self.blink_phase_visible,
                        window,
                    );
                }
                self.graphics
                    .paint_layer(GraphicsLayer::AboveText, window)
                    .map_err(|_| PaintBatchFailure::RendererResources)?;
                // Marked Text remains above every image layer.
                for row in &self.rows {
                    if let Some(preedit) = &row.preedit {
                        for background in preedit.backgrounds.iter() {
                            window.paint_quad(background.clone());
                        }
                        for text in preedit.text.iter() {
                            text.line
                                .paint(text.origin, self.line_height, window, cx)
                                .map_err(|_| PaintBatchFailure::Presentation)?;
                        }
                        if let Some(caret) = &preedit.caret {
                            window.paint_quad(caret.clone());
                        }
                    }
                }
                Ok(())
            },
        )
    }
}

fn paint_prepared_row_text(
    row: &PreparedFrameRow,
    line_height: Pixels,
    blink_phase_visible: bool,
    cursor_paint: CursorTextPaint,
    window: &mut Window,
) -> Result<(), PaintBatchFailure> {
    for text in row
        .stable
        .text
        .iter()
        .filter(|text| text_fragment_visible(text.blinking, blink_phase_visible))
    {
        paint_terminal_text(text, line_height, cursor_paint, window)
            .map_err(|_| PaintBatchFailure::Presentation)?;
    }
    Ok(())
}

fn preflight_text(
    text: &PreparedText,
    line_height: Pixels,
    window: &mut Window,
) -> gpui::Result<()> {
    let layout = &*text.line;
    let baseline = (line_height - layout.ascent - layout.descent) / 2.0 + layout.ascent;
    for (font_id, glyph, glyph_origin) in text.positioned_glyphs() {
        let origin = glyph_origin + point(px(0.0), baseline);
        if glyph.is_emoji {
            window.paint_emoji(origin, font_id, glyph.id, layout.font_size)?;
        } else {
            window.paint_glyph(origin, font_id, glyph.id, layout.font_size, rgba(0).into())?;
        }
    }
    Ok(())
}

fn paint_terminal_text(
    text: &PreparedText,
    line_height: Pixels,
    cursor_paint: CursorTextPaint,
    window: &mut Window,
) -> gpui::Result<()> {
    let layout = &*text.line;
    let baseline = (line_height - layout.ascent - layout.descent) / 2.0 + layout.ascent;
    for (font_id, glyph, glyph_origin) in text.positioned_glyphs() {
        let origin = glyph_origin + point(px(0.0), baseline);
        if glyph.is_emoji {
            match cursor_paint {
                CursorTextPaint::Unchanged => {
                    window.paint_emoji(origin, font_id, glyph.id, layout.font_size)?;
                }
                CursorTextPaint::Exclude(bounds) => window.paint_emoji_with_region(
                    origin,
                    font_id,
                    glyph.id,
                    layout.font_size,
                    GlyphPaintRegion::Exclude(bounds),
                )?,
                CursorTextPaint::Recolor { bounds, color } => window.paint_emoji_with_region(
                    origin,
                    font_id,
                    glyph.id,
                    layout.font_size,
                    GlyphPaintRegion::Recolor { bounds, color },
                )?,
            }
        } else {
            let color = text.color_at(glyph.index);
            match cursor_paint {
                CursorTextPaint::Unchanged => {
                    window.paint_glyph(origin, font_id, glyph.id, layout.font_size, color)?
                }
                CursorTextPaint::Exclude(bounds) => window.paint_glyph_with_region(
                    origin,
                    font_id,
                    glyph.id,
                    layout.font_size,
                    color,
                    GlyphPaintRegion::Exclude(bounds),
                )?,
                CursorTextPaint::Recolor {
                    bounds,
                    color: cursor_color,
                } => window.paint_glyph_with_region(
                    origin,
                    font_id,
                    glyph.id,
                    layout.font_size,
                    color,
                    GlyphPaintRegion::Recolor {
                        bounds,
                        color: cursor_color,
                    },
                )?,
            }
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct PreparedDecorations {
    // GPUI consumes Path and rebuilds its scaled vertex Vec during paint. Flat scene primitives
    // keep stable decorations reusable through the row Arc without cloning heap geometry.
    quads: Vec<PreparedQuad>,
    underlines: Vec<PreparedUnderline>,
}

#[derive(Clone)]
struct PreparedQuad {
    quad: PaintQuad,
    blinking: bool,
}

#[derive(Clone)]
struct PreparedUnderline {
    origin: gpui::Point<Pixels>,
    width: Pixels,
    style: UnderlineStyle,
    blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreparedRowKey {
    grid_left: Pixels,
    grid_right: Pixels,
    row_top: Pixels,
    row_bottom: Pixels,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    scale_factor_bits: u32,
    decoration_metrics: DecorationMetrics,
}

#[derive(Clone, Copy, PartialEq)]
struct PreparedGridLayout {
    grid_bounds: Bounds<Pixels>,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    scale_factor: f32,
    decoration_metrics: DecorationMetrics,
}

type PreparedRows = Arc<[Arc<PreparedRow>]>;

struct PreparedVisibleGeometry {
    source: Arc<[Arc<RowPaintInput>]>,
    layout: PreparedGridLayout,
    rows: PreparedRows,
}

struct PreparedRowCacheEntry<T> {
    source: Arc<RowPaintInput>,
    key: PreparedRowKey,
    prepared: Arc<T>,
}

struct PreparedRowTextCacheEntry {
    source: Arc<RowPaintInput>,
    key: PreparedRowTextKey,
    prepared: Arc<PreparedRowText>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreparedRowTextKey {
    font_size: Pixels,
    cell_width: Pixels,
}

fn row_text_shape_eq(first: &RowPaintInput, second: &RowPaintInput) -> bool {
    first.font_resolution_identity == second.font_resolution_identity
        && first.fragments.len() == second.fragments.len()
        && first
            .fragments
            .iter()
            .zip(&second.fragments)
            .all(|(first, second)| {
                first.start == second.start
                    && first.text == second.text
                    && first.simple_cells == second.simple_cells
                    && first.blinking == second.blinking
                    && first.runs.len() == second.runs.len()
                    && first
                        .runs
                        .iter()
                        .zip(&second.runs)
                        .all(|(first, second)| first.len == second.len && first.font == second.font)
            })
}

fn reuse_or_prepare_row<T>(
    cached: &mut Option<PreparedRowCacheEntry<T>>,
    source: &Arc<RowPaintInput>,
    key: PreparedRowKey,
    prepare: impl FnOnce() -> T,
) -> Arc<T> {
    if let Some(cached) = cached
        && Arc::ptr_eq(&cached.source, source)
        && cached.key == key
    {
        return Arc::clone(&cached.prepared);
    }

    let prepared = Arc::new(prepare());
    *cached = Some(PreparedRowCacheEntry {
        source: Arc::clone(source),
        key,
        prepared: Arc::clone(&prepared),
    });
    prepared
}

impl TerminalGridCache {
    fn prepare_frame_geometry(
        &mut self,
        rows: &Arc<[Arc<RowPaintInput>]>,
        visible_rows: usize,
        layout: PreparedGridLayout,
        cursor: Option<&(CursorPositionSnapshot, CellSnapshot)>,
        cursor_style: CursorSnapshot,
        window: &mut Window,
    ) -> (PreparedRows, Option<(usize, PreparedDecorations)>) {
        let stable_rows = self.prepare_visible_geometry(rows, visible_rows, layout, window);
        let cursor_symbols = cursor
            .filter(|_| {
                cursor_style.visible && matches!(cursor_style.shape, CursorShapeSnapshot::Block)
            })
            .and_then(|cursor| {
                let row_index = usize::from(cursor.0.row);
                if row_index >= visible_rows {
                    return None;
                }
                let row = rows.get(row_index)?;
                let row_top = layout.grid_bounds.top() + layout.line_height * row_index as f32;
                let row_bottom = (row_top + layout.line_height).min(layout.grid_bounds.bottom());
                Some((
                    row_index,
                    prepare_cursor_symbols(
                        row,
                        cursor,
                        cursor_style.text_color,
                        row_top,
                        row_bottom,
                        layout.grid_bounds.left(),
                        layout.cell_width,
                        layout.scale_factor,
                        &mut self.symbol_plans,
                    ),
                ))
            });
        (stable_rows, cursor_symbols)
    }

    fn prepare_visible_geometry(
        &mut self,
        rows: &Arc<[Arc<RowPaintInput>]>,
        visible_rows: usize,
        layout: PreparedGridLayout,
        window: &mut Window,
    ) -> PreparedRows {
        let visible_rows = visible_rows.min(rows.len());
        if let Some(cached) = &self.prepared_visible_geometry
            && Arc::ptr_eq(&cached.source, rows)
            && cached.layout == layout
            && cached.rows.len() == visible_rows
        {
            return Arc::clone(&cached.rows);
        }
        self.prepared_geometry.resize_with(visible_rows, || None);
        self.prepared_geometry.truncate(visible_rows);
        let previous_text = std::mem::take(&mut self.prepared_text);
        let mut prepared_text = Vec::with_capacity(visible_rows);
        let mut prepared_rows = Vec::with_capacity(visible_rows);
        let text_key = PreparedRowTextKey {
            font_size: layout.font_size,
            cell_width: layout.cell_width,
        };
        let alignment = find_row_alignment(
            &rows[..visible_rows],
            &previous_text,
            |_, source, cached| row_text_shape_eq(source, &cached.source) && cached.key == text_key,
        );
        let mut previous_text = previous_text.into_iter().map(Some).collect::<Vec<_>>();

        for (row_index, source) in rows.iter().take(visible_rows).enumerate() {
            let row_top = layout.grid_bounds.top() + layout.line_height * row_index as f32;
            let row_bottom = (layout.grid_bounds.top()
                + layout.line_height * row_index.saturating_add(1) as f32)
                .min(layout.grid_bounds.bottom());
            let text = if let Some(cached) =
                take_aligned_row(&mut previous_text, row_index, alignment, |cached| {
                    row_text_shape_eq(source, &cached.source) && cached.key == text_key
                }) {
                cached.prepared
            } else {
                Arc::new(prepare_row_text(
                    source,
                    layout.font_size,
                    window.text_system(),
                ))
            };
            prepared_text.push(PreparedRowTextCacheEntry {
                source: Arc::clone(source),
                key: text_key,
                prepared: Arc::clone(&text),
            });

            let key = PreparedRowKey {
                grid_left: layout.grid_bounds.left(),
                grid_right: layout.grid_bounds.right(),
                row_top,
                row_bottom,
                font_size: layout.font_size,
                cell_width: layout.cell_width,
                line_height: layout.line_height,
                scale_factor_bits: layout.scale_factor.to_bits(),
                decoration_metrics: layout.decoration_metrics,
            };
            let prepared =
                reuse_or_prepare_row(&mut self.prepared_geometry[row_index], source, key, || {
                    prepare_stable_row(source, &text, key, &mut self.symbol_plans)
                });
            prepared_rows.push(prepared);
        }

        self.prepared_text = prepared_text;
        let prepared_rows = Arc::from(prepared_rows);
        self.prepared_visible_geometry = Some(PreparedVisibleGeometry {
            source: Arc::clone(rows),
            layout,
            rows: Arc::clone(&prepared_rows),
        });
        prepared_rows
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_preedit(
        &mut self,
        layout: Option<&PreeditLayout>,
        visible_rows: usize,
        grid_bounds: Bounds<Pixels>,
        font: &Font,
        font_size: Pixels,
        cell_width: Pixels,
        line_height: Pixels,
        foreground: Color,
        background: Color,
        caret_color: Color,
        scale_factor: f32,
        window: &mut Window,
    ) -> Option<Arc<[PreparedPreeditRow]>> {
        let Some(layout) = layout else {
            self.preedit = None;
            return None;
        };
        let key = PreparedPreeditKey {
            clusters: Arc::clone(&layout.clusters),
            caret: layout.caret,
            visible_rows,
            grid_bounds,
            font: font.clone(),
            font_size,
            cell_width,
            line_height,
            foreground,
            background,
            caret_color,
            scale_factor_bits: scale_factor.to_bits(),
        };
        if let Some(cached) = &self.preedit
            && cached.key == key
        {
            return Some(Arc::clone(&cached.rows));
        }

        let mut rows = (0..visible_rows)
            .map(|_| PreparedPreeditRow::default())
            .collect::<Vec<_>>();
        for (row_index, row) in rows.iter_mut().enumerate() {
            let row_top = grid_bounds.top() + line_height * row_index as f32;
            let row_bottom = (row_top + line_height).min(grid_bounds.bottom());
            let clusters = layout
                .clusters
                .iter()
                .filter(|cluster| cluster.row == row_index);
            let mut text = Vec::new();
            let mut backgrounds = Vec::new();
            for cluster in clusters {
                let cluster_left = grid_bounds.left() + cell_width * cluster.column as f32;
                let width_cells = usize::from(cluster.width).max(1);
                let cluster_right =
                    (cluster_left + cell_width * width_cells as f32).min(grid_bounds.right());
                backgrounds.push(fill(
                    Bounds::new(
                        point(cluster_left, row_top),
                        size(cluster_right - cluster_left, row_bottom - row_top),
                    ),
                    gpui_color(background),
                ));
                let color = gpui_color(foreground).into();
                let line = Arc::new(window.text_system().shape_line(
                    cluster.text.clone().into(),
                    font_size,
                    &[TextRun {
                        len: cluster.text.len(),
                        font: font.clone(),
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
                ));
                let origin = point(cluster_left, row_top);
                let mut previous = gpui::Point::default();
                let mut position = origin;
                let glyph_origins = line
                    .runs
                    .iter()
                    .flat_map(|run| &run.glyphs)
                    .map(|glyph| {
                        position += glyph.position - previous;
                        previous = glyph.position;
                        position
                    })
                    .collect();
                text.push(PreparedText {
                    line,
                    origin,
                    glyph_origins,
                    blinking: false,
                    paint_runs: Arc::from([]),
                });
            }
            row.text = Arc::from(text);
            row.backgrounds = Arc::from(backgrounds);
            if layout.caret.row == row_index {
                let caret_left = grid_bounds.left() + cell_width * layout.caret.column as f32;
                row.caret = Some(fill(
                    Bounds::new(point(caret_left, row_top), size(px(1.0), line_height)),
                    gpui_color(caret_color),
                ));
            }
        }
        let rows = Arc::from(rows);
        self.preedit = Some(PreparedPreedit {
            key,
            rows: Arc::clone(&rows),
        });
        Some(rows)
    }
}

fn prepare_row_text(
    row: &RowPaintInput,
    font_size: Pixels,
    text_system: &gpui::WindowTextSystem,
) -> PreparedRowText {
    let text = row
        .fragments
        .iter()
        .map(|fragment| PreparedShapedText {
            line: Arc::new(text_system.shape_line(
                fragment.text.clone(),
                font_size,
                &fragment.runs,
                None,
            )),
            start: fragment.start,
            blinking: fragment.blinking,
        })
        .collect();
    PreparedRowText { text }
}

/// Resolve each glyph from its absolute terminal column, never from a preceding
/// fragment or glyph. Fractional fitted cell widths must use identical arithmetic
/// even when an application redraw inserts a symbol and splits a shaping run.
fn terminal_glyph_origins(
    fragment: &TextFragment,
    line: &ShapedLine,
    grid_left: Pixels,
    row_top: Pixels,
    cell_width: Pixels,
) -> Arc<[gpui::Point<Pixels>]> {
    // Complex graphemes and wide cells already occupy their own fragment. Keep
    // their native offsets intact, including marks before or above the base glyph.
    if !fragment.simple_cells {
        let origin = point(grid_left + cell_width * fragment.start as f32, row_top);
        return line
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .map(|glyph| origin + glyph.position)
            .collect();
    }

    // These fragments contain exactly one scalar per terminal cell. Native
    // shaping may still produce multiple glyphs for one scalar, so map by byte
    // index and retain offsets within each cluster rather than counting glyphs.
    let mut cells = fragment
        .text
        .char_indices()
        .map(|(index, _)| (index, None::<Pixels>))
        .collect::<Vec<_>>();
    line.runs
        .iter()
        .flat_map(|run| &run.glyphs)
        .map(|glyph| {
            let column = cells
                .partition_point(|(index, _)| *index <= glyph.index)
                .saturating_sub(1);
            let cluster_origin = *cells[column].1.get_or_insert(glyph.position.x);
            point(
                grid_left
                    + cell_width * (fragment.start + column) as f32
                    + (glyph.position.x - cluster_origin),
                row_top + glyph.position.y,
            )
        })
        .collect()
}

fn prepare_stable_row(
    row: &RowPaintInput,
    shaped: &PreparedRowText,
    key: PreparedRowKey,
    symbol_plans: &mut SymbolPlanCache,
) -> PreparedRow {
    let text = shaped
        .text
        .iter()
        .zip(&row.fragments)
        .map(|(text, fragment)| PreparedText {
            line: Arc::clone(&text.line),
            glyph_origins: terminal_glyph_origins(
                fragment,
                &text.line,
                key.grid_left,
                key.row_top,
                key.cell_width,
            ),
            origin: point(
                key.grid_left + key.cell_width * text.start as f32,
                key.row_top,
            ),
            blinking: text.blinking,
            paint_runs: Arc::clone(&fragment.paint_runs),
        })
        .collect();
    let backgrounds = prepare_background_geometry(
        &row.backgrounds,
        Bounds::new(
            point(key.grid_left, key.row_top),
            size(key.grid_right - key.grid_left, key.row_bottom - key.row_top),
        ),
        key.cell_width,
    );
    let selections = prepare_background_geometry(
        &row.selections,
        Bounds::new(
            point(key.grid_left, key.row_top),
            size(key.grid_right - key.grid_left, key.row_bottom - key.row_top),
        ),
        key.cell_width,
    );
    let under_text_decorations = prepare_decoration_geometry(
        &row.under_text_decorations,
        key.row_top,
        key.grid_left,
        key.cell_width,
        key.decoration_metrics,
    );
    let over_text_decorations = prepare_decoration_geometry(
        &row.over_text_decorations,
        key.row_top,
        key.grid_left,
        key.cell_width,
        key.decoration_metrics,
    );
    let symbols = prepare_symbol_geometry(
        &row.symbols,
        key.row_top,
        key.row_bottom,
        key.grid_left,
        key.cell_width,
        f32::from_bits(key.scale_factor_bits),
        symbol_plans,
    );

    PreparedRow {
        text,
        symbols,
        backgrounds,
        selections,
        under_text_decorations,
        over_text_decorations,
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_cursor_symbols(
    row: &RowPaintInput,
    cursor: &(CursorPositionSnapshot, CellSnapshot),
    text_color: Color,
    row_top: Pixels,
    row_bottom: Pixels,
    grid_left: Pixels,
    cell_width: Pixels,
    scale_factor: f32,
    symbol_plans: &mut SymbolPlanCache,
) -> PreparedDecorations {
    let (position, cell) = cursor;
    if cell.spacer_tail || cell.invisible {
        return PreparedDecorations::default();
    }
    let Some(symbol) = row
        .symbols
        .iter()
        .find(|symbol| symbol.start == usize::from(position.column))
    else {
        return PreparedDecorations::default();
    };
    let mut symbol = symbol.clone();
    symbol.color = text_color;
    prepare_symbol_geometry(
        &[symbol],
        row_top,
        row_bottom,
        grid_left,
        cell_width,
        scale_factor,
        symbol_plans,
    )
}

fn paint_prepared_decorations(
    prepared: &PreparedDecorations,
    blink_phase_visible: bool,
    window: &mut Window,
) {
    for prepared in prepared
        .quads
        .iter()
        .filter(|prepared| text_fragment_visible(prepared.blinking, blink_phase_visible))
    {
        window.paint_quad(prepared.quad.clone());
    }
    for prepared in prepared
        .underlines
        .iter()
        .filter(|prepared| text_fragment_visible(prepared.blinking, blink_phase_visible))
    {
        window.paint_underline(prepared.origin, prepared.width, &prepared.style);
    }
}

fn paint_prepared_symbols_excluding_region(
    prepared: &PreparedDecorations,
    blink_phase_visible: bool,
    excluded_bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    debug_assert!(prepared.underlines.is_empty());
    for prepared in prepared
        .quads
        .iter()
        .filter(|prepared| text_fragment_visible(prepared.blinking, blink_phase_visible))
    {
        window.paint_quad_excluding_region(prepared.quad.clone(), excluded_bounds);
    }
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
        let fitted_cell = TerminalGeometry::fitted_cell_size(
            LogicalSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height)),
            LogicalCellSize::new(
                f32::from(self.nominal_cell_width),
                f32::from(self.nominal_line_height),
            ),
            self.grid_size,
        );
        self.cell_width = px(fitted_cell.width);
        self.line_height = px(fitted_cell.height);
        let viewport_rows = usize::from(self.grid_size.rows);
        let visible_rows = viewport_rows.min(self.rows.len());
        let mut prepared_rows = Vec::with_capacity(visible_rows);
        let grid_bounds = bounds;
        let grid_left = grid_bounds.left();
        let base_font = self.terminal_fonts.regular.clone();
        let font_id = window.text_system().resolve_font(&base_font);
        let baseline =
            window
                .text_system()
                .baseline_offset(font_id, self.font_size, self.line_height);
        let ascent = window.text_system().ascent(font_id, self.font_size);
        let descent = window.text_system().descent(font_id, self.font_size);
        let x_height = window.text_system().x_height(font_id, self.font_size);
        let decoration_metrics = decoration_metrics(
            baseline,
            ascent,
            descent,
            x_height,
            self.line_height,
            window.scale_factor(),
        );
        let rows = Arc::clone(&self.rows);
        let cursor = self.cursor.as_ref();
        let cursor_preparation_style = self.cursor_preparation_style;
        let terminal_fonts = self.terminal_fonts.clone();
        let (stable_rows, preedit_rows, cursor_symbols) = self.cache.update(_cx, |cache, _| {
            let (stable_rows, cursor_symbols) = cache.prepare_frame_geometry(
                &rows,
                visible_rows,
                PreparedGridLayout {
                    grid_bounds,
                    font_size: self.font_size,
                    cell_width: self.cell_width,
                    line_height: self.line_height,
                    scale_factor: self.scale_factor,
                    decoration_metrics,
                },
                cursor,
                cursor_preparation_style,
                window,
            );
            let preedit_rows = cache.prepare_preedit(
                self.preedit.as_ref(),
                visible_rows,
                grid_bounds,
                &terminal_fonts.regular,
                self.font_size,
                self.cell_width,
                self.line_height,
                self.foreground,
                self.background,
                self.cursor_style.color,
                self.scale_factor,
                window,
            );
            (stable_rows, preedit_rows, cursor_symbols)
        });
        let active_hyperlink_occurrence =
            hyperlink_occurrence(&self.presentation, self.active_hyperlink);
        let mut batch_cursor_text_overlay = None;

        for (row_index, stable) in stable_rows.iter().cloned().enumerate() {
            let row_top = bounds.top() + self.line_height * row_index as f32;
            let find_backgrounds = prepare_background_geometry(
                &find_background_spans(
                    row_index,
                    &self.find_spans,
                    &self.presentation.colors.configured,
                ),
                Bounds::new(
                    point(grid_bounds.left(), row_top),
                    size(
                        grid_bounds.size.width,
                        (row_top + self.line_height).min(grid_bounds.bottom()) - row_top,
                    ),
                ),
                self.cell_width,
            );
            let mut cursor_background = None;
            let mut cursor_text_overlay = None;
            if preedit_rows
                .as_ref()
                .and_then(|rows| rows.get(row_index))
                .is_none()
                && self.cursor_style.visible
                && let Some((position, _)) = &self.cursor
                && usize::from(position.row) == row_index
            {
                let plan = frame_cursor_paint_plan(
                    grid_left,
                    row_top,
                    self.cell_width,
                    self.line_height,
                    *position,
                    self.cursor_style,
                )
                .expect("a visible cursor always produces a paint plan");
                cursor_background = Some(match plan.paint {
                    CursorPaint::Fill => fill(plan.bounds, gpui_color(self.cursor_style.color)),
                    CursorPaint::Outline => outline(
                        plan.bounds,
                        gpui_color(self.cursor_style.color),
                        BorderStyle::default(),
                    ),
                });
                cursor_text_overlay = plan.recolor_text.then_some(CursorTextOverlay {
                    row_index,
                    bounds: plan.bounds.intersect(&grid_bounds),
                    color: gpui_color(self.cursor_style.text_color).into(),
                });
            }

            let mut frame = PreparedFrameRow::new(stable);
            frame.find_backgrounds = find_backgrounds;
            frame.hyperlink_hover_decorations = prepare_decoration_geometry(
                &hyperlink_hover_underline_spans(
                    &self.presentation.rows[row_index],
                    row_index,
                    active_hyperlink_occurrence,
                    self.presentation.colors.configured.hyperlink,
                ),
                row_top,
                grid_left,
                self.cell_width,
                decoration_metrics,
            );
            frame.cursor_background = cursor_background;
            batch_cursor_text_overlay = batch_cursor_text_overlay.or(cursor_text_overlay);
            if cursor_text_overlay.is_some()
                && let Some((cursor_row, symbols)) = &cursor_symbols
                && *cursor_row == row_index
            {
                frame.cursor_symbols = symbols.clone();
            }
            frame.preedit = preedit_rows
                .as_ref()
                .and_then(|rows| rows.get(row_index))
                .cloned();
            prepared_rows.push(frame);
        }

        let mut candidate = TerminalPaintBatch {
            surface: None,
            grid_bounds,
            line_height: self.line_height,
            rows: prepared_rows,
            cursor_text_overlay: batch_cursor_text_overlay,
            graphics: self.graphics.paint_plan(
                grid_bounds,
                self.cell_width,
                self.line_height,
                self.scale_factor,
            ),
            blink_phase_visible: self.blink_phase_visible,
        };
        let cursor = self.cursor_layer.as_ref().and_then(|_| {
            let position = self.cursor.as_ref()?.0;
            let row = candidate.rows.get_mut(usize::from(position.row))?;
            let cursor_row = row.clone();
            let cursor_bounds = cursor_row
                .cursor_background
                .as_ref()?
                .bounds
                .intersect(&grid_bounds);
            row.cursor_background = None;
            row.cursor_symbols = PreparedDecorations::default();
            let cursor_text_overlay = candidate.cursor_text_overlay.map(|mut overlay| {
                overlay.row_index = 0;
                overlay
            });
            candidate.cursor_text_overlay = None;
            Some(std::rc::Rc::new(TerminalPaintBatch {
                surface: Some(fill(cursor_bounds, gpui_color(self.background))),
                grid_bounds: cursor_bounds,
                line_height: self.line_height,
                rows: vec![cursor_row],
                cursor_text_overlay,
                graphics: GraphicsPaintPlan::default(),
                blink_phase_visible: true,
            }))
        });
        let fallback = self.fallback.as_mut().map(|fallback| {
            let mut request_layout = ();
            fallback
                .prepaint(None, None, bounds, &mut request_layout, window, _cx)
                .candidate
        });
        PrepaintState {
            candidate,
            fallback,
            cursor,
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
        if let Some(layer) = &self.cursor_layer {
            layer.clear();
            #[cfg(test)]
            layer.record_grid_paint();
        }
        let mut failure = prepaint
            .candidate
            .preflight(self.paint_fault.take(), window, cx)
            .err();
        if failure.is_none()
            && let Some(cursor) = &prepaint.cursor
        {
            failure = cursor.preflight(None, window, cx).err();
        }
        let mut submitted_generation = None;
        if failure.is_none() {
            match prepaint
                .candidate
                .submit(prepaint.candidate.grid_bounds, window, cx)
            {
                Ok(()) => {
                    submitted_generation = Some(self.presentation.generation);
                    if let Some(layer) = &self.cursor_layer {
                        layer.set(prepaint.cursor.clone());
                    }
                }
                Err(submission_failure) => failure = Some(submission_failure),
            }
        }
        if failure.is_some()
            && let Some(graphics_attempt) = self.graphics_attempt
        {
            self.graphics_cache.update(cx, |cache, cx| {
                cache.rollback(graphics_attempt, Some(window), cx);
            });
        }
        if failure.is_some()
            && let Some(fallback) = &prepaint.fallback
            && fallback.preflight(None, window, cx).is_ok()
            && fallback.submit(fallback.grid_bounds, window, cx).is_ok()
        {
            submitted_generation = self.fallback_generation;
        }
        let Some(pane) = self.input.upgrade() else {
            return;
        };
        TerminalPane::capture_pointer_drag(&pane, window);
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, pane.clone()),
            cx,
        );
        let presentation = Arc::clone(&self.presentation);
        if let (Some(operation), Some(graphics_attempt)) =
            (self.presentation_operation, self.graphics_attempt)
        {
            window.defer(cx, move |window, cx| {
                pane.update(cx, |pane, cx| {
                    if let Some(generation) = submitted_generation {
                        pane.record_scene_submission_attempt(generation);
                    }
                    match failure {
                        Some(PaintBatchFailure::RendererResources) => {
                            pane.renderer_resource_failed(operation, graphics_attempt, cx);
                        }
                        Some(PaintBatchFailure::Presentation) => {
                            pane.presentation_failed(operation, graphics_attempt, cx);
                        }
                        None => {
                            pane.presentation_succeeded(
                                operation,
                                graphics_attempt,
                                presentation,
                                window,
                                cx,
                            );
                        }
                    }
                });
            });
        }
    }
}

struct RowPaintInput {
    font_resolution_identity: String,
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
    symbol: TerminalSymbol,
}

struct TextFragment {
    start: usize,
    text: SharedString,
    runs: Vec<TextRun>,
    paint_runs: Arc<[TextPaintRun]>,
    // Otherwise the fragment is one complete grapheme anchored at its head cell.
    simple_cells: bool,
    blinking: bool,
}

struct FragmentBuilder {
    start: usize,
    blinking: bool,
    text: String,
    runs: Vec<TextRun>,
    paint_runs: Vec<TextPaintRun>,
}

impl FragmentBuilder {
    fn new(start: usize, blinking: bool) -> Self {
        Self {
            start,
            blinking,
            text: String::new(),
            runs: Vec::new(),
            paint_runs: Vec::new(),
        }
    }

    fn push(&mut self, cell: &CellSnapshot, foreground: Color, terminal_fonts: &TerminalFonts) {
        let start = self.text.len();
        self.text.push_str(&cell.text);
        let len = self.text.len() - start;
        if len == 0 {
            return;
        }

        let color = gpui_color(foreground).into();
        let font = terminal_fonts.cell(cell.bold, cell.italic);

        if let Some(previous) = self.paint_runs.last_mut()
            && previous.color == color
        {
            previous.end = self.text.len();
        } else {
            self.paint_runs.push(TextPaintRun {
                end: self.text.len(),
                color,
            });
        }

        if let Some(previous) = self.runs.last_mut()
            && previous.font == *font
        {
            previous.len += len;
        } else {
            self.runs.push(TextRun {
                len,
                font: font.clone(),
                // GPUI also splits font runs on color changes. Keep selection and Find
                // colors in paint_runs so they cannot change glyph positioning.
                color: rgba(0).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }

    fn finish(self, simple_cells: bool) -> TextFragment {
        TextFragment {
            start: self.start,
            text: self.text.into(),
            runs: self.runs,
            paint_runs: Arc::from(self.paint_runs),
            simple_cells,
            blinking: self.blinking,
        }
    }
}

#[cfg(test)]
thread_local! {
    static TERMINAL_FONT_PREPARATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn terminal_cell_font(family: &SharedString, bold: bool, italic: bool) -> Font {
    #[cfg(test)]
    TERMINAL_FONT_PREPARATIONS.with(|count| count.set(count.get() + 1));
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

#[cfg(test)]
fn test_terminal_fonts(family: &SharedString) -> TerminalFonts {
    TerminalFonts {
        resolution_identity: family.to_string(),
        regular: terminal_cell_font(family, false, false),
        bold: terminal_cell_font(family, true, false),
        italic: terminal_cell_font(family, false, true),
        bold_italic: terminal_cell_font(family, true, true),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackgroundSpan {
    start: usize,
    len: usize,
    color: Color,
}

fn find_background_spans(
    row_index: usize,
    find_spans: &[FindHighlightSpan],
    configured: &TerminalColors,
) -> Vec<BackgroundSpan> {
    [false, true]
        .into_iter()
        .flat_map(|current| {
            find_spans
                .iter()
                .filter(move |span| usize::from(span.row) == row_index && span.current == current)
                .map(move |span| BackgroundSpan {
                    start: usize::from(span.start_column),
                    len: usize::from(
                        span.end_column
                            .saturating_sub(span.start_column)
                            .saturating_add(1),
                    ),
                    color: if current {
                        configured.find_active_match_background
                    } else {
                        configured.find_match_background
                    },
                })
        })
        .collect()
}

fn prepare_background_geometry(
    spans: &[BackgroundSpan],
    row_bounds: Bounds<Pixels>,
    cell_width: Pixels,
) -> Vec<PaintQuad> {
    spans
        .iter()
        .map(|span| {
            let span_end = span.start.saturating_add(span.len);
            let left = row_bounds.left() + cell_width * span.start as f32;
            let right = (row_bounds.left() + cell_width * span_end as f32).min(row_bounds.right());
            fill(
                Bounds::new(
                    point(left, row_bounds.top()),
                    size((right - left).max(px(0.0)), row_bounds.size.height),
                ),
                gpui_color(span.color),
            )
        })
        .collect()
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
    descent: Pixels,
    x_height: Pixels,
    line_height: Pixels,
    scale_factor: f32,
) -> DecorationMetrics {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let device_pixel = px(1.0 / scale_factor);
    let snap = |value: Pixels| px((f32::from(value) * scale_factor).round() / scale_factor);
    let row_bottom = px((f32::from(line_height) * scale_factor).floor() / scale_factor);
    let lowest_safe_underline_y = (row_bottom - device_pixel * 3.0).max(px(0.0));
    let underline_y = snap(baseline + descent * 0.618).min(lowest_safe_underline_y);
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
) -> PreparedDecorations {
    let mut prepared = PreparedDecorations::default();
    for span in spans {
        let left = grid_left + cell_width * span.start as f32;
        let right = left + cell_width * span.len as f32;
        let width = right - left;
        let mut push_line = |y: Pixels| {
            prepared.quads.push(PreparedQuad {
                quad: fill(
                    Bounds::new(point(left, row_top + y), size(width, metrics.thickness)),
                    gpui_color(span.color),
                ),
                blinking: span.blinking,
            });
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
                    prepared.quads.push(PreparedQuad {
                        quad: fill(
                            Bounds::new(
                                point(x, row_top + metrics.underline_y),
                                size(dot_width, metrics.thickness),
                            ),
                            gpui_color(span.color),
                        ),
                        blinking: span.blinking,
                    });
                    x += metrics.device_pixel * 2.0;
                }
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Dashed) => {
                let dash_width = metrics.device_pixel * 3.0;
                let mut x = left;
                while x < right {
                    let width = dash_width.min(right - x);
                    prepared.quads.push(PreparedQuad {
                        quad: fill(
                            Bounds::new(
                                point(x, row_top + metrics.underline_y),
                                size(width, metrics.thickness),
                            ),
                            gpui_color(span.color),
                        ),
                        blinking: span.blinking,
                    });
                    x += dash_width + metrics.device_pixel * 2.0;
                }
            }
            DecorationKind::Underline(TerminalUnderlineSnapshot::Curly) => {
                prepared.underlines.push(PreparedUnderline {
                    origin: point(left, row_top + metrics.underline_y),
                    width,
                    style: UnderlineStyle {
                        thickness: metrics.thickness,
                        color: Some(gpui_color(span.color).into()),
                        wavy: true,
                    },
                    blinking: span.blinking,
                });
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
    row_bottom: Pixels,
    grid_left: Pixels,
    cell_width: Pixels,
    scale_factor: f32,
    symbol_plans: &mut SymbolPlanCache,
) -> PreparedDecorations {
    let mut prepared = PreparedDecorations::default();
    for symbol in symbols {
        debug_assert!(matches!(symbol.width_cells, 1 | 2));
        let bounds = snapped_symbol_bounds(
            grid_left,
            row_top,
            row_bottom,
            cell_width,
            symbol.start,
            symbol.width_cells,
            scale_factor,
        );
        let plan = symbol_plans.get(
            symbol.symbol,
            bounds.width_device,
            bounds.height_device,
            bounds.scale_factor,
        );
        let origin = bounds.origin();
        for primitive in &plan.primitives {
            match primitive {
                SymbolPrimitive::Rect {
                    x,
                    y,
                    width,
                    height,
                    alpha,
                } => prepared.quads.push(PreparedQuad {
                    quad: fill(
                        Bounds::new(
                            point(
                                origin.x + px(f32::from(*x) / bounds.scale_factor),
                                origin.y + px(f32::from(*y) / bounds.scale_factor),
                            ),
                            size(
                                px(f32::from(*width) / bounds.scale_factor),
                                px(f32::from(*height) / bounds.scale_factor),
                            ),
                        ),
                        gpui_color(symbol_color_with_alpha(symbol.color, *alpha)),
                    ),
                    blinking: symbol.blinking,
                }),
                SymbolPrimitive::Polygon { points, alpha } => {
                    let context = DeviceRasterContext {
                        cell_width: plan.width_device,
                        cell_height: plan.height_device,
                        origin,
                        scale: bounds.scale_factor,
                        color: symbol_color_with_alpha(symbol.color, *alpha),
                        blinking: symbol.blinking,
                    };
                    push_device_polygon_quads(&mut prepared, points, &context);
                }
                SymbolPrimitive::Stroke {
                    points,
                    thickness,
                    alpha,
                } => {
                    let context = DeviceRasterContext {
                        cell_width: plan.width_device,
                        cell_height: plan.height_device,
                        origin,
                        scale: bounds.scale_factor,
                        color: symbol_color_with_alpha(symbol.color, *alpha),
                        blinking: symbol.blinking,
                    };
                    push_device_stroke_quads(&mut prepared, points, *thickness, &context);
                }
            }
        }
    }
    prepared
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SnappedSymbolBounds {
    left_device: i32,
    top_device: i32,
    width_device: u16,
    height_device: u16,
    scale_factor: f32,
}

impl SnappedSymbolBounds {
    fn origin(self) -> gpui::Point<Pixels> {
        point(
            px(self.left_device as f32 / self.scale_factor),
            px(self.top_device as f32 / self.scale_factor),
        )
    }
}

fn snapped_symbol_bounds(
    grid_left: Pixels,
    row_top: Pixels,
    row_bottom: Pixels,
    cell_width: Pixels,
    start: usize,
    width_cells: u8,
    scale_factor: f32,
) -> SnappedSymbolBounds {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let start = start as f32;
    let end = start + f32::from(width_cells.max(1));
    let left_device =
        ((f32::from(grid_left) + f32::from(cell_width) * start) * scale_factor).round() as i32;
    let right_device =
        ((f32::from(grid_left) + f32::from(cell_width) * end) * scale_factor).round() as i32;
    // Snap cumulative endpoints so adjacent cells reuse one device-pixel boundary instead of
    // accumulating an independently rounded cell width or line height.
    let top_device = (f32::from(row_top) * scale_factor).round() as i32;
    let bottom_device = (f32::from(row_bottom) * scale_factor).round() as i32;
    let width_device = right_device
        .saturating_sub(left_device)
        .clamp(1, i32::from(u16::MAX)) as u16;
    let height_device = bottom_device
        .saturating_sub(top_device)
        .clamp(1, i32::from(u16::MAX)) as u16;
    SnappedSymbolBounds {
        left_device,
        top_device,
        width_device,
        height_device,
        scale_factor,
    }
}

struct DeviceRasterContext {
    cell_width: u16,
    cell_height: u16,
    origin: gpui::Point<Pixels>,
    scale: f32,
    color: Color,
    blinking: bool,
}

fn push_device_polygon_quads(
    prepared: &mut PreparedDecorations,
    points: &[DevicePoint],
    context: &DeviceRasterContext,
) {
    if points.len() < 3 {
        return;
    }

    let top = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as u16;
    let bottom = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(f32::from(context.cell_height)) as u16;
    let mut intersections = Vec::with_capacity(points.len());
    for y in top..bottom {
        intersections.clear();
        let sample_y = f32::from(y) + 0.5;
        for index in 0..points.len() {
            let start = points[index];
            let end = points[(index + 1) % points.len()];
            if (start.y <= sample_y && end.y > sample_y)
                || (end.y <= sample_y && start.y > sample_y)
            {
                intersections
                    .push(start.x + (sample_y - start.y) * (end.x - start.x) / (end.y - start.y));
            }
        }
        intersections.sort_by(f32::total_cmp);
        for pair in intersections.chunks_exact(2) {
            push_device_quad(
                prepared,
                pair[0],
                f32::from(y),
                pair[1] - pair[0],
                1.0,
                context,
            );
        }
    }
}

fn push_device_stroke_quads(
    prepared: &mut PreparedDecorations,
    points: &[DevicePoint],
    thickness: u16,
    context: &DeviceRasterContext,
) {
    let thickness = f32::from(thickness.max(1));
    let offset = (thickness / 2.0).floor();
    let mut covered =
        vec![false; usize::from(context.cell_width) * usize::from(context.cell_height)];
    for segment in points.windows(2) {
        let start = segment[0];
        let end = segment[1];
        let delta_x = end.x - start.x;
        let delta_y = end.y - start.y;
        let samples = (delta_x.abs().max(delta_y.abs()).ceil() as usize).max(1);
        for sample in 0..=samples {
            let progress = sample as f32 / samples as f32;
            let left = ((start.x + delta_x * progress).round() - offset)
                .clamp(0.0, (f32::from(context.cell_width) - thickness).max(0.0));
            let top = ((start.y + delta_y * progress).round() - offset)
                .clamp(0.0, (f32::from(context.cell_height) - thickness).max(0.0));
            let right = (left + thickness).ceil().min(f32::from(context.cell_width)) as usize;
            let bottom = (top + thickness).ceil().min(f32::from(context.cell_height)) as usize;
            for y in top.floor() as usize..bottom {
                for x in left.floor() as usize..right {
                    covered[y * usize::from(context.cell_width) + x] = true;
                }
            }
        }
    }
    push_covered_device_runs(prepared, &covered, context);
}

fn push_covered_device_runs(
    prepared: &mut PreparedDecorations,
    covered: &[bool],
    context: &DeviceRasterContext,
) {
    let width = usize::from(context.cell_width);
    for y in 0..usize::from(context.cell_height) {
        let mut x = 0;
        while x < width {
            if !covered[y * width + x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && covered[y * width + x] {
                x += 1;
            }
            push_device_quad(
                prepared,
                start as f32,
                y as f32,
                (x - start) as f32,
                1.0,
                context,
            );
        }
    }
}

fn push_device_quad(
    prepared: &mut PreparedDecorations,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    context: &DeviceRasterContext,
) {
    let left = x.clamp(0.0, f32::from(context.cell_width));
    let top = y.clamp(0.0, f32::from(context.cell_height));
    let right = (x + width).clamp(0.0, f32::from(context.cell_width));
    let bottom = (y + height).clamp(0.0, f32::from(context.cell_height));
    if right <= left || bottom <= top {
        return;
    }
    prepared.quads.push(PreparedQuad {
        quad: fill(
            Bounds::new(
                point(
                    context.origin.x + px(left / context.scale),
                    context.origin.y + px(top / context.scale),
                ),
                size(
                    px((right - left) / context.scale),
                    px((bottom - top) / context.scale),
                ),
            ),
            gpui_color(context.color),
        ),
        blinking: context.blinking,
    });
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HyperlinkOccurrence {
    identity: u64,
    start_row: usize,
    start_column: usize,
    end_row: usize,
    end_column: usize,
}

fn cell_has_hyperlink(screen: &ScreenSnapshot, row: usize, column: usize, identity: u64) -> bool {
    screen
        .rows
        .get(row)
        .and_then(|row| row.get(column))
        .and_then(|cell| cell.hyperlink.as_ref())
        .is_some_and(|link| link.identity == identity)
}

fn hyperlink_occurrence(
    screen: &ScreenSnapshot,
    active_hyperlink: Option<(u64, CellGridPosition)>,
) -> Option<HyperlinkOccurrence> {
    let (identity, hovered_cell) = active_hyperlink?;
    let hovered_row = usize::from(hovered_cell.row);
    let hovered_column = usize::from(hovered_cell.col);
    if !cell_has_hyperlink(screen, hovered_row, hovered_column, identity) {
        return None;
    }

    let mut start_row = hovered_row;
    let mut start_column = hovered_column;
    loop {
        if start_column > 0 && cell_has_hyperlink(screen, start_row, start_column - 1, identity) {
            start_column -= 1;
            continue;
        }
        if start_column == 0 && start_row > 0 {
            let previous_row = start_row - 1;
            if let Some(previous_column) = screen
                .rows
                .get(previous_row)
                .and_then(|row| row.len().checked_sub(1))
                && screen
                    .row_soft_wrapped
                    .get(previous_row)
                    .copied()
                    .unwrap_or(false)
                && cell_has_hyperlink(screen, previous_row, previous_column, identity)
            {
                start_row = previous_row;
                start_column = previous_column;
                continue;
            }
        }
        break;
    }

    let mut end_row = hovered_row;
    let mut end_column = hovered_column;
    loop {
        let row_len = screen.rows.get(end_row)?.len();
        if end_column + 1 < row_len && cell_has_hyperlink(screen, end_row, end_column + 1, identity)
        {
            end_column += 1;
            continue;
        }
        if end_column + 1 == row_len
            && screen
                .row_soft_wrapped
                .get(end_row)
                .copied()
                .unwrap_or(false)
            && cell_has_hyperlink(screen, end_row + 1, 0, identity)
        {
            end_row += 1;
            end_column = 0;
            continue;
        }
        break;
    }

    Some(HyperlinkOccurrence {
        identity,
        start_row,
        start_column,
        end_row,
        end_column,
    })
}

fn hyperlink_hover_underline_spans(
    row: &RowSnapshot,
    row_index: usize,
    occurrence: Option<HyperlinkOccurrence>,
    color: Color,
) -> Vec<DecorationSpan> {
    let Some(occurrence) = occurrence
        .filter(|occurrence| (occurrence.start_row..=occurrence.end_row).contains(&row_index))
    else {
        return Vec::new();
    };
    let start_column = if row_index == occurrence.start_row {
        occurrence.start_column
    } else {
        0
    };
    let end_column = if row_index == occurrence.end_row {
        occurrence.end_column
    } else {
        row.len().saturating_sub(1)
    };
    let mut spans = Vec::new();
    for (column, cell) in row
        .iter()
        .enumerate()
        .skip(start_column)
        .take(end_column.saturating_sub(start_column) + 1)
    {
        if cell.invisible
            || is_kitty_placeholder(cell)
            || cell.underline != TerminalUnderlineSnapshot::None
            || cell
                .hyperlink
                .as_ref()
                .is_none_or(|link| link.identity != occurrence.identity)
        {
            continue;
        }
        push_decoration(
            &mut spans,
            DecorationSpan {
                start: column,
                len: 1,
                kind: DecorationKind::Underline(TerminalUnderlineSnapshot::Single),
                color,
                blinking: false,
            },
        );
    }
    spans
}

#[cfg(test)]
fn prepare_row(
    row: &RowSnapshot,
    colors: &TerminalColorsSnapshot,
    font_family: &SharedString,
) -> RowPaintInput {
    let terminal_fonts = test_terminal_fonts(font_family);
    prepare_row_cached(row, colors, &terminal_fonts, 0, &[])
}

fn prepare_row_cached(
    row: &RowSnapshot,
    colors: &TerminalColorsSnapshot,
    terminal_fonts: &TerminalFonts,
    row_index: usize,
    find_spans: &[FindHighlightSpan],
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
        let placeholder = is_kitty_placeholder(cell);
        let (_, background) = effective_colors(cell, colors);
        // Source identity matters even when a program's explicit RGB matches the default tint.
        // Reverse video turns foreground colors into cell backgrounds, which remain opaque.
        if cell.inverse
            || (!colors.reversed && cell.background_source != TerminalColor::Default)
            || (colors.reversed && cell.foreground_source != TerminalColor::Default)
            || background != colors.effective_background()
        {
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
            let selection = colors.configured.selection_background;
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

        let foreground = presented_cell_foreground(
            cell,
            colors,
            find_foreground_for_cell(row_index, column, find_spans, &colors.configured),
        );
        if !cell.invisible && !placeholder {
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

        if cell.spacer_tail || cell.invisible || placeholder {
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
            symbols.push(SymbolPaintInput {
                start: column,
                width_cells,
                color: foreground,
                blinking: cell.blinking,
                symbol,
            });
            continue;
        }

        if regular_fragment
            .as_ref()
            .is_some_and(|fragment| fragment.blinking != cell.blinking)
            && let Some(fragment) = regular_fragment.take()
        {
            fragments.push(fragment.finish(true));
        }

        let requires_whole_cell_shaping = !is_simple_cell(&cell.text, width_cells);
        if requires_whole_cell_shaping {
            if let Some(fragment) = regular_fragment.take() {
                fragments.push(fragment.finish(true));
            }
            let mut fragment = FragmentBuilder::new(column, cell.blinking);
            fragment.push(cell, foreground, terminal_fonts);
            fragments.push(fragment.finish(false));
        } else {
            regular_fragment
                .get_or_insert_with(|| FragmentBuilder::new(column, cell.blinking))
                .push(cell, foreground, terminal_fonts);
        }
    }

    if let Some(fragment) = regular_fragment {
        fragments.push(fragment.finish(true));
    }

    underlines.extend(overlines);
    RowPaintInput {
        font_resolution_identity: terminal_fonts.resolution_identity.clone(),
        fragments,
        symbols,
        backgrounds,
        selections,
        under_text_decorations: underlines,
        over_text_decorations: strikethroughs,
    }
}

fn is_kitty_placeholder(cell: &CellSnapshot) -> bool {
    // The entire grapheme encodes image placement, including its diacritics.
    // It must remain blank even when its image is missing or has been deleted.
    cell.text.starts_with('\u{10eeee}')
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

fn is_simple_cell(text: &str, width_cells: u8) -> bool {
    width_cells == 1 && text.chars().count() == 1 && !text.chars().any(is_bidi_sensitive)
}

fn text_fragment_visible(blinking: bool, blink_phase_visible: bool) -> bool {
    !blinking || blink_phase_visible
}

fn effective_colors(cell: &CellSnapshot, colors: &TerminalColorsSnapshot) -> (Color, Color) {
    let foreground_source = match (cell.foreground_source, cell.bold && colors.bold_as_bright) {
        (TerminalColor::Palette(index @ 0..=7), true) => TerminalColor::Palette(index + 8),
        (source, _) => source,
    };
    let mut foreground = if colors.bold_as_bright
        && cell.bold
        && !cell.faint
        && foreground_source == TerminalColor::Default
        && colors.foreground_source == TerminalDefaultColorSource::HostDefault
    {
        colors.configured.bright_foreground
    } else {
        resolve_color_source(foreground_source, colors, colors.foreground)
    };
    let mut background = resolve_color_source(cell.background_source, colors, colors.background);
    if cell.inverse ^ colors.reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.faint {
        let source = if cell.inverse ^ colors.reversed {
            cell.background_source
        } else {
            foreground_source
        };
        foreground = dim_color(
            foreground,
            source,
            colors,
            !(cell.inverse ^ colors.reversed),
        );
    }
    (foreground, background)
}

fn presented_cell_foreground(
    cell: &CellSnapshot,
    colors: &TerminalColorsSnapshot,
    find_foreground: Option<Color>,
) -> Color {
    let foreground = effective_colors(cell, colors).0;
    if cell.selected {
        colors.configured.selection_foreground.unwrap_or(foreground)
    } else {
        find_foreground.unwrap_or(foreground)
    }
}

fn find_foreground_for_cell(
    row_index: usize,
    column: usize,
    find_spans: &[FindHighlightSpan],
    configured: &TerminalColors,
) -> Option<Color> {
    let matching = find_spans.iter().filter(|span| {
        usize::from(span.row) == row_index
            && (usize::from(span.start_column)..=usize::from(span.end_column)).contains(&column)
    });
    let current = matching.clone().any(|span| span.current);
    if current {
        configured.find_active_match_foreground
    } else if matching.count() > 0 {
        configured.find_match_foreground
    } else {
        None
    }
}

fn dim_color(
    color: Color,
    source: TerminalColor,
    colors: &TerminalColorsSnapshot,
    default_is_foreground: bool,
) -> Color {
    match source {
        TerminalColor::Default
            if default_is_foreground
                && colors.foreground_source == TerminalDefaultColorSource::HostDefault =>
        {
            colors.configured.dim_foreground
        }
        TerminalColor::Palette(index @ 0..=15) if !colors.palette_overrides[usize::from(index)] => {
            colors.configured.dim[usize::from(index % 8)]
        }
        _ => Color {
            a: color.a.div_ceil(2),
            ..color
        },
    }
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

fn frame_cursor_paint_plan(
    grid_left: Pixels,
    row_top: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    position: CursorPositionSnapshot,
    style: CursorSnapshot,
) -> Option<CursorPaintPlan> {
    let cursor_left = grid_left + cell_width * f32::from(position.column);
    cursor_paint_plan(
        style.visible,
        style.shape,
        point(cursor_left, row_top),
        cell_width,
        line_height,
        position.width_cells,
    )
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use super::*;
    use crate::ui::terminal_ime::layout_preedit;

    #[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
    mod macos_adapter_tests {
        include!("../platform/macos_adapter_tests/terminal_glyphs.rs");
    }

    #[derive(Clone, Default)]
    struct PaintCapture {
        glyphs: Rc<RefCell<Vec<gpui::PaintedGlyphForTest>>>,
        quads: Rc<RefCell<Vec<gpui::PaintedQuadForTest>>>,
        quad_paint_calls: Rc<Cell<usize>>,
    }

    struct PaintBatches {
        batches: Vec<TerminalPaintBatch>,
        capture: PaintCapture,
    }

    impl IntoElement for PaintBatches {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for PaintBatches {
        type RequestLayoutState = ();
        type PrepaintState = ();

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
            (window.request_layout(Style::default(), [], cx), ())
        }

        fn prepaint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            _bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            _window: &mut Window,
            _cx: &mut App,
        ) {
        }

        fn paint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            _bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            _prepaint: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            window.reset_paint_call_counts_for_test();
            for batch in &self.batches {
                batch.submit(batch.grid_bounds, window, cx).unwrap();
            }
            *self.capture.glyphs.borrow_mut() = window.painted_glyphs_for_test();
            *self.capture.quads.borrow_mut() = window.painted_quads_for_test();
            self.capture
                .quad_paint_calls
                .set(window.quad_paint_call_count_for_test());
        }
    }

    fn cursor_render_batches(window: &mut Window, retained: bool) -> Vec<TerminalPaintBatch> {
        let cell_width = px(8.375);
        let line_height = px(14.0);
        let grid_bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(80.0), px(28.0)));
        let cursor_bounds = Bounds::new(grid_bounds.origin, size(cell_width * 3.0, line_height));
        let mut cursor_text = cell("B");
        cursor_text.foreground_source = TerminalColor::Rgb(Color::rgb(0x00_aa_00));
        let mut cursor_emoji_tail = cell(" ");
        cursor_emoji_tail.spacer_tail = true;
        let mut overhang_text = cell("A\u{30d}\u{30d}\u{30d}\u{30d}\u{30d}\u{30d}");
        overhang_text.foreground_source = TerminalColor::Rgb(Color::rgb(0xdd_00_00));
        let mut overhang_emoji_tail = cell(" ");
        overhang_emoji_tail.spacer_tail = true;
        let rows = Arc::<[RowSnapshot]>::from([
            Arc::from([cursor_text, cell("😀"), cursor_emoji_tail]),
            Arc::from([overhang_text, cell("😀"), overhang_emoji_tail]),
        ]);
        let terminal_fonts = test_terminal_fonts(&"Menlo".into());
        let mut cache = TerminalGridCache::new();
        let inputs = cache.prepare(
            &rows,
            &colors(),
            &terminal_fonts,
            &Arc::from([]),
            TerminalGridMetrics {
                cell_width,
                line_height,
                scale_factor: window.scale_factor(),
            },
        );
        let stable_rows = cache.prepare_visible_geometry(
            &inputs,
            rows.len(),
            PreparedGridLayout {
                grid_bounds,
                font_size: px(18.0),
                cell_width,
                line_height,
                scale_factor: window.scale_factor(),
                decoration_metrics: decoration_metrics(
                    px(12.0),
                    px(10.0),
                    px(4.0),
                    px(7.0),
                    line_height,
                    window.scale_factor(),
                ),
            },
            window,
        );
        assert!(stable_rows.iter().all(|row| !row.text.is_empty()));
        let mut rows = stable_rows
            .iter()
            .cloned()
            .map(PreparedFrameRow::new)
            .collect::<Vec<_>>();
        rows[0].cursor_background = Some(fill(cursor_bounds, rgba(0x22_44_88_ff)));
        let overlay = CursorTextOverlay {
            row_index: 0,
            bounds: cursor_bounds,
            color: rgba(0xff_ff_ff_ff).into(),
        };

        if !retained {
            return vec![TerminalPaintBatch {
                surface: None,
                grid_bounds,
                line_height,
                rows,
                cursor_text_overlay: Some(overlay),
                graphics: GraphicsPaintPlan::default(),
                blink_phase_visible: true,
            }];
        }

        let mut cursor_row = rows[0].clone();
        rows[0].cursor_background = None;
        cursor_row.cursor_background = Some(fill(cursor_bounds, rgba(0x22_44_88_ff)));
        vec![
            TerminalPaintBatch {
                surface: None,
                grid_bounds,
                line_height,
                rows,
                cursor_text_overlay: None,
                graphics: GraphicsPaintPlan::default(),
                blink_phase_visible: true,
            },
            TerminalPaintBatch {
                surface: Some(fill(cursor_bounds, rgba(0x0b_0b_0b_ff))),
                grid_bounds: cursor_bounds,
                line_height,
                rows: vec![cursor_row],
                cursor_text_overlay: Some(overlay),
                graphics: GraphicsPaintPlan::default(),
                blink_phase_visible: true,
            },
        ]
    }

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
            foreground_source: crate::terminal::TerminalDefaultColorSource::HostDefault,
            background_source: crate::terminal::TerminalDefaultColorSource::HostDefault,
            cursor_source: crate::terminal::TerminalDefaultColorSource::HostDefault,
            palette_overrides: Arc::new([false; 256]),
            configured: Arc::new(crate::appearance::TerminalColors::default()),
            bold_as_bright: true,
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
            semantic_content: crate::terminal::CellSemanticSnapshot::Output,
            hyperlink: None,
        }
    }

    fn grid_metrics() -> TerminalGridMetrics {
        TerminalGridMetrics {
            cell_width: px(8.0),
            line_height: px(20.0),
            scale_factor: 1.0,
        }
    }

    fn prepared_row_key() -> PreparedRowKey {
        PreparedRowKey {
            grid_left: px(0.0),
            grid_right: px(80.0),
            row_top: px(0.0),
            row_bottom: px(20.0),
            font_size: px(14.0),
            cell_width: px(8.0),
            line_height: px(20.0),
            scale_factor_bits: 2.0f32.to_bits(),
            decoration_metrics: decoration_metrics(
                px(15.0),
                px(11.0),
                px(4.0),
                px(8.0),
                px(20.0),
                2.0,
            ),
        }
    }

    fn prepared_grid_layout(
        _terminal_fonts: &TerminalFonts,
        font_size: Pixels,
        cell_width: Pixels,
    ) -> PreparedGridLayout {
        PreparedGridLayout {
            grid_bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(80.0), px(40.0))),
            font_size,
            cell_width,
            line_height: px(20.0),
            scale_factor: 2.0,
            decoration_metrics: decoration_metrics(
                px(15.0),
                px(11.0),
                px(4.0),
                px(8.0),
                px(20.0),
                2.0,
            ),
        }
    }

    #[gpui::test]
    fn glyph_colors_follow_utf8_boundaries_independent_of_glyph_order(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let fonts = test_terminal_fonts(&"Menlo".into());
                let red = Color::rgb(0xdd_00_00);
                let blue = Color::rgb(0x00_00_dd);
                let green = Color::rgb(0x00_dd_00);
                let styled_cells = [
                    ("a", red),
                    ("é", red),
                    ("b", blue),
                    ("c", green),
                    ("d", red),
                    ("é", blue),
                    ("f", green),
                    ("g", red),
                    ("h", blue),
                    ("i", green),
                    ("j", red),
                ];
                for (cell_count, expected_text, expected_colors) in [
                    (4, "aébc", &[red, red, red, blue, green][..]),
                    (
                        11,
                        "aébcdéfghij",
                        &[
                            red, red, red, blue, green, red, blue, blue, green, red, blue, green,
                            red,
                        ][..],
                    ),
                ] {
                    let cells = styled_cells[..cell_count]
                        .iter()
                        .map(|(text, foreground)| {
                            let mut subject = cell(text);
                            subject.foreground_source = TerminalColor::Rgb(*foreground);
                            subject
                        })
                        .collect::<Vec<_>>();
                    let rows = Arc::<[RowSnapshot]>::from([Arc::from(cells)]);
                    let mut cache = TerminalGridCache::new();
                    let inputs =
                        cache.prepare(&rows, &colors(), &fonts, &Arc::from([]), grid_metrics());
                    let prepared = cache.prepare_visible_geometry(
                        &inputs,
                        1,
                        prepared_grid_layout(&fonts, px(14.0), px(8.0)),
                        window,
                    );
                    assert_eq!(prepared[0].text.len(), 1);
                    let text = &prepared[0].text[0];
                    assert_eq!(text.line.text.as_ref(), expected_text);
                    for index in (0..expected_colors.len())
                        .rev()
                        .chain(0..expected_colors.len())
                    {
                        assert_eq!(
                            text.color_at(index),
                            gpui_color(expected_colors[index]).into()
                        );
                    }
                    assert_eq!(text.color_at(expected_colors.len()), rgba(0).into());
                    assert_eq!(text.color_at(usize::MAX), rgba(0).into());
                    let mut empty = text.clone();
                    empty.paint_runs = Arc::from([]);
                    assert_eq!(empty.color_at(0), rgba(0).into());
                }
            })
            .expect("the glyph color test window should remain available");
    }

    #[gpui::test]
    fn visible_geometry_reuses_exact_inputs_and_rebuilds_for_every_layout_dependency(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let fonts = test_terminal_fonts(&"Menlo".into());
                let rows = Arc::<[RowSnapshot]>::from([
                    Arc::<[CellSnapshot]>::from([cell("first")]),
                    Arc::<[CellSnapshot]>::from([cell("second")]),
                ]);
                let mut cache = TerminalGridCache::new();
                let colors = colors();
                let inputs = cache.prepare(&rows, &colors, &fonts, &Arc::from([]), grid_metrics());
                let layout = prepared_grid_layout(&fonts, px(14.0), px(8.0));
                let original = cache.prepare_visible_geometry(&inputs, 2, layout, window);
                let reused = cache.prepare_visible_geometry(&inputs, 2, layout, window);
                assert!(Arc::ptr_eq(&original, &reused));

                let variants = [
                    PreparedGridLayout {
                        grid_bounds: Bounds::new(point(px(3.0), px(5.0)), layout.grid_bounds.size),
                        ..layout
                    },
                    PreparedGridLayout {
                        grid_bounds: Bounds::new(
                            layout.grid_bounds.origin,
                            size(px(75.0), px(31.0)),
                        ),
                        ..layout
                    },
                    PreparedGridLayout {
                        font_size: px(15.0),
                        ..layout
                    },
                    PreparedGridLayout {
                        cell_width: px(8.5),
                        ..layout
                    },
                    PreparedGridLayout {
                        line_height: px(21.0),
                        ..layout
                    },
                    PreparedGridLayout {
                        scale_factor: 1.0,
                        ..layout
                    },
                    PreparedGridLayout {
                        decoration_metrics: DecorationMetrics {
                            device_pixel: px(0.25),
                            ..layout.decoration_metrics
                        },
                        ..layout
                    },
                ];
                for variant in variants {
                    let rebuilt = cache.prepare_visible_geometry(&inputs, 2, variant, window);
                    assert!(!Arc::ptr_eq(&original, &rebuilt));
                }

                let moved = cache.prepare_visible_geometry(
                    &inputs,
                    2,
                    PreparedGridLayout {
                        grid_bounds: Bounds::new(point(px(3.0), px(5.0)), layout.grid_bounds.size),
                        ..layout
                    },
                    window,
                );
                assert_eq!(moved[0].text[0].origin, point(px(3.0), px(5.0)));

                let clipped = cache.prepare_visible_geometry(
                    &inputs,
                    2,
                    PreparedGridLayout {
                        grid_bounds: Bounds::new(
                            layout.grid_bounds.origin,
                            size(px(75.0), px(31.0)),
                        ),
                        ..layout
                    },
                    window,
                );
                assert_eq!(clipped[1].text[0].origin.y, px(20.0));
                assert_eq!(
                    cache
                        .prepare_visible_geometry(&inputs, 1, layout, window)
                        .len(),
                    1
                );

                let mut changed_colors = colors.clone();
                changed_colors.foreground = Color::rgb(0x12_34_56);
                Arc::make_mut(&mut changed_colors.configured).foreground =
                    changed_colors.foreground;
                let changed_inputs = cache.prepare(
                    &rows,
                    &changed_colors,
                    &fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                let recolored = cache.prepare_visible_geometry(&changed_inputs, 2, layout, window);
                assert!(!Arc::ptr_eq(&original, &recolored));
                assert_ne!(
                    original[0].text[0].paint_runs[0].color,
                    recolored[0].text[0].paint_runs[0].color
                );

                let mut selected = cell("first");
                selected.selected = true;
                let selected_rows = Arc::<[RowSnapshot]>::from([
                    Arc::<[CellSnapshot]>::from([selected]),
                    Arc::<[CellSnapshot]>::from([cell("second")]),
                ]);
                let selected_inputs = cache.prepare(
                    &selected_rows,
                    &colors,
                    &fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                let selected_geometry =
                    cache.prepare_visible_geometry(&selected_inputs, 2, layout, window);
                assert!(!Arc::ptr_eq(&original, &selected_geometry));
                assert!(!selected_geometry[0].selections.is_empty());

                let retained = Arc::downgrade(&selected_geometry);
                drop(selected_geometry);
                cache.evict();
                assert!(retained.upgrade().is_none());
            })
            .expect("the geometry cache test window should remain available");
    }

    fn prepared_preedit_key(layout: &PreeditLayout, visible_rows: usize) -> PreparedPreeditKey {
        PreparedPreeditKey {
            clusters: Arc::clone(&layout.clusters),
            caret: layout.caret,
            visible_rows,
            grid_bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(80.0), px(40.0))),
            font: terminal_cell_font(&"Menlo".into(), false, false),
            font_size: px(14.0),
            cell_width: px(8.0),
            line_height: px(20.0),
            foreground: Color::rgb(0xff_ff_ff),
            background: Color::rgb(0),
            caret_color: Color::rgb(0xff_ff_ff),
            scale_factor_bits: 2.0f32.to_bits(),
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
    fn theme_intensity_roles_preserve_application_supplied_colors() {
        let mut colors = colors();
        colors.foreground = TerminalColors::default().foreground;
        let mut palette = *colors.palette;
        palette[..8].copy_from_slice(&TerminalColors::default().normal);
        palette[8..16].copy_from_slice(&TerminalColors::default().bright);
        colors.palette = Arc::new(palette);
        let mut subject = cell("x");
        subject.bold = true;
        assert_eq!(
            effective_colors(&subject, &colors).0,
            TerminalColors::default().bright_foreground
        );
        subject.faint = true;
        assert_eq!(
            effective_colors(&subject, &colors).0,
            TerminalColors::default().dim_foreground
        );
        for index in 0..16 {
            subject.foreground_source = TerminalColor::Palette(index);
            assert_eq!(
                effective_colors(&subject, &colors).0,
                TerminalColors::default().dim[usize::from(index % 8)]
            );
        }
        let custom = Color::rgb(0x123456);
        Arc::make_mut(&mut colors.palette)[9] = custom;
        Arc::make_mut(&mut colors.palette_overrides)[9] = true;
        subject.foreground_source = TerminalColor::Palette(1);
        assert_eq!(
            effective_colors(&subject, &colors).0,
            Color { a: 128, ..custom }
        );
        subject.foreground_source = TerminalColor::Default;
        colors.foreground = custom;
        colors.foreground_source = TerminalDefaultColorSource::ProgramOverride;
        subject.faint = false;
        assert_eq!(effective_colors(&subject, &colors).0, custom);
        subject.foreground_source = TerminalColor::Rgb(custom);
        subject.faint = true;
        assert_eq!(
            effective_colors(&subject, &colors).0,
            Color { a: 128, ..custom }
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
    fn equal_program_overrides_do_not_gain_host_bold_or_dim_roles() {
        let mut colors = colors();
        colors.foreground = colors.configured.foreground;
        colors.foreground_source = TerminalDefaultColorSource::ProgramOverride;
        let mut subject = cell("x");
        subject.bold = true;

        assert_eq!(effective_colors(&subject, &colors).0, colors.foreground);

        subject.faint = true;
        assert_eq!(
            effective_colors(&subject, &colors).0,
            Color {
                a: colors.foreground.a.div_ceil(2),
                ..colors.foreground
            }
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
    fn final_row_cursor_should_keep_normal_cell_geometry() {
        let grid = Bounds::new(point(px(0.0), px(0.0)), size(px(95.0), px(45.0)));
        let position = CursorPositionSnapshot {
            column: 9,
            row: 1,
            width_cells: 1,
        };
        let style = CursorSnapshot {
            visible: true,
            shape: CursorShapeSnapshot::Bar,
            ..CursorSnapshot::default()
        };

        let actual =
            frame_cursor_paint_plan(grid.left(), px(20.0), px(9.0), px(20.0), position, style)
                .unwrap();
        let expected = cursor_paint_plan(
            true,
            CursorShapeSnapshot::Bar,
            point(px(81.0), px(20.0)),
            px(9.0),
            px(20.0),
            1,
        )
        .unwrap();

        assert_eq!(actual, expected);
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
    fn block_cursor_excludes_neighbor_overhang_and_recolors_only_its_row() {
        let bounds = Bounds::new(point(px(20.0), px(20.0)), size(px(10.0), px(20.0)));
        let color = rgba(0xffffff).into();
        let overlay = CursorTextOverlay {
            row_index: 1,
            bounds,
            color,
        };

        assert_eq!(
            cursor_text_paint(Some(overlay), 0),
            CursorTextPaint::Exclude(bounds)
        );
        assert_eq!(
            cursor_text_paint(Some(overlay), 1),
            CursorTextPaint::Recolor { bounds, color }
        );
        assert_eq!(
            cursor_text_paint(Some(overlay), 2),
            CursorTextPaint::Exclude(bounds)
        );
        assert_eq!(cursor_text_paint(None, 1), CursorTextPaint::Unchanged);
    }

    #[gpui::test]
    fn direct_block_cursor_clips_real_neighbor_glyphs_and_recolors_only_its_row(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.set_glyph_raster_bounds(Bounds::new(
            point(gpui::DevicePixels(-2), gpui::DevicePixels(-40)),
            size(gpui::DevicePixels(20), gpui::DevicePixels(48)),
        ));
        let cx = cx.add_empty_window();
        let capture = PaintCapture::default();
        let paint_capture = capture.clone();
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(80.0), px(28.0)),
            move |window, _| PaintBatches {
                batches: cursor_render_batches(window, false),
                capture: paint_capture,
            },
        );
        let scale_factor = cx.update(|window, _| window.scale_factor());
        let glyphs = capture.glyphs.borrow();
        let cursor = Bounds::new(point(px(0.0), px(0.0)), size(px(8.375 * 3.0), px(14.0)))
            .scale(scale_factor);
        let red: Hsla = rgba(0xdd_00_00_ff).into();
        let white: Hsla = rgba(0xff_ff_ff_ff).into();
        let neighbor_text = glyphs
            .iter()
            .filter(|glyph| {
                matches!(
                    glyph.kind,
                    gpui::PaintedGlyphKindForTest::Monochrome { color } if color == red
                )
            })
            .collect::<Vec<_>>();
        assert!(
            neighbor_text
                .iter()
                .any(|glyph| glyph.raster_bounds.intersects(&cursor)),
            "the fixture must contain a real neighboring-row raster overhang: neighbor={neighbor_text:#?}; all={glyphs:#?}; cursor={cursor:?}"
        );
        assert!(
            neighbor_text
                .iter()
                .all(|glyph| !glyph.visible_bounds.intersects(&cursor)),
            "neighboring monochrome pixels must be excluded from the cursor"
        );
        assert!(glyphs.iter().any(|glyph| {
            matches!(
                glyph.kind,
                gpui::PaintedGlyphKindForTest::Monochrome { color } if color == white
            ) && glyph.visible_bounds.intersects(&cursor)
        }));

        let emoji = glyphs
            .iter()
            .filter(|glyph| matches!(glyph.kind, gpui::PaintedGlyphKindForTest::Emoji))
            .collect::<Vec<_>>();
        let cursor_center_y = cursor.origin.y + cursor.size.height / 2.0;
        let cursor_emoji = emoji
            .iter()
            .filter(|glyph| glyph.raster_bounds.origin.y < cursor_center_y)
            .collect::<Vec<_>>();
        let neighbor_emoji = emoji
            .iter()
            .filter(|glyph| glyph.raster_bounds.origin.y >= cursor_center_y)
            .collect::<Vec<_>>();
        assert!(
            cursor_emoji
                .iter()
                .any(|glyph| glyph.visible_bounds.intersects(&cursor)),
            "the cursor-owning row must preserve polychrome glyphs"
        );
        assert!(
            neighbor_emoji
                .iter()
                .any(|glyph| glyph.raster_bounds.intersects(&cursor)),
            "the fixture must contain a neighboring emoji raster overhang"
        );
        assert!(
            neighbor_emoji
                .iter()
                .all(|glyph| !glyph.visible_bounds.intersects(&cursor)),
            "neighboring emoji pixels must be excluded from the cursor"
        );
    }

    #[gpui::test]
    fn retained_block_cursor_covers_neighbor_overhang_before_repainting_its_row(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.set_glyph_raster_bounds(Bounds::new(
            point(gpui::DevicePixels(-2), gpui::DevicePixels(-40)),
            size(gpui::DevicePixels(20), gpui::DevicePixels(48)),
        ));
        let cx = cx.add_empty_window();
        let capture = PaintCapture::default();
        let paint_capture = capture.clone();
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(80.0), px(28.0)),
            move |window, _| PaintBatches {
                batches: cursor_render_batches(window, true),
                capture: paint_capture,
            },
        );
        let scale_factor = cx.update(|window, _| window.scale_factor());
        let glyphs = capture.glyphs.borrow();
        let quads = capture.quads.borrow();
        let cursor = Bounds::new(point(px(0.0), px(0.0)), size(px(8.375 * 3.0), px(14.0)))
            .scale(scale_factor);
        let red: Hsla = rgba(0xdd_00_00_ff).into();
        let white: Hsla = rgba(0xff_ff_ff_ff).into();
        let neighbor_order = glyphs
            .iter()
            .filter(|glyph| {
                matches!(
                    glyph.kind,
                    gpui::PaintedGlyphKindForTest::Monochrome { color } if color == red
                ) && glyph.visible_bounds.intersects(&cursor)
            })
            .map(|glyph| glyph.order)
            .max()
            .expect("the retained base frame keeps the neighboring overhang");
        let cover_order = quads
            .iter()
            .filter(|quad| quad.visible_bounds == cursor && quad.order > neighbor_order)
            .map(|quad| quad.order)
            .max()
            .expect("the retained cursor layer must cover the cursor rectangle");
        assert!(glyphs.iter().any(|glyph| {
            matches!(
                glyph.kind,
                gpui::PaintedGlyphKindForTest::Monochrome { color } if color == white
            ) && glyph.visible_bounds.intersects(&cursor)
                && glyph.order > cover_order
        }));
        assert!(glyphs.iter().any(|glyph| {
            matches!(glyph.kind, gpui::PaintedGlyphKindForTest::Emoji)
                && glyph.visible_bounds.intersects(&cursor)
                && glyph.order > cover_order
        }));
    }

    #[gpui::test]
    fn direct_block_cursor_visits_each_symbol_quad_once(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let capture = PaintCapture::default();
        let paint_capture = capture.clone();
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(80.0), px(60.0)),
            move |_, _| {
                let stable = Arc::new(PreparedRow {
                    text: Vec::new(),
                    symbols: PreparedDecorations {
                        quads: (0..3)
                            .map(|index| PreparedQuad {
                                quad: fill(
                                    Bounds::new(
                                        point(px(30.0 + index as f32 * 10.0), px(0.0)),
                                        size(px(8.0), px(20.0)),
                                    ),
                                    rgba(0xff_ff_ff_ff),
                                ),
                                blinking: false,
                            })
                            .collect(),
                        underlines: Vec::new(),
                    },
                    backgrounds: Vec::new(),
                    selections: Vec::new(),
                    under_text_decorations: PreparedDecorations::default(),
                    over_text_decorations: PreparedDecorations::default(),
                });
                let empty = Arc::new(PreparedRow {
                    text: Vec::new(),
                    symbols: PreparedDecorations::default(),
                    backgrounds: Vec::new(),
                    selections: Vec::new(),
                    under_text_decorations: PreparedDecorations::default(),
                    over_text_decorations: PreparedDecorations::default(),
                });
                PaintBatches {
                    batches: vec![TerminalPaintBatch {
                        surface: None,
                        grid_bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(80.0), px(60.0))),
                        line_height: px(20.0),
                        rows: vec![
                            PreparedFrameRow::new(stable),
                            PreparedFrameRow::new(Arc::clone(&empty)),
                            PreparedFrameRow::new(empty),
                        ],
                        cursor_text_overlay: Some(CursorTextOverlay {
                            row_index: 1,
                            bounds: Bounds::new(point(px(20.0), px(20.0)), size(px(8.0), px(20.0))),
                            color: rgba(0xff_ff_ff_ff).into(),
                        }),
                        graphics: GraphicsPaintPlan::default(),
                        blink_phase_visible: true,
                    }],
                    capture: paint_capture,
                }
            },
        );

        assert_eq!(capture.quad_paint_calls.get(), 3);
    }

    #[test]
    fn terminal_focus_alone_selects_negotiated_or_steady_hollow_cursor() {
        let negotiated = CursorSnapshot {
            visible: true,
            blinking: true,
            shape: crate::terminal::CursorShapeSnapshot::Bar,
            ..CursorSnapshot::default()
        };

        assert_eq!(presented_cursor_style(negotiated, true, true), negotiated);
        assert_eq!(
            presented_cursor_style(negotiated, false, false),
            CursorSnapshot {
                blinking: false,
                shape: crate::terminal::CursorShapeSnapshot::BlockHollow,
                ..negotiated
            }
        );

        assert_eq!(
            presented_cursor_style(negotiated, true, false),
            CursorSnapshot {
                visible: false,
                ..negotiated
            }
        );

        let hidden = CursorSnapshot {
            visible: false,
            ..negotiated
        };
        assert_eq!(presented_cursor_style(hidden, false, false), hidden);
    }

    #[test]
    fn final_edge_selection_should_stop_at_the_real_cell_bounds() {
        let mut selected = cell("x");
        selected.selected = true;
        let input = prepare_row(&Arc::from([selected]), &colors(), &"Menlo".into());
        let shaped = PreparedRowText { text: Vec::new() };
        let mut key = prepared_row_key();
        key.grid_right = px(10.0);

        let prepared = prepare_stable_row(&input, &shaped, key, &mut SymbolPlanCache::default());

        assert_eq!(
            prepared.selections[0].bounds,
            Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(20.0)))
        );
    }

    #[test]
    fn kitty_placeholder_graphemes_leave_gaps_without_shaping_ids_or_diacritics() {
        let rows = Arc::from([
            cell("a"),
            cell("\u{10eeee}\u{305}\u{30d}\u{30e}"),
            cell("\u{10eeee}"),
            cell("e\u{301}"),
            cell("z"),
        ]);

        let prepared = prepare_row(&rows, &colors(), &"Menlo".into());

        assert_eq!(
            prepared
                .fragments
                .iter()
                .map(|fragment| (fragment.start, fragment.text.as_ref()))
                .collect::<Vec<_>>(),
            [(0, "a"), (3, "e\u{301}"), (4, "z")]
        );
    }

    #[test]
    fn kitty_placeholder_retains_background_and_selection_without_text_decorations() {
        let mut placeholder = cell("\u{10eeee}\u{305}\u{30d}");
        placeholder.foreground_source = TerminalColor::Rgb(Color::rgb(0x12_34_56));
        placeholder.background_source = TerminalColor::Rgb(Color::rgb(0x65_43_21));
        placeholder.underline_source = TerminalColor::Palette(200);
        placeholder.underline = TerminalUnderlineSnapshot::Double;
        placeholder.strikethrough = true;
        placeholder.overline = true;
        placeholder.selected = true;

        let prepared = prepare_row(&Arc::from([placeholder]), &colors(), &"Menlo".into());

        assert_eq!(
            (
                prepared.backgrounds.as_slice(),
                prepared.selections.as_slice()
            ),
            (
                [BackgroundSpan {
                    start: 0,
                    len: 1,
                    color: Color::rgb(0x65_43_21),
                }]
                .as_slice(),
                [BackgroundSpan {
                    start: 0,
                    len: 1,
                    color: TerminalColors::default().selection_background,
                }]
                .as_slice(),
            )
        );
        assert!(prepared.fragments.is_empty());
        assert!(prepared.symbols.is_empty());
        assert!(prepared.under_text_decorations.is_empty());
        assert!(prepared.over_text_decorations.is_empty());
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
    fn text_runs_preserve_font_and_color_transitions() {
        let colors = colors();
        let family: SharedString = "JetBrains Mono".into();
        let terminal_fonts = test_terminal_fonts(&family);
        let mut fragment = FragmentBuilder::new(0, false);
        let styles = [(false, false), (true, false), (true, true), (false, true)];
        let mut expected_fonts = Vec::new();
        let mut expected_paint = Vec::new();
        for (bold, italic) in styles {
            expected_fonts.push(TextRun {
                len: 8,
                font: terminal_cell_font(&family, bold, italic),
                color: rgba(0).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
            for foreground in [Color::rgb(0x12_34_56), Color::rgb(0x65_43_21)] {
                let mut subject = cell("é");
                subject.bold = bold;
                subject.italic = italic;
                subject.foreground_source = TerminalColor::Rgb(foreground);
                let foreground = effective_colors(&subject, &colors).0;
                fragment.push(&subject, foreground, &terminal_fonts);
                fragment.push(&subject, foreground, &terminal_fonts);
                expected_paint.push(TextPaintRun {
                    end: (expected_paint.len() + 1) * 4,
                    color: gpui_color(foreground).into(),
                });
            }
        }

        let fragment = fragment.finish(true);

        assert_eq!(fragment.runs, expected_fonts);
        assert_eq!(fragment.paint_runs.as_ref(), expected_paint);
        assert_eq!(fragment.text.as_ref(), "é".repeat(16));
    }

    #[test]
    fn text_runs_keep_family_changes_and_ignore_empty_cells() {
        let colors = colors();
        let first_family: SharedString = "Menlo".into();
        let second_family: SharedString = "JetBrains Mono".into();
        let first_fonts = test_terminal_fonts(&first_family);
        let second_fonts = test_terminal_fonts(&second_family);
        let mut fragment = FragmentBuilder::new(0, false);
        fragment.push(&cell("a"), colors.foreground, &first_fonts);
        let mut empty = cell("");
        empty.bold = true;
        empty.italic = true;
        fragment.push(&empty, colors.foreground, &second_fonts);
        fragment.push(&cell("b"), colors.foreground, &first_fonts);
        fragment.push(&cell("é"), colors.foreground, &second_fonts);
        fragment.push(&cell("c"), colors.foreground, &second_fonts);

        let fragment = fragment.finish(true);

        assert_eq!(fragment.text.as_ref(), "abéc");
        assert_eq!(fragment.runs.len(), 2);
        assert_eq!(fragment.runs[0].len, 2);
        assert_eq!(
            fragment.runs[0].font,
            terminal_cell_font(&first_family, false, false)
        );
        assert_eq!(fragment.runs[1].len, 3);
        assert_eq!(
            fragment.runs[1].font,
            terminal_cell_font(&second_family, false, false)
        );
    }

    #[test]
    fn row_preparation_constructs_the_font_set_once_instead_of_per_cell() {
        let row = Arc::<[CellSnapshot]>::from(vec![cell("é"); 192]);
        let colors = colors();
        let family: SharedString = "Menlo".into();
        let before = TERMINAL_FONT_PREPARATIONS.with(std::cell::Cell::get);

        let input = prepare_row(&row, &colors, &family);

        let prepared = TERMINAL_FONT_PREPARATIONS.with(std::cell::Cell::get) - before;
        assert_eq!(prepared, 4);
        assert_eq!(input.fragments.len(), 1);
        assert_eq!(input.fragments[0].runs.len(), 1);
        assert_eq!(input.fragments[0].runs[0].len, 384);
    }

    #[test]
    fn terminal_text_runs_configure_emoji_and_system_fallbacks() {
        let row = Arc::<[CellSnapshot]>::from([cell("A")]);

        let input = prepare_row(&row, &colors(), &"JetBrains Mono".into());

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
        let accent = TerminalColors::default().normal[1];
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
    fn explicit_background_matching_default_still_paints_over_transparency() {
        let colors = colors();
        let mut explicit = cell("a");
        explicit.background_source = TerminalColor::Rgb(colors.background);
        let row = Arc::<[CellSnapshot]>::from([cell(" "), explicit]);
        let input = prepare_row(&row, &colors, &"Menlo".into());
        assert_eq!(
            input.backgrounds,
            vec![BackgroundSpan {
                start: 1,
                len: 1,
                color: colors.background
            }]
        );
    }

    #[test]
    fn prepared_background_plan_distinguishes_find_matches_and_paints_selection_on_top() {
        let mut selected = cell("a");
        selected.background_source = TerminalColor::Palette(1);
        selected.selected = true;
        let row = Arc::<[CellSnapshot]>::from([selected]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());
        let spans = [
            FindHighlightSpan {
                row: 0,
                start_column: 0,
                end_column: 0,
                current: false,
            },
            FindHighlightSpan {
                row: 0,
                start_column: 0,
                end_column: 0,
                current: true,
            },
        ];

        let configured = TerminalColors::default();
        let find_backgrounds = find_background_spans(0, &spans, &configured);
        let stable = Arc::new(PreparedRow {
            text: Vec::new(),
            symbols: PreparedDecorations::default(),
            backgrounds: prepare_background_geometry(
                &input.backgrounds,
                Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(20.0))),
                px(8.0),
            ),
            selections: prepare_background_geometry(
                &input.selections,
                Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(20.0))),
                px(8.0),
            ),
            under_text_decorations: PreparedDecorations::default(),
            over_text_decorations: PreparedDecorations::default(),
        });
        let mut prepared = PreparedFrameRow::new(stable);
        prepared.find_backgrounds = prepare_background_geometry(
            &find_backgrounds,
            Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(20.0))),
            px(8.0),
        );
        prepared.cursor_background = Some(fill(
            Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(20.0))),
            rgba(0xffff_ffff),
        ));

        assert_eq!(
            (
                find_backgrounds
                    .iter()
                    .map(|background| background.color)
                    .collect::<Vec<_>>(),
                prepared
                    .backgrounds_in_paint_order()
                    .map(|(layer, _)| layer)
                    .collect::<Vec<_>>(),
            ),
            (
                vec![
                    configured.find_match_background,
                    configured.find_active_match_background,
                ],
                vec![
                    BackgroundPaintLayer::Terminal,
                    BackgroundPaintLayer::Find,
                    BackgroundPaintLayer::Find,
                    BackgroundPaintLayer::Selection,
                    BackgroundPaintLayer::Cursor,
                ],
            )
        );
    }

    #[test]
    fn interaction_foregrounds_follow_selection_then_active_find_precedence() {
        let mut colors = colors();
        let configured = Arc::make_mut(&mut colors.configured);
        configured.selection_foreground = Some(Color::rgb(0x11_22_33));
        configured.find_match_foreground = Some(Color::rgb(0x44_55_66));
        configured.find_active_match_foreground = Some(Color::rgb(0x77_88_99));
        let mut selected = cell("b");
        selected.selected = true;
        let row = Arc::<[CellSnapshot]>::from([cell("a"), selected, cell("c")]);
        let find_spans = Arc::from([
            FindHighlightSpan {
                row: 0,
                start_column: 0,
                end_column: 1,
                current: false,
            },
            FindHighlightSpan {
                row: 0,
                start_column: 2,
                end_column: 2,
                current: true,
            },
        ]);
        let fonts = test_terminal_fonts(&"Menlo".into());

        let input = prepare_row_cached(&row, &colors, &fonts, 0, &find_spans);
        let run_colors = input
            .fragments
            .iter()
            .flat_map(|fragment| fragment.paint_runs.iter().map(|run| run.color))
            .collect::<Vec<_>>();

        assert_eq!(
            run_colors,
            [
                gpui_color(Color::rgb(0x44_55_66)).into(),
                gpui_color(Color::rgb(0x11_22_33)).into(),
                gpui_color(Color::rgb(0x77_88_99)).into(),
            ]
        );
    }

    #[test]
    fn invisible_cells_keep_backgrounds_without_preparing_foreground_text() {
        let accent = TerminalColors::default().normal[1];
        let mut invisible = cell("secret");
        invisible.invisible = true;
        invisible.background_source = crate::terminal::TerminalColor::Rgb(accent);
        let row = Arc::<[CellSnapshot]>::from([invisible]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

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
        let accent = TerminalColors::default().normal[1];
        let mut decorated = cell("界");
        decorated.underline = crate::terminal::TerminalUnderlineSnapshot::Double;
        decorated.underline_source = crate::terminal::TerminalColor::Rgb(accent);
        decorated.overline = true;
        decorated.strikethrough = true;
        let mut tail = decorated.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([decorated, tail]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

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
    fn hyperlink_hover_underlines_only_undecorated_cells_in_mixed_spans() {
        let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
        let mut plain_first = cell("a");
        plain_first.hyperlink = Some(Arc::new(link.clone()));
        let mut decorated = cell("b");
        decorated.hyperlink = Some(Arc::new(link.clone()));
        decorated.underline = crate::terminal::TerminalUnderlineSnapshot::Curly;
        let mut plain_second = cell("c");
        plain_second.hyperlink = Some(Arc::new(link.clone()));
        let mut other = cell("d");
        other.hyperlink = crate::terminal::HyperlinkTarget::url("https://other.test").map(Arc::new);
        let row = Arc::<[CellSnapshot]>::from([plain_first, decorated, plain_second, other]);
        let screen = ScreenSnapshot::from_test_parts(
            Arc::from([Arc::clone(&row)]),
            crate::terminal::ScrollbarSnapshot::default(),
            "test",
        );
        let occurrence =
            hyperlink_occurrence(&screen, Some((link.identity, CellGridPosition::new(0, 0))));

        let hyperlink = TerminalColors::default().hyperlink;
        let spans = hyperlink_hover_underline_spans(&row, 0, occurrence, hyperlink);

        assert_eq!(
            spans,
            [
                DecorationSpan {
                    start: 0,
                    len: 1,
                    kind: DecorationKind::Underline(TerminalUnderlineSnapshot::Single),
                    color: hyperlink,
                    blinking: false,
                },
                DecorationSpan {
                    start: 2,
                    len: 1,
                    kind: DecorationKind::Underline(TerminalUnderlineSnapshot::Single),
                    color: hyperlink,
                    blinking: false,
                },
            ]
        );
    }

    #[test]
    fn hyperlink_hover_underlines_only_the_hovered_occurrence_when_targets_repeat() {
        let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
        let mut first = cell("a");
        first.hyperlink = Some(Arc::new(link.clone()));
        let separator = cell(" ");
        let mut second = cell("b");
        second.hyperlink = Some(Arc::new(link.clone()));
        let row = Arc::<[CellSnapshot]>::from([first, separator, second]);
        let screen = ScreenSnapshot::from_test_parts(
            Arc::from([Arc::clone(&row)]),
            crate::terminal::ScrollbarSnapshot::default(),
            "test",
        );
        let occurrence =
            hyperlink_occurrence(&screen, Some((link.identity, CellGridPosition::new(2, 0))));

        assert_eq!(
            hyperlink_hover_underline_spans(
                &row,
                0,
                occurrence,
                TerminalColors::default().hyperlink,
            ),
            [DecorationSpan {
                start: 2,
                len: 1,
                kind: DecorationKind::Underline(TerminalUnderlineSnapshot::Single),
                color: TerminalColors::default().hyperlink,
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

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(input.selections.len(), 1);
        assert!(
            input
                .under_text_decorations
                .iter()
                .chain(&input.over_text_decorations)
                .all(|span| span.color == colors().background && span.blinking)
        );
        let metrics = decoration_metrics(px(15.0), px(11.0), px(4.0), px(8.0), px(20.0), 2.0);
        let prepared = prepare_decoration_geometry(
            &input.under_text_decorations,
            px(0.0),
            px(0.0),
            px(8.0),
            metrics,
        );
        assert!(prepared.quads.iter().all(|quad| quad.blinking));

        decorated.invisible = true;
        let invisible = Arc::<[CellSnapshot]>::from([decorated]);
        let invisible = prepare_row(&invisible, &colors(), &"Menlo".into());
        assert!(invisible.under_text_decorations.is_empty());
        assert!(invisible.over_text_decorations.is_empty());
        assert_eq!(invisible.selections.len(), 1);
    }

    #[test]
    fn decoration_metrics_follow_font_metrics_and_snap_to_retina_pixels() {
        let metrics = decoration_metrics(px(15.2), px(11.1), px(4.0), px(8.2), px(20.0), 2.0);

        assert_eq!(metrics.device_pixel, px(0.5));
        assert_eq!(metrics.thickness, px(0.5));
        assert_eq!(metrics.underline_y, px(17.5));
        assert_eq!(metrics.double_underline_y, px(18.5));
        assert_eq!(metrics.strikethrough_y, px(11.0));
        assert_eq!(metrics.overline_y, px(4.0));
        assert!(metrics.wave_amplitude >= metrics.device_pixel);
    }

    #[test]
    fn decoration_metrics_keep_the_tallest_underline_inside_its_cell_row() {
        let metrics = decoration_metrics(px(19.0), px(15.0), px(6.0), px(8.0), px(20.0), 1.0);

        assert_eq!(
            (
                metrics.underline_y,
                metrics.double_underline_y + metrics.thickness,
            ),
            (px(17.0), px(20.0))
        );
    }

    #[test]
    fn underline_variants_produce_distinct_cell_clipped_geometry() {
        let metrics = decoration_metrics(px(15.0), px(11.0), px(4.0), px(8.0), px(20.0), 2.0);
        let span = |kind| DecorationSpan {
            start: 0,
            len: 2,
            kind: DecorationKind::Underline(kind),
            color: Color::rgb(0x12_34_56),
            blinking: false,
        };
        let prepare =
            |kind| prepare_decoration_geometry(&[span(kind)], px(0.0), px(0.0), px(8.0), metrics);

        let single = prepare(crate::terminal::TerminalUnderlineSnapshot::Single);
        let double = prepare(crate::terminal::TerminalUnderlineSnapshot::Double);
        let curly = prepare(crate::terminal::TerminalUnderlineSnapshot::Curly);
        let dotted = prepare(crate::terminal::TerminalUnderlineSnapshot::Dotted);
        let dashed = prepare(crate::terminal::TerminalUnderlineSnapshot::Dashed);

        assert_eq!(single.quads.len(), 1);
        assert_eq!(double.quads.len(), 2);
        assert_eq!(curly.underlines.len(), 1);
        assert!(curly.underlines[0].style.wavy);
        assert!(dotted.quads.len() > dashed.quads.len());
        assert!(dashed.quads.len() > single.quads.len());
        assert!(
            single
                .quads
                .iter()
                .all(|quad| quad.quad.bounds.right() <= px(16.0))
        );
        assert!(
            double
                .quads
                .iter()
                .all(|quad| quad.quad.bounds.right() <= px(16.0))
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
                color: TerminalColors::default().selection_background,
            }]
        );
    }

    #[test]
    fn adjacent_narrow_cells_share_one_shaping_fragment() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("b"), cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(
            input
                .fragments
                .iter()
                .map(|fragment| fragment.text.as_ref())
                .collect::<Vec<_>>(),
            vec!["abc"]
        );
    }

    #[test]
    fn shaping_fragments_do_not_cross_text_blink_boundaries() {
        let mut blinking = cell("b");
        blinking.blinking = true;
        let row = Arc::<[CellSnapshot]>::from([cell("a"), blinking, cell("c")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

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

        let input = prepare_row(&row, &colors(), &"Menlo".into());

        assert_eq!(input.fragments.len(), 4);
        assert_eq!(input.fragments[0].start, 0);
        assert_eq!(input.fragments[0].text.as_ref(), "界");
        assert!(!input.fragments[0].simple_cells);
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

        let input = prepare_row(&row, &colors(), &"Menlo".into());

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
    fn symbol_prepaint_preserves_blink_demand_and_terminal_width() {
        let mut block = cell("█");
        block.blinking = true;
        let mut tail = block.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([block, tail]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());
        let mut symbol_plans = SymbolPlanCache::default();

        let prepared = prepare_symbol_geometry(
            &input.symbols,
            px(10.0),
            px(30.0),
            px(5.0),
            px(8.0),
            1.0,
            &mut symbol_plans,
        );

        assert!(prepared.quads[0].blinking);
        assert!(!text_fragment_visible(prepared.quads[0].blinking, false));
        assert_eq!(prepared.quads.len(), 1);
        assert_eq!(
            prepared.quads[0].quad.bounds,
            Bounds::new(point(px(5.0), px(10.0)), size(px(16.0), px(20.0)))
        );
    }

    #[test]
    fn symbol_prepaint_snaps_origins_to_backing_pixels() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("a"), cell("█")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());
        let mut symbol_plans = SymbolPlanCache::default();

        let visible = prepare_symbol_geometry(
            &input.symbols,
            px(10.1),
            px(30.1),
            px(5.1),
            px(8.25),
            2.0,
            &mut symbol_plans,
        );

        assert_eq!(visible.quads[0].quad.bounds.origin.x, px(21.5));
        assert_eq!(visible.quads[0].quad.bounds.origin.y, px(10.0));
    }

    #[test]
    fn vector_symbols_prepare_flat_cell_local_quads() {
        let row = Arc::<[CellSnapshot]>::from([cell("\u{e0b0}"), cell("\u{e0b1}")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());
        let mut symbol_plans = SymbolPlanCache::default();

        let prepared = prepare_symbol_geometry(
            &input.symbols,
            px(10.0),
            px(30.0),
            px(5.0),
            px(8.0),
            1.0,
            &mut symbol_plans,
        );

        assert!(prepared.quads.len() > 2);
        assert!(prepared.quads.iter().all(|prepared| {
            prepared.quad.bounds.left() >= px(5.0)
                && prepared.quad.bounds.right() <= px(21.0)
                && prepared.quad.bounds.top() >= px(10.0)
                && prepared.quad.bounds.bottom() <= px(30.0)
        }));
    }

    #[test]
    fn adjacent_symbol_bounds_share_backing_pixel_edges() {
        let first = snapped_symbol_bounds(px(5.1), px(10.1), px(30.35), px(8.25), 0, 1, 2.0);
        let second = snapped_symbol_bounds(px(5.1), px(10.1), px(30.35), px(8.25), 1, 1, 2.0);
        let next_row = snapped_symbol_bounds(px(5.1), px(30.35), px(50.6), px(8.25), 0, 1, 2.0);

        assert_eq!(
            first.left_device + i32::from(first.width_device),
            second.left_device
        );
        assert_eq!(
            first.top_device + i32::from(first.height_device),
            next_row.top_device
        );
        assert_eq!((first.width_device, second.width_device), (17, 16));
    }

    #[test]
    fn adjacent_full_block_quads_share_backing_pixel_edges() {
        let row = Arc::<[CellSnapshot]>::from([cell("█"), cell("█"), cell("█")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into());
        let mut symbol_plans = SymbolPlanCache::default();

        let prepared = prepare_symbol_geometry(
            &input.symbols,
            px(10.1),
            px(30.35),
            px(5.1),
            px(8.25),
            2.0,
            &mut symbol_plans,
        );

        assert_eq!(prepared.quads.len(), 3);
        assert_eq!(
            prepared.quads[0].quad.bounds.right(),
            prepared.quads[1].quad.bounds.left()
        );
        assert_eq!(
            prepared.quads[1].quad.bounds.right(),
            prepared.quads[2].quad.bounds.left()
        );
    }

    #[test]
    fn right_to_left_cells_keep_terminal_cell_order() {
        let row = Arc::<[CellSnapshot]>::from([cell("א"), cell("ב"), cell("ג")]);

        let input = prepare_row(&row, &colors(), &"Menlo".into());

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
                .all(|fragment| !fragment.simple_cells)
        );
    }

    #[test]
    fn only_simple_narrow_cells_share_shaping_fragments() {
        assert!(is_simple_cell("a", 1));
        assert!(!is_simple_cell("界", 2));
        assert!(!is_simple_cell("e\u{301}", 1));
        assert!(!is_simple_cell("\u{2764}\u{fe0f}", 2));
        assert!(!is_simple_cell("👩\u{200d}💻", 2));
        assert!(!is_simple_cell("א", 1));
    }

    #[test]
    fn render_cache_reuses_unchanged_prepared_rows() {
        let first_row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let second_row = Arc::<[CellSnapshot]>::from([cell("b")]);
        let rows = Arc::<[RowSnapshot]>::from([Arc::clone(&first_row), Arc::clone(&second_row)]);
        let mut cache = TerminalGridCache::new();
        let terminal_fonts = test_terminal_fonts(&"Menlo".into());
        let find_spans = Arc::from([]);
        let first = cache.prepare(
            &rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        let changed_row = Arc::<[CellSnapshot]>::from([cell("c")]);
        let changed_rows = Arc::<[RowSnapshot]>::from([first_row, changed_row]);
        let second = cache.prepare(
            &changed_rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert!(!Arc::ptr_eq(&first[1], &second[1]));
    }

    #[test]
    fn render_cache_reuses_content_that_moves_between_row_indices() {
        let first_rows = Arc::<[RowSnapshot]>::from([
            Arc::<[CellSnapshot]>::from([cell("old")]),
            Arc::<[CellSnapshot]>::from([cell("e\u{301}")]),
            Arc::<[CellSnapshot]>::from([cell("界")]),
        ]);
        let mut cache = TerminalGridCache::new();
        let terminal_fonts = test_terminal_fonts(&"Menlo".into());
        let find_spans = Arc::from([]);
        let first = cache.prepare(
            &first_rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        let moved_rows = Arc::<[RowSnapshot]>::from([
            Arc::<[CellSnapshot]>::from([cell("e\u{301}")]),
            Arc::<[CellSnapshot]>::from([cell("界")]),
            Arc::<[CellSnapshot]>::from([cell("new")]),
        ]);
        let moved = cache.prepare(
            &moved_rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        assert_eq!(
            (
                Arc::ptr_eq(&first[1], &moved[0]),
                Arc::ptr_eq(&first[2], &moved[1]),
                Arc::ptr_eq(&first[0], &moved[2]),
            ),
            (true, true, false)
        );
    }

    fn matched_row_indices(
        current: &[i32],
        previous: &[i32],
        comparisons: &Cell<usize>,
    ) -> Vec<Option<usize>> {
        let alignment = find_row_alignment(current, previous, |_, row, cached| {
            comparisons.set(comparisons.get() + 1);
            row == cached
        });
        let mut previous = previous
            .iter()
            .copied()
            .enumerate()
            .map(Some)
            .collect::<Vec<_>>();
        current
            .iter()
            .enumerate()
            .map(|(index, row)| {
                take_aligned_row(&mut previous, index, alignment, |(_, cached)| {
                    comparisons.set(comparisons.get() + 1);
                    row == cached
                })
                .map(|(previous_index, _)| previous_index)
            })
            .collect()
    }

    #[test]
    fn row_alignment_handles_scrolling_in_both_directions() {
        let previous = [10, 11, 12, 13];

        assert_eq!(
            matched_row_indices(&[11, 12, 13, 14], &previous, &Cell::new(0)),
            [Some(1), Some(2), Some(3), None]
        );
        assert_eq!(
            matched_row_indices(&[9, 10, 11, 12], &previous, &Cell::new(0)),
            [None, Some(0), Some(1), Some(2)]
        );
    }

    #[test]
    fn row_alignment_has_linear_comparison_cost_when_frames_do_not_overlap() {
        let current = (0..64).collect::<Vec<_>>();
        let previous = (64..128).collect::<Vec<_>>();
        let comparisons = Cell::new(0);

        let matched = matched_row_indices(&current, &previous, &comparisons);

        assert!(matched.iter().all(Option::is_none));
        assert!(comparisons.get() <= current.len() * 3);
    }

    #[gpui::test]
    fn kitty_placeholder_protocol_never_reaches_text_or_block_cursor_shaping(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        use crate::terminal::testing::{TerminalEmulator, graphics_test_lock};

        let _guard = graphics_test_lock();
        let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
            CellGridSize::new(3, 2),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        ))
        .unwrap();
        emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=42,s=1,v=1,U=1,c=1,r=1,q=2;AQIDBA==\x1b\\");
        emulator.feed("a\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\x1b[39mz\r\x1b[C".as_bytes());
        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.graphics.placements.len(), 1);
        let position = snapshot.cursor.position.unwrap();
        assert_eq!(position.column, 1);
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let font_family: SharedString = "Menlo".into();
                let terminal_fonts = test_terminal_fonts(&font_family);
                let mut cache = TerminalGridCache::new();
                let rows = cache.prepare(
                    &snapshot.rows,
                    &snapshot.colors,
                    &terminal_fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                let geometry = cache.prepare_visible_geometry(
                    &rows,
                    1,
                    prepared_grid_layout(&terminal_fonts, px(14.0), px(8.0)),
                    window,
                );

                assert_eq!(
                    geometry[0]
                        .text
                        .iter()
                        .map(|text| (text.origin.x, text.line.text.as_ref()))
                        .collect::<Vec<_>>(),
                    [(px(0.0), "a"), (px(16.0), "z")]
                );
            })
            .expect("the test window should remain available");
    }

    #[gpui::test]
    fn shaped_text_cache_reuses_moved_unicode_and_repositions_geometry(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let font_family: SharedString = "Menlo".into();
                let terminal_fonts = test_terminal_fonts(&font_family);
                let first_rows = Arc::<[RowSnapshot]>::from([
                    Arc::<[CellSnapshot]>::from([cell("old")]),
                    Arc::<[CellSnapshot]>::from([cell("e\u{301}")]),
                ]);
                let mut cache = TerminalGridCache::new();
                let first_inputs = cache.prepare(
                    &first_rows,
                    &colors(),
                    &terminal_fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                let first_geometry = cache.prepare_visible_geometry(
                    &first_inputs,
                    2,
                    prepared_grid_layout(&terminal_fonts, px(14.0), px(8.0)),
                    window,
                );
                let first_text = Arc::clone(&cache.prepared_text[1].prepared);
                let first_line = Arc::clone(&first_geometry[1].text[0].line);
                let first_origin = first_geometry[1].text[0].origin;

                let moved_rows = Arc::<[RowSnapshot]>::from([
                    Arc::<[CellSnapshot]>::from([cell("e\u{301}")]),
                    Arc::<[CellSnapshot]>::from([cell("new")]),
                ]);
                let moved_inputs = cache.prepare(
                    &moved_rows,
                    &colors(),
                    &terminal_fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                let moved_geometry = cache.prepare_visible_geometry(
                    &moved_inputs,
                    2,
                    prepared_grid_layout(&terminal_fonts, px(14.0), px(8.0)),
                    window,
                );

                assert_eq!(
                    (
                        Arc::ptr_eq(&first_text, &cache.prepared_text[0].prepared),
                        Arc::ptr_eq(&first_line, &moved_geometry[0].text[0].line),
                        first_origin.y,
                        moved_geometry[0].text[0].origin.y,
                        moved_geometry[0].text[0].line.text.as_ref(),
                    ),
                    (true, true, px(20.0), px(0.0), "e\u{301}")
                );
            })
            .expect("the test window should remain available");
    }

    #[gpui::test]
    fn shaped_text_cache_invalidates_when_shaping_geometry_changes(cx: &mut gpui::TestAppContext) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let font_family: SharedString = "Menlo".into();
                let terminal_fonts = test_terminal_fonts(&font_family);
                let rows = Arc::<[RowSnapshot]>::from([Arc::<[CellSnapshot]>::from([cell("a")])]);
                let mut cache = TerminalGridCache::new();
                let inputs = cache.prepare(
                    &rows,
                    &colors(),
                    &terminal_fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                cache.prepare_visible_geometry(
                    &inputs,
                    1,
                    prepared_grid_layout(&terminal_fonts, px(14.0), px(8.0)),
                    window,
                );
                let first = Arc::clone(&cache.prepared_text[0].prepared);

                cache.prepare_visible_geometry(
                    &inputs,
                    1,
                    prepared_grid_layout(&terminal_fonts, px(15.0), px(8.0)),
                    window,
                );

                assert!(!Arc::ptr_eq(&first, &cache.prepared_text[0].prepared));
            })
            .expect("the test window should remain available");
    }

    #[gpui::test]
    fn selection_and_find_preserve_shaped_glyphs_at_fractional_cell_widths(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let fonts = test_terminal_fonts(&"Menlo".into());
                let mut colors = colors();
                let selected_color = Color::rgb(0x12_34_56);
                let find_color = Color::rgb(0x65_43_21);
                Arc::make_mut(&mut colors.configured).selection_foreground = Some(selected_color);
                Arc::make_mut(&mut colors.configured).find_match_foreground = Some(find_color);
                let mut cells = "abcdefghijklmnop"
                    .chars()
                    .map(|ch| cell(&ch.to_string()))
                    .collect::<Vec<_>>();
                cells[4].bold = true;
                cells[7].italic = true;
                let mut tail = cell(" ");
                tail.spacer_tail = true;
                cells.extend([
                    cell("e\u{301}"),
                    cell("界"),
                    tail.clone(),
                    cell("😀"),
                    tail,
                    cell("z"),
                ]);
                let rows = Arc::<[RowSnapshot]>::from([Arc::from(cells.clone())]);
                let mut cache = TerminalGridCache::new();
                let inputs = cache.prepare(&rows, &colors, &fonts, &Arc::from([]), grid_metrics());
                let original = cache.prepare_visible_geometry(
                    &inputs,
                    1,
                    prepared_grid_layout(&fonts, px(14.0), px(8.375)),
                    window,
                );
                for selecting in [true, false] {
                    for end in 1..=cells.len() {
                        let mut changed = cells.clone();
                        let spans = if selecting {
                            for cell in &mut changed[..end] {
                                cell.selected = true;
                            }
                            Arc::from([])
                        } else {
                            Arc::from([FindHighlightSpan {
                                row: 0,
                                start_column: 0,
                                end_column: (end - 1) as u16,
                                current: false,
                            }])
                        };
                        let rows = Arc::<[RowSnapshot]>::from([Arc::from(changed)]);
                        let inputs = cache.prepare(&rows, &colors, &fonts, &spans, grid_metrics());
                        let prepared = cache.prepare_visible_geometry(
                            &inputs,
                            1,
                            prepared_grid_layout(&fonts, px(14.0), px(8.375)),
                            window,
                        );
                        assert_eq!(
                            original[0].text.len(),
                            prepared[0].text.len(),
                            "selection={selecting}, end={end}"
                        );
                        for (before, after) in original[0].text.iter().zip(&prepared[0].text) {
                            // Reusing the shaped line preserves every glyph, position, and baseline.
                            assert!(
                                Arc::ptr_eq(&before.line, &after.line),
                                "selection={selecting}, end={end}"
                            );
                            assert_eq!(before.origin, after.origin);
                        }
                        let expected = if selecting {
                            selected_color
                        } else {
                            find_color
                        };
                        assert_eq!(
                            prepared[0].text[0].paint_runs[0].color,
                            gpui_color(expected).into()
                        );
                        assert_eq!(!prepared[0].selections.is_empty(), selecting);
                    }
                }
            })
            .unwrap();
    }

    #[gpui::test]
    fn scope_guide_redraw_preserves_suffix_glyph_raster_positions(cx: &mut gpui::TestAppContext) {
        cx.set_glyph_raster_bounds(Bounds::new(
            point(gpui::DevicePixels(0), gpui::DevicePixels(-10)),
            size(gpui::DevicePixels(6), gpui::DevicePixels(12)),
        ));
        let cx = cx.add_empty_window();
        let suffix_color = Color::rgb(0x12_34_56);
        let mut baseline = None;
        for guide in [" ", "│", " "] {
            let capture = PaintCapture::default();
            let paint_capture = capture.clone();
            cx.draw(
                point(px(0.0), px(0.0)),
                size(px(1100.0), px(40.0)),
                move |window, _| {
                    let fonts = test_terminal_fonts(&"Menlo".into());
                    let mut cells = vec![cell(" "); 4];
                    cells[3] = cell(guide);
                    cells.extend((0..120).map(|_| {
                        let mut suffix = cell("a");
                        suffix.foreground_source = TerminalColor::Rgb(suffix_color);
                        suffix
                    }));
                    let rows = Arc::<[RowSnapshot]>::from([Arc::from(cells)]);
                    let mut cache = TerminalGridCache::new();
                    let inputs =
                        cache.prepare(&rows, &colors(), &fonts, &Arc::from([]), grid_metrics());
                    let mut layout = prepared_grid_layout(&fonts, px(14.0), px(8.41));
                    layout.grid_bounds.size.width = px(1100.0);
                    let stable = cache.prepare_visible_geometry(&inputs, 1, layout, window);
                    PaintBatches {
                        batches: vec![TerminalPaintBatch {
                            surface: None,
                            grid_bounds: layout.grid_bounds,
                            line_height: layout.line_height,
                            rows: stable.iter().cloned().map(PreparedFrameRow::new).collect(),
                            cursor_text_overlay: None,
                            graphics: GraphicsPaintPlan::default(),
                            blink_phase_visible: true,
                        }],
                        capture: paint_capture,
                    }
                },
            );
            let positions = capture
                .glyphs
                .borrow()
                .iter()
                .filter_map(|glyph| {
                    matches!(glyph.kind, gpui::PaintedGlyphKindForTest::Monochrome { color }
                    if color == Hsla::from(gpui_color(suffix_color)))
                    .then_some(glyph.raster_bounds.origin)
                })
                .collect::<Vec<_>>();
            assert_eq!(positions.len(), 120);
            if let Some(baseline) = &baseline {
                assert_eq!(
                    &positions, baseline,
                    "guide={guide:?} must not move unchanged text"
                );
            } else {
                baseline = Some(positions);
            }
        }
    }

    #[gpui::test]
    fn cursor_motion_preserves_absolute_glyph_positions_at_fractional_cell_widths(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let fonts = test_terminal_fonts(&"Menlo".into());
                let mut cells = "abcdefghijklmnop"
                    .chars()
                    .map(|ch| cell(&ch.to_string()))
                    .collect::<Vec<_>>();
                let mut tail = cell(" ");
                tail.spacer_tail = true;
                cells.extend([cell("e\u{301}"), cell("界"), tail, cell("z")]);
                let rows = Arc::<[RowSnapshot]>::from([
                    Arc::from(cells.clone()),
                    Arc::from(cells.clone()),
                ]);
                let mut cache = TerminalGridCache::new();
                let absolute_glyph_positions = |row: &PreparedRow| {
                    row.text
                        .iter()
                        .flat_map(|text| text.glyph_origins.iter().copied())
                        .collect::<Vec<_>>()
                };

                let inputs =
                    cache.prepare(&rows, &colors(), &fonts, &Arc::from([]), grid_metrics());
                let (baseline, _) = cache.prepare_frame_geometry(
                    &inputs,
                    rows.len(),
                    prepared_grid_layout(&fonts, px(14.0), px(8.375)),
                    None,
                    CursorSnapshot::default(),
                    window,
                );
                let baseline_positions = baseline
                    .iter()
                    .map(|row| absolute_glyph_positions(row))
                    .collect::<Vec<_>>();

                for shape in [
                    CursorShapeSnapshot::Bar,
                    CursorShapeSnapshot::Block,
                    CursorShapeSnapshot::Underline,
                ] {
                    for (row, column) in [0, 1, 0]
                        .into_iter()
                        .flat_map(|row| [0, 5, 15, 16, 17, 19].map(|column| (row, column)))
                    {
                        let position = CursorPositionSnapshot {
                            row,
                            column,
                            width_cells: u8::from(column == 17) + 1,
                        };
                        let inputs =
                            cache.prepare(&rows, &colors(), &fonts, &Arc::from([]), grid_metrics());
                        let cursor = (position, cells[usize::from(column)].clone());
                        let (prepared, _) = cache.prepare_frame_geometry(
                            &inputs,
                            rows.len(),
                            prepared_grid_layout(&fonts, px(14.0), px(8.375)),
                            Some(&cursor),
                            CursorSnapshot {
                                visible: true,
                                shape,
                                ..CursorSnapshot::default()
                            },
                            window,
                        );

                        assert_eq!(
                            prepared
                                .iter()
                                .map(|row| absolute_glyph_positions(row))
                                .collect::<Vec<_>>(),
                            baseline_positions,
                            "shape={shape:?}, row={row}, column={column}"
                        );
                        assert!(
                            baseline
                                .iter()
                                .zip(prepared.iter())
                                .all(|(before, after)| { Arc::ptr_eq(before, after) }),
                            "cursor movement must reuse every row's stable geometry"
                        );
                    }
                }
            })
            .unwrap();
    }

    #[gpui::test]
    fn shaped_text_cache_reuses_colors_but_invalidates_prepared_fonts(
        cx: &mut gpui::TestAppContext,
    ) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let rows = Arc::<[RowSnapshot]>::from([Arc::<[CellSnapshot]>::from([cell("a")])]);
                let first_fonts = test_terminal_fonts(&"Menlo".into());
                let mut second_fonts = first_fonts.clone();
                second_fonts.resolution_identity = "Menlo:resolved-face-2".to_owned();
                let no_find = Arc::from([]);
                let mut first_colors = colors();
                let mut cache = TerminalGridCache::new();
                let first_inputs =
                    cache.prepare(&rows, &first_colors, &first_fonts, &no_find, grid_metrics());
                let first_geometry = cache.prepare_visible_geometry(
                    &first_inputs,
                    1,
                    prepared_grid_layout(&first_fonts, px(14.0), px(8.0)),
                    window,
                );
                let first_line = Arc::clone(&first_geometry[0].text[0].line);
                let first_color = first_geometry[0].text[0].paint_runs[0].color;

                first_colors.foreground = Color::rgb(0x12_34_56);
                Arc::make_mut(&mut first_colors.configured).foreground = first_colors.foreground;
                let color_inputs =
                    cache.prepare(&rows, &first_colors, &first_fonts, &no_find, grid_metrics());
                let color_geometry = cache.prepare_visible_geometry(
                    &color_inputs,
                    1,
                    prepared_grid_layout(&first_fonts, px(14.0), px(8.0)),
                    window,
                );
                let color_line = Arc::clone(&color_geometry[0].text[0].line);
                let second_color = color_geometry[0].text[0].paint_runs[0].color;

                Arc::make_mut(&mut first_colors.configured).find_match_foreground =
                    Some(Color::rgb(0x65_43_21));
                let find_spans = Arc::from([FindHighlightSpan {
                    row: 0,
                    start_column: 0,
                    end_column: 0,
                    current: false,
                }]);
                let find_inputs = cache.prepare(
                    &rows,
                    &first_colors,
                    &first_fonts,
                    &find_spans,
                    grid_metrics(),
                );
                let find_geometry = cache.prepare_visible_geometry(
                    &find_inputs,
                    1,
                    prepared_grid_layout(&first_fonts, px(14.0), px(8.0)),
                    window,
                );
                let find_line = Arc::clone(&find_geometry[0].text[0].line);
                let find_color = find_geometry[0].text[0].paint_runs[0].color;

                let font_inputs = cache.prepare(
                    &rows,
                    &first_colors,
                    &second_fonts,
                    &find_spans,
                    grid_metrics(),
                );
                let font_geometry = cache.prepare_visible_geometry(
                    &font_inputs,
                    1,
                    prepared_grid_layout(&second_fonts, px(14.0), px(8.0)),
                    window,
                );

                assert!(Arc::ptr_eq(&first_line, &color_line));
                assert_ne!(first_color, second_color);
                assert!(Arc::ptr_eq(&color_line, &find_line));
                assert_ne!(second_color, find_color);
                assert!(!Arc::ptr_eq(&find_line, &font_geometry[0].text[0].line));
            })
            .expect("the test window should remain available");
    }

    #[gpui::test]
    fn shaped_text_cache_retains_only_visible_rows(cx: &mut gpui::TestAppContext) {
        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, _| {
                let font_family: SharedString = "Menlo".into();
                let terminal_fonts = test_terminal_fonts(&font_family);
                let rows = Arc::<[RowSnapshot]>::from([
                    Arc::<[CellSnapshot]>::from([cell("a")]),
                    Arc::<[CellSnapshot]>::from([cell("b")]),
                    Arc::<[CellSnapshot]>::from([cell("c")]),
                ]);
                let mut cache = TerminalGridCache::new();
                let inputs = cache.prepare(
                    &rows,
                    &colors(),
                    &terminal_fonts,
                    &Arc::from([]),
                    grid_metrics(),
                );
                cache.prepare_visible_geometry(
                    &inputs,
                    2,
                    prepared_grid_layout(&terminal_fonts, px(14.0), px(8.0)),
                    window,
                );

                assert_eq!(
                    (cache.prepared_text.len(), cache.prepared_geometry.len()),
                    (2, 2)
                );
            })
            .expect("the test window should remain available");
    }

    #[test]
    fn preedit_shape_cache_key_reuses_only_the_same_logical_cluster_snapshot() {
        let layout = layout_preedit("かな", 0, 0, 80, 2);
        let first = prepared_preedit_key(&layout, 24);
        let second = prepared_preedit_key(&layout, 24);
        let equal_content_new_snapshot = layout_preedit("かな", 0, 0, 80, 2);
        let replaced = prepared_preedit_key(&equal_content_new_snapshot, 24);

        assert_eq!(first, second);
        assert_ne!(first, replaced);
    }

    #[test]
    fn preedit_shape_cache_key_invalidates_when_visible_height_changes() {
        let layout = layout_preedit("かな", 0, 0, 80, 2);
        let first = prepared_preedit_key(&layout, 24);
        let resized = prepared_preedit_key(&layout, 25);

        assert_ne!(first, resized);
    }

    #[test]
    fn blink_frames_reuse_the_stable_geometry_buffer_without_clone_or_rebuild() {
        let source = Arc::new(prepare_row(
            &Arc::from([cell("a")]),
            &colors(),
            &"Menlo".into(),
        ));
        let builds = Cell::new(0);
        let mut cached = None;
        let stable = reuse_or_prepare_row(&mut cached, &source, prepared_row_key(), || {
            builds.set(builds.get() + 1);
            PreparedRow {
                text: Vec::new(),
                symbols: PreparedDecorations::default(),
                backgrounds: Vec::new(),
                selections: Vec::new(),
                under_text_decorations: PreparedDecorations {
                    quads: vec![PreparedQuad {
                        quad: fill(
                            Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(1.0))),
                            gpui_color(Color::rgb(0xff_ff_ff)),
                        ),
                        blinking: true,
                    }],
                    underlines: Vec::new(),
                },
                over_text_decorations: PreparedDecorations::default(),
            }
        });
        let stable_buffer = stable.under_text_decorations.quads.as_ptr();

        for _phase in [false, true] {
            let stable = reuse_or_prepare_row(&mut cached, &source, prepared_row_key(), || {
                builds.set(builds.get() + 1);
                panic!("blink phase must not rebuild stable row geometry")
            });
            let frame = PreparedFrameRow::new(Arc::clone(&stable));
            assert!(Arc::ptr_eq(&stable, &frame.stable));
            assert_eq!(
                frame.stable.under_text_decorations.quads.as_ptr(),
                stable_buffer
            );
        }

        assert_eq!(builds.get(), 1);
    }

    #[test]
    fn shaped_geometry_cache_invalidates_when_row_identity_changes() {
        let first_source = Arc::new(prepare_row(
            &Arc::from([cell("a")]),
            &colors(),
            &"Menlo".into(),
        ));
        let second_source = Arc::new(prepare_row(
            &Arc::from([cell("b")]),
            &colors(),
            &"Menlo".into(),
        ));
        let mut cached = None;
        let first = reuse_or_prepare_row(&mut cached, &first_source, prepared_row_key(), || ());
        let second = reuse_or_prepare_row(&mut cached, &second_source, prepared_row_key(), || ());

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn shaped_geometry_cache_invalidates_when_layout_geometry_changes() {
        let source = Arc::new(prepare_row(
            &Arc::from([cell("a")]),
            &colors(),
            &"Menlo".into(),
        ));
        let mut cached = None;
        let first_key = prepared_row_key();
        let first = reuse_or_prepare_row(&mut cached, &source, first_key, || ());
        let second = reuse_or_prepare_row(
            &mut cached,
            &source,
            PreparedRowKey {
                cell_width: px(9.0),
                ..first_key
            },
            || (),
        );

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn render_cache_invalidates_rows_when_color_semantics_change() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let first_colors = colors();
        let terminal_fonts = test_terminal_fonts(&"Menlo".into());
        let find_spans = Arc::from([]);
        let first = cache.prepare(
            &rows,
            &first_colors,
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        let mut changed_colors = first_colors.clone();
        Arc::make_mut(&mut changed_colors.palette)[1] = Color::rgb(0xff_00_00);
        let second = cache.prepare(
            &rows,
            &changed_colors,
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }

    #[test]
    fn scale_invalidation_should_discard_prepared_terminal_rows() {
        let row = Arc::<[CellSnapshot]>::from([cell("a")]);
        let rows = Arc::<[RowSnapshot]>::from([row]);
        let mut cache = TerminalGridCache::new();
        let terminal_fonts = test_terminal_fonts(&"Menlo".into());
        let find_spans = Arc::from([]);
        let first = cache.prepare(
            &rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        cache.invalidate_scale_dependent();
        let second = cache.prepare(
            &rows,
            &colors(),
            &terminal_fonts,
            &find_spans,
            grid_metrics(),
        );

        assert!(!Arc::ptr_eq(&first[0], &second[0]));
    }
}

#[cfg(test)]
mod idle_retention_tests {
    use super::*;

    #[test]
    fn evict_releases_retained_rows_and_geometry_capacity() {
        let mut cache = TerminalGridCache::new();
        let row: RowSnapshot = Arc::from([]);
        let retained = Arc::downgrade(&row);
        cache.row_inputs.push(PreparedRowInputCacheEntry {
            source: row,
            prepared: Arc::new(RowPaintInput {
                font_resolution_identity: String::new(),
                fragments: Vec::new(),
                symbols: Vec::new(),
                backgrounds: Vec::new(),
                selections: Vec::new(),
                under_text_decorations: Vec::new(),
                over_text_decorations: Vec::new(),
            }),
        });
        cache.prepared_text.push(PreparedRowTextCacheEntry {
            source: Arc::clone(&cache.row_inputs[0].prepared),
            key: PreparedRowTextKey {
                font_size: px(14.0),
                cell_width: px(8.0),
            },
            prepared: Arc::new(PreparedRowText { text: Vec::new() }),
        });
        cache.prepared_geometry.resize_with(128, || None);
        cache.evict();
        assert!(retained.upgrade().is_none());
        assert_eq!(
            (
                cache.row_inputs.capacity(),
                cache.prepared_text.capacity(),
                cache.prepared_geometry.capacity(),
            ),
            (0, 0, 0)
        );
    }
}
