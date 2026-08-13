use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, Font, FontFallbacks, FontFeatures, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Style, TextRun,
    UnderlineStyle, Window, fill, font, outline, point, px, relative, rgba, size,
};
use unicode_bidi::{BidiClass, bidi_class};

use crate::terminal::{
    CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, FindHighlightSpan,
    RowSnapshot, ScreenSnapshot, TerminalColor, TerminalColorsSnapshot, TerminalUnderlineSnapshot,
};
use crate::theme::{ACTIVE_THEME, Color};

use super::terminal_graphics::{
    GraphicsAttemptToken, GraphicsLayer, GraphicsPaintPlan, PreparedGraphics, TerminalGraphicsCache,
};
use super::terminal_ime::PreeditLayout;
use super::terminal_pane::{OperationToken, TerminalPane};
use super::terminal_symbols::{
    DevicePoint, SymbolPlan, SymbolPlanCache, SymbolPrimitive, terminal_symbol,
};

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
    prepared_geometry: Vec<Option<PreparedRowCacheEntry<PreparedRow>>>,
    preedit: Option<PreparedPreedit>,
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
            prepared_geometry: Vec::new(),
            preedit: None,
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
        self.prepared_geometry.clear();
        self.preedit = None;
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
    cache: Entity<TerminalGridCache>,
    columns: usize,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    cursor: Option<(CursorPositionSnapshot, CellSnapshot)>,
    cursor_style: CursorSnapshot,
    cursor_preparation_style: CursorSnapshot,
    font_family: SharedString,
    preedit: Option<PreeditLayout>,
    focus_handle: FocusHandle,
    input: Entity<TerminalPane>,
    blink_phase_visible: bool,
    find_spans: Arc<[FindHighlightSpan]>,
    graphics: PreparedGraphics,
    scale_factor: f32,
    presentation: Arc<ScreenSnapshot>,
    presentation_operation: Option<OperationToken>,
    graphics_attempt: Option<GraphicsAttemptToken>,
    graphics_cache: Entity<TerminalGraphicsCache>,
    fallback: Option<Box<TerminalGridElement>>,
    fallback_generation: Option<crate::terminal::PresentationGeneration>,
    paint_fault: Option<PaintPreflightFault>,
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
    pub(crate) blink_phase_visible: bool,
    pub(crate) scale_factor: f32,
    pub(crate) find_spans: Arc<[FindHighlightSpan]>,
    pub(crate) graphics: PreparedGraphics,
    pub(crate) presentation_operation: Option<OperationToken>,
    pub(crate) graphics_attempt: Option<GraphicsAttemptToken>,
    pub(crate) graphics_cache: Entity<TerminalGraphicsCache>,
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
    Glyph(usize),
    Image(usize),
}

impl TerminalGridElement {
    pub(crate) fn new(
        screen: &Arc<ScreenSnapshot>,
        cache: Entity<TerminalGridCache>,
        mut configuration: TerminalGridConfiguration,
        cx: &mut App,
    ) -> Self {
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
                        font_size: configuration.font_size,
                        line_height: configuration.line_height,
                        cell_width: configuration.cell_width,
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
                &configuration.font_family,
                screen.cursor.position,
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
            columns: screen.rows.first().map_or(0, |row| row.len()),
            font_size: configuration.font_size,
            line_height: configuration.line_height,
            cell_width: configuration.cell_width,
            cursor,
            cursor_style,
            cursor_preparation_style,
            font_family: configuration.font_family,
            preedit: configuration.preedit,
            focus_handle: configuration.focus_handle,
            input: configuration.input,
            blink_phase_visible: configuration.blink_phase_visible,
            find_spans: configuration.find_spans,
            graphics: configuration.graphics,
            scale_factor: configuration.scale_factor,
            presentation: Arc::clone(screen),
            presentation_operation: configuration.presentation_operation,
            graphics_attempt: configuration.graphics_attempt,
            graphics_cache: configuration.graphics_cache,
            fallback,
            fallback_generation,
            paint_fault: configuration.paint_fault,
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
    line: ShapedLine,
    origin: gpui::Point<Pixels>,
    blinking: bool,
}

struct PreparedRow {
    text: Vec<PreparedText>,
    symbols: PreparedDecorations,
    backgrounds: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    under_text_decorations: PreparedDecorations,
    over_text_decorations: PreparedDecorations,
    cursor_text: Vec<PreparedText>,
    cursor_symbols: PreparedDecorations,
}

struct PreparedFrameRow {
    stable: Arc<PreparedRow>,
    find_backgrounds: Vec<PaintQuad>,
    cursor_background: Option<PaintQuad>,
    cursor_overlay_visible: bool,
    preedit: Option<PreparedPreeditRow>,
}

#[derive(Clone, Debug)]
struct PreparedPreeditKey {
    clusters: Arc<[super::terminal_ime::PreeditCluster]>,
    caret: super::terminal_ime::PreeditPosition,
    visible_rows: usize,
    grid_left: Pixels,
    grid_top: Pixels,
    font_family: SharedString,
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
            && self.grid_left == other.grid_left
            && self.grid_top == other.grid_top
            && self.font_family == other.font_family
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
            cursor_background: None,
            cursor_overlay_visible: false,
            preedit: None,
        }
    }
}

pub(crate) struct PrepaintState {
    candidate: TerminalPaintBatch,
    fallback: Option<TerminalPaintBatch>,
}

struct TerminalPaintBatch {
    surface: Option<PaintQuad>,
    grid_bounds: Bounds<Pixels>,
    rows: Vec<PreparedFrameRow>,
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
        line_height: Pixels,
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
                    preflight_text(text, line_height, window)
                        .map_err(|_| PaintBatchFailure::Presentation)?;
                }
                if row.cursor_overlay_visible {
                    for text in row.stable.cursor_text.iter().filter(|text| {
                        text_fragment_visible(text.blinking, self.blink_phase_visible)
                    }) {
                        preflight_text(text, line_height, window)
                            .map_err(|_| PaintBatchFailure::Presentation)?;
                    }
                }
            }
            self.graphics
                .preflight_layer(GraphicsLayer::AboveText, window)
                .map_err(|_| PaintBatchFailure::RendererResources)?;
            for row in &self.rows {
                if let Some(preedit) = &row.preedit {
                    for text in preedit.text.iter() {
                        preflight_text(text, line_height, window)
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
                            row.cursor_overlay_visible
                                .then_some(&row.stable.cursor_text)
                                .into_iter()
                                .flat_map(|text| text.iter())
                                .filter(|text| {
                                    text_fragment_visible(text.blinking, self.blink_phase_visible)
                                }),
                        )
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
            PaintPreflightFault::Image(index) => (index < self.graphics.image_count())
                .then_some(PaintBatchFailure::RendererResources),
        }
    }

    fn submit(
        &self,
        grid_bounds: Bounds<Pixels>,
        line_height: Pixels,
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
                    for background in &row.stable.backgrounds {
                        window.paint_quad(background.clone());
                    }
                    for background in &row.find_backgrounds {
                        window.paint_quad(background.clone());
                    }
                    for background in &row.stable.selections {
                        window.paint_quad(background.clone());
                    }
                    if let Some(background) = &row.cursor_background {
                        window.paint_quad(background.clone());
                    }
                }
                self.graphics
                    .paint_layer(GraphicsLayer::BelowText, window)
                    .map_err(|_| PaintBatchFailure::RendererResources)?;
                for row in &self.rows {
                    paint_prepared_decorations(
                        &row.stable.under_text_decorations,
                        self.blink_phase_visible,
                        window,
                    );
                    paint_prepared_decorations(
                        &row.stable.symbols,
                        self.blink_phase_visible,
                        window,
                    );
                    for text in row.stable.text.iter().filter(|text| {
                        text_fragment_visible(text.blinking, self.blink_phase_visible)
                    }) {
                        text.line
                            .paint(text.origin, line_height, window, cx)
                            .map_err(|_| PaintBatchFailure::Presentation)?;
                    }
                    if row.cursor_overlay_visible {
                        paint_prepared_decorations(
                            &row.stable.cursor_symbols,
                            self.blink_phase_visible,
                            window,
                        );
                        for text in row.stable.cursor_text.iter().filter(|text| {
                            text_fragment_visible(text.blinking, self.blink_phase_visible)
                        }) {
                            text.line
                                .paint(text.origin, line_height, window, cx)
                                .map_err(|_| PaintBatchFailure::Presentation)?;
                        }
                    }
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
                                .paint(text.origin, line_height, window, cx)
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

fn preflight_text(
    text: &PreparedText,
    line_height: Pixels,
    window: &mut Window,
) -> gpui::Result<()> {
    let layout = &*text.line;
    let baseline = (line_height - layout.ascent - layout.descent) / 2.0 + layout.ascent;
    let mut glyph_origin = text.origin;
    let mut previous_position = gpui::Point::default();
    for run in &layout.runs {
        for glyph in &run.glyphs {
            glyph_origin += glyph.position - previous_position;
            previous_position = glyph.position;
            let origin = glyph_origin + point(px(0.0), baseline);
            if glyph.is_emoji {
                window.paint_emoji(origin, run.font_id, glyph.id, layout.font_size)?;
            } else {
                window.paint_glyph(
                    origin,
                    run.font_id,
                    glyph.id,
                    layout.font_size,
                    rgba(0).into(),
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct PreparedDecorations {
    // GPUI consumes Path and rebuilds its scaled vertex Vec during paint. Flat scene primitives
    // keep stable decorations reusable through the row Arc without cloning heap geometry.
    quads: Vec<PreparedQuad>,
    underlines: Vec<PreparedUnderline>,
}

struct PreparedQuad {
    quad: PaintQuad,
    blinking: bool,
}

struct PreparedUnderline {
    origin: gpui::Point<Pixels>,
    width: Pixels,
    style: UnderlineStyle,
    blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreparedRowKey {
    grid_left: Pixels,
    row_top: Pixels,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    decoration_metrics: DecorationMetrics,
    cursor: Option<PreparedCursorKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedCursorKey {
    position: CursorPositionSnapshot,
    style: CursorSnapshot,
}

struct PreparedGridLayout<'a> {
    grid_left: Pixels,
    grid_top: Pixels,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    font_family: &'a SharedString,
    decoration_metrics: DecorationMetrics,
}

struct PreparedRowCacheEntry<T> {
    source: Arc<RowPaintInput>,
    key: PreparedRowKey,
    prepared: Arc<T>,
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
    fn prepare_visible_geometry(
        &mut self,
        rows: &Arc<[Arc<RowPaintInput>]>,
        visible_rows: usize,
        layout: PreparedGridLayout<'_>,
        cursor: Option<&(CursorPositionSnapshot, CellSnapshot)>,
        cursor_style: CursorSnapshot,
        window: &mut Window,
    ) -> Vec<Arc<PreparedRow>> {
        self.prepared_geometry.resize_with(rows.len(), || None);
        self.prepared_geometry.truncate(rows.len());

        rows.iter()
            .take(visible_rows)
            .enumerate()
            .map(|(row_index, source)| {
                let row_top = layout.grid_top + layout.line_height * row_index as f32;
                let cursor = cursor
                    .filter(|(position, _)| usize::from(position.row) == row_index)
                    .filter(|_| cursor_style.visible);
                let key = PreparedRowKey {
                    grid_left: layout.grid_left,
                    row_top,
                    font_size: layout.font_size,
                    cell_width: layout.cell_width,
                    line_height: layout.line_height,
                    decoration_metrics: layout.decoration_metrics,
                    cursor: cursor.map(|(position, _)| PreparedCursorKey {
                        position: *position,
                        style: cursor_style,
                    }),
                };
                reuse_or_prepare_row(&mut self.prepared_geometry[row_index], source, key, || {
                    prepare_stable_row(
                        source,
                        key,
                        layout.font_family,
                        cursor,
                        cursor_style,
                        window,
                    )
                })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_preedit(
        &mut self,
        layout: Option<&PreeditLayout>,
        visible_rows: usize,
        grid_left: Pixels,
        grid_top: Pixels,
        font_family: &SharedString,
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
            grid_left,
            grid_top,
            font_family: font_family.clone(),
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
            let row_top = grid_top + line_height * row_index as f32;
            let clusters = layout
                .clusters
                .iter()
                .filter(|cluster| cluster.row == row_index);
            let mut text = Vec::new();
            let mut backgrounds = Vec::new();
            for cluster in clusters {
                let cluster_left = grid_left + cell_width * cluster.column as f32;
                let width_cells = usize::from(cluster.width).max(1);
                backgrounds.push(fill(
                    Bounds::new(
                        point(cluster_left, row_top),
                        size(cell_width * width_cells as f32, line_height),
                    ),
                    gpui_color(background),
                ));
                let color = gpui_color(foreground).into();
                text.push(PreparedText {
                    line: window.text_system().shape_line(
                        cluster.text.clone().into(),
                        font_size,
                        &[TextRun {
                            len: cluster.text.len(),
                            font: terminal_cell_font(font_family, false, false),
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
                    blinking: false,
                });
            }
            row.text = Arc::from(text);
            row.backgrounds = Arc::from(backgrounds);
            if layout.caret.row == row_index {
                let caret_left = grid_left + cell_width * layout.caret.column as f32;
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

fn prepare_stable_row(
    row: &RowPaintInput,
    key: PreparedRowKey,
    font_family: &SharedString,
    cursor: Option<&(CursorPositionSnapshot, CellSnapshot)>,
    cursor_style: CursorSnapshot,
    window: &mut Window,
) -> PreparedRow {
    let text = row
        .fragments
        .iter()
        .map(|fragment| PreparedText {
            line: window.text_system().shape_line(
                fragment.text.clone(),
                key.font_size,
                &fragment.runs,
                fragment.force_cell_width.then_some(key.cell_width),
            ),
            origin: point(
                key.grid_left + key.cell_width * fragment.start as f32,
                key.row_top,
            ),
            blinking: fragment.blinking,
        })
        .collect();
    let backgrounds = prepare_background_geometry(
        &row.backgrounds,
        key.row_top,
        key.grid_left,
        key.cell_width,
        key.line_height,
    );
    let selections = prepare_background_geometry(
        &row.selections,
        key.row_top,
        key.grid_left,
        key.cell_width,
        key.line_height,
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
    let symbols = prepare_symbol_geometry(&row.symbols, key.row_top, key.grid_left, key.cell_width);

    let mut cursor_text = Vec::new();
    let mut cursor_symbols = PreparedDecorations::default();
    if let Some((position, cell)) = cursor {
        let cursor_left = key.grid_left + key.cell_width * f32::from(position.column);
        let plan = cursor_paint_plan(
            true,
            cursor_style.shape,
            point(cursor_left, key.row_top),
            key.cell_width,
            key.line_height,
            position.width_cells,
        );
        let recolor_text = plan.is_some_and(|plan| plan.recolor_text);
        if recolor_text && !cell.spacer_tail && !cell.invisible {
            if let Some(symbol) = row
                .symbols
                .iter()
                .find(|symbol| symbol.start == usize::from(position.column))
            {
                let mut symbol = symbol.clone();
                symbol.color = cursor_style.text_color;
                cursor_symbols =
                    prepare_symbol_geometry(&[symbol], key.row_top, key.grid_left, key.cell_width);
            } else {
                cursor_text.push(PreparedText {
                    line: window.text_system().shape_line(
                        cell.text.clone().into(),
                        key.font_size,
                        &[TextRun {
                            len: cell.text.len(),
                            font: terminal_cell_font(font_family, cell.bold, cell.italic),
                            color: gpui_color(cursor_style.text_color).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        force_cell_width_for_cell(&cell.text, position.width_cells)
                            .then_some(key.cell_width),
                    ),
                    origin: point(cursor_left, key.row_top),
                    blinking: cell.blinking,
                });
            }
        }
    }

    PreparedRow {
        text,
        symbols,
        backgrounds,
        selections,
        under_text_decorations,
        over_text_decorations,
        cursor_text,
        cursor_symbols,
    }
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
        let rows = Arc::clone(&self.rows);
        let cursor = self.cursor.as_ref();
        let cursor_preparation_style = self.cursor_preparation_style;
        let font_family = self.font_family.clone();
        let (stable_rows, preedit_rows) = self.cache.update(_cx, |cache, _| {
            let stable_rows = cache.prepare_visible_geometry(
                &rows,
                visible_rows,
                PreparedGridLayout {
                    grid_left,
                    grid_top: bounds.top(),
                    font_size: self.font_size,
                    cell_width: self.cell_width,
                    line_height: self.line_height,
                    font_family: &font_family,
                    decoration_metrics,
                },
                cursor,
                cursor_preparation_style,
                window,
            );
            let preedit_rows = cache.prepare_preedit(
                self.preedit.as_ref(),
                visible_rows,
                grid_left,
                bounds.top(),
                &font_family,
                self.font_size,
                self.cell_width,
                self.line_height,
                self.foreground,
                self.background,
                self.cursor_style.color,
                self.scale_factor,
                window,
            );
            (stable_rows, preedit_rows)
        });

        for (row_index, stable) in stable_rows.into_iter().enumerate() {
            let row_top = bounds.top() + self.line_height * row_index as f32;
            let find_backgrounds = prepare_background_geometry(
                &find_background_spans(row_index, &self.find_spans),
                row_top,
                grid_left,
                self.cell_width,
                self.line_height,
            );
            let mut cursor_background = None;
            let mut cursor_overlay_visible = false;
            if preedit_rows
                .as_ref()
                .and_then(|rows| rows.get(row_index))
                .is_none()
                && self.cursor_style.visible
                && let Some((position, _)) = &self.cursor
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
                cursor_background = Some(match plan.paint {
                    CursorPaint::Fill => fill(plan.bounds, gpui_color(self.cursor_style.color)),
                    CursorPaint::Outline => outline(
                        plan.bounds,
                        gpui_color(self.cursor_style.color),
                        BorderStyle::default(),
                    ),
                });
                cursor_overlay_visible = plan.recolor_text;
            }

            let mut frame = PreparedFrameRow::new(stable);
            frame.find_backgrounds = find_backgrounds;
            frame.cursor_background = cursor_background;
            frame.cursor_overlay_visible = cursor_overlay_visible;
            frame.preedit = preedit_rows
                .as_ref()
                .and_then(|rows| rows.get(row_index))
                .cloned();
            prepared_rows.push(frame);
        }

        let grid_bounds = terminal_grid_content_bounds(bounds, self.columns, self.cell_width);
        let grid_bounds = Bounds::new(
            grid_bounds.origin,
            size(
                grid_bounds.size.width,
                (self.line_height * visible_rows as f32).min(grid_bounds.size.height),
            ),
        );
        let candidate = TerminalPaintBatch {
            surface: Some(fill(bounds, gpui_color(self.background))),
            grid_bounds,
            rows: prepared_rows,
            graphics: self.graphics.paint_plan(
                grid_bounds,
                self.cell_width,
                self.line_height,
                self.scale_factor,
            ),
            blink_phase_visible: self.blink_phase_visible,
        };
        let fallback = self.fallback.as_mut().map(|fallback| {
            let mut request_layout = ();
            fallback
                .prepaint(None, None, bounds, &mut request_layout, window, _cx)
                .candidate
        });
        PrepaintState {
            candidate,
            fallback,
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
        let mut failure = prepaint
            .candidate
            .preflight(self.line_height, self.paint_fault.take(), window, cx)
            .err();
        let mut submitted_generation = None;
        if failure.is_none() {
            match prepaint.candidate.submit(
                prepaint.candidate.grid_bounds,
                self.line_height,
                window,
                cx,
            ) {
                Ok(()) => submitted_generation = Some(self.presentation.generation),
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
            && fallback
                .preflight(self.line_height, None, window, cx)
                .is_ok()
            && fallback
                .submit(fallback.grid_bounds, self.line_height, window, cx)
                .is_ok()
        {
            submitted_generation = self.fallback_generation;
        }
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let pane = self.input.clone();
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

#[cfg(test)]
fn presentation_backgrounds(
    row: &RowPaintInput,
    row_index: usize,
    find_spans: &[FindHighlightSpan],
) -> Vec<BackgroundSpan> {
    let mut backgrounds = row.backgrounds.clone();
    backgrounds.extend(find_background_spans(row_index, find_spans));
    backgrounds.extend(row.selections.iter().copied());
    backgrounds
}

fn find_background_spans(
    row_index: usize,
    find_spans: &[FindHighlightSpan],
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
                        ACTIVE_THEME.search_current_match_background
                    } else {
                        ACTIVE_THEME.search_match_background
                    },
                })
        })
        .collect()
}

fn prepare_background_geometry(
    spans: &[BackgroundSpan],
    row_top: Pixels,
    grid_left: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
) -> Vec<PaintQuad> {
    spans
        .iter()
        .map(|span| {
            fill(
                Bounds::new(
                    point(grid_left + cell_width * span.start as f32, row_top),
                    size(cell_width * span.len as f32, line_height),
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
    grid_left: Pixels,
    cell_width: Pixels,
) -> PreparedDecorations {
    let mut prepared = PreparedDecorations::default();
    for symbol in symbols {
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
                } => prepared.quads.push(PreparedQuad {
                    quad: fill(
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
                    ),
                    blinking: symbol.blinking,
                }),
                SymbolPrimitive::Polygon { points, alpha } => {
                    let context = DeviceRasterContext {
                        cell_width: symbol.plan.width_device,
                        cell_height: symbol.plan.height_device,
                        origin,
                        scale,
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
                        cell_width: symbol.plan.width_device,
                        cell_height: symbol.plan.height_device,
                        origin,
                        scale,
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
    use std::cell::Cell;

    use super::*;
    use crate::ui::terminal_ime::layout_preedit;

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

    fn prepared_row_key(cursor: Option<PreparedCursorKey>) -> PreparedRowKey {
        PreparedRowKey {
            grid_left: px(0.0),
            row_top: px(0.0),
            font_size: px(14.0),
            cell_width: px(8.0),
            line_height: px(20.0),
            decoration_metrics: decoration_metrics(px(15.0), px(11.0), px(8.0), 2.0),
            cursor,
        }
    }

    fn prepared_preedit_key(layout: &PreeditLayout, visible_rows: usize) -> PreparedPreeditKey {
        PreparedPreeditKey {
            clusters: Arc::clone(&layout.clusters),
            caret: layout.caret,
            visible_rows,
            grid_left: px(0.0),
            grid_top: px(0.0),
            font_family: "Menlo".into(),
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
    fn find_backgrounds_distinguish_current_match_and_keep_selection_on_top() {
        let mut selected = cell("a");
        selected.selected = true;
        let row = Arc::<[CellSnapshot]>::from([selected]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);
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

        let backgrounds = presentation_backgrounds(&input, 0, &spans);

        assert_eq!(
            backgrounds
                .iter()
                .map(|background| background.color)
                .collect::<Vec<_>>(),
            [
                ACTIVE_THEME.search_match_background,
                ACTIVE_THEME.search_current_match_background,
                ACTIVE_THEME.players[0].selection,
            ]
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
    fn symbol_prepaint_preserves_blink_demand_and_terminal_width() {
        let mut block = cell("█");
        block.blinking = true;
        let mut tail = block.clone();
        tail.text = " ".to_owned();
        tail.spacer_tail = true;
        let row = Arc::<[CellSnapshot]>::from([block, tail]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        let prepared = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.0));

        assert!(prepared.quads[0].blinking);
        assert!(!text_fragment_visible(prepared.quads[0].blinking, false));
        assert_eq!(prepared.quads.len(), 1);
        assert_eq!(
            prepared.quads[0].quad.bounds,
            Bounds::new(point(px(5.0), px(10.0)), size(px(16.0), px(20.0)))
        );
    }

    #[test]
    fn symbol_prepaint_uses_fractional_logical_cell_origins() {
        let row = Arc::<[CellSnapshot]>::from([cell("a"), cell("a"), cell("█")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        let visible = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.25));

        assert_eq!(visible.quads[0].quad.bounds.origin.x, px(21.5));
    }

    #[test]
    fn vector_symbols_prepare_flat_cell_local_quads() {
        let row = Arc::<[CellSnapshot]>::from([cell("\u{e0b0}"), cell("\u{e0b1}")]);
        let input = prepare_row(&row, &colors(), &"Menlo".into(), None);

        let prepared = prepare_symbol_geometry(&input.symbols, px(10.0), px(5.0), px(8.0));

        assert!(prepared.quads.len() > 2);
        assert!(prepared.quads.iter().all(|prepared| {
            prepared.quad.bounds.left() >= px(5.0)
                && prepared.quad.bounds.right() <= px(21.0)
                && prepared.quad.bounds.top() >= px(10.0)
                && prepared.quad.bounds.bottom() <= px(30.0)
        }));
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
            None,
        ));
        let builds = Cell::new(0);
        let mut cached = None;
        let stable = reuse_or_prepare_row(&mut cached, &source, prepared_row_key(None), || {
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
                cursor_text: Vec::new(),
                cursor_symbols: PreparedDecorations::default(),
            }
        });
        let stable_buffer = stable.under_text_decorations.quads.as_ptr();

        for _phase in [false, true] {
            let stable = reuse_or_prepare_row(&mut cached, &source, prepared_row_key(None), || {
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
            None,
        ));
        let second_source = Arc::new(prepare_row(
            &Arc::from([cell("b")]),
            &colors(),
            &"Menlo".into(),
            None,
        ));
        let mut cached = None;
        let first = reuse_or_prepare_row(&mut cached, &first_source, prepared_row_key(None), || ());
        let second =
            reuse_or_prepare_row(&mut cached, &second_source, prepared_row_key(None), || ());

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn shaped_geometry_cache_invalidates_when_layout_geometry_changes() {
        let source = Arc::new(prepare_row(
            &Arc::from([cell("a")]),
            &colors(),
            &"Menlo".into(),
            None,
        ));
        let mut cached = None;
        let first_key = prepared_row_key(None);
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
    fn shaped_geometry_cache_limits_cursor_invalidation_to_the_cursor_row() {
        let source = Arc::new(prepare_row(
            &Arc::from([cell("a")]),
            &colors(),
            &"Menlo".into(),
            None,
        ));
        let cursor = PreparedCursorKey {
            position: CursorPositionSnapshot {
                row: 0,
                column: 0,
                width_cells: 1,
            },
            style: CursorSnapshot {
                visible: true,
                ..CursorSnapshot::default()
            },
        };
        let mut cursor_row = None;
        let mut unchanged_row = None;
        let before = reuse_or_prepare_row(&mut cursor_row, &source, prepared_row_key(None), || ());
        let after = reuse_or_prepare_row(
            &mut cursor_row,
            &source,
            prepared_row_key(Some(cursor)),
            || (),
        );
        let unchanged_before =
            reuse_or_prepare_row(&mut unchanged_row, &source, prepared_row_key(None), || ());
        let unchanged_after =
            reuse_or_prepare_row(&mut unchanged_row, &source, prepared_row_key(None), || {
                panic!("a cursor on another row must not invalidate stable geometry")
            });

        assert_eq!(
            (
                Arc::ptr_eq(&before, &after),
                Arc::ptr_eq(&unchanged_before, &unchanged_after),
            ),
            (false, true)
        );
    }

    #[test]
    fn cursor_blink_phase_reuses_every_prepared_row() {
        let rows = Arc::<[RowSnapshot]>::from([
            Arc::<[CellSnapshot]>::from([cell("a"), cell("b")]),
            Arc::<[CellSnapshot]>::from([cell("c"), cell("d")]),
            Arc::<[CellSnapshot]>::from([cell("e"), cell("f")]),
        ]);
        let cursor = CursorPositionSnapshot {
            row: 1,
            column: 0,
            width_cells: 1,
        };
        let mut cache = TerminalGridCache::new();
        let first = cache.prepare(
            &rows,
            &colors(),
            &"Menlo".into(),
            Some(cursor),
            grid_metrics(),
        );

        let negotiated = CursorSnapshot {
            position: Some(cursor),
            visible: true,
            blinking: true,
            ..CursorSnapshot::default()
        };
        assert!(presented_cursor_style(negotiated, true, true).visible);
        assert!(!presented_cursor_style(negotiated, true, false).visible);

        let second = cache.prepare(
            &rows,
            &colors(),
            &"Menlo".into(),
            Some(cursor),
            grid_metrics(),
        );
        assert!(
            first
                .iter()
                .zip(second.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
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
