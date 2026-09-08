use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyView, App, AppContext, Context, Entity, IntoElement, Render, Styled, Window, canvas, div,
};

use super::{
    TerminalGridCache, TerminalGridConfiguration, TerminalGridElement, TerminalPaintBatch,
};
use crate::terminal::ScreenSnapshot;

/// Retains GPUI's grid scene between cursor phases. The cursor row is composited
/// over that scene so backgrounds, selection, symbols, and decorations keep their
/// normal paint order even for a block cursor over a wide or decorated cell.
pub(crate) struct TerminalGridPresentation {
    grid: Option<Entity<GridView>>,
    cursor: CursorLayer,
}

#[derive(Clone, Default)]
pub(super) struct CursorLayer {
    batch: Rc<RefCell<Option<Rc<TerminalPaintBatch>>>>,
    #[cfg(test)]
    counts: Rc<std::cell::Cell<(usize, usize)>>,
}

impl CursorLayer {
    pub(super) fn clear(&self) {
        self.set(None);
    }

    pub(super) fn set(&self, batch: Option<Rc<TerminalPaintBatch>>) {
        *self.batch.borrow_mut() = batch;
    }

    #[cfg(test)]
    pub(super) fn record_grid_paint(&self) {
        let (grid, cursor) = self.counts.get();
        self.counts.set((grid + 1, cursor));
    }
}

struct GridView(TerminalGridElement);

impl Render for GridView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
    }
}

impl TerminalGridPresentation {
    pub(crate) fn new() -> Self {
        Self {
            grid: None,
            cursor: CursorLayer::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn paint_counts(&self) -> (usize, usize) {
        self.cursor.counts.get()
    }

    #[cfg(test)]
    pub(crate) fn cursor_storage(&self) -> Option<(usize, usize)> {
        self.cursor
            .batch
            .borrow()
            .as_ref()
            .map(|batch| (Rc::as_ptr(batch) as usize, batch.rows.len()))
    }

    pub(crate) fn render(
        &mut self,
        screen: &Arc<ScreenSnapshot>,
        cache: Entity<TerminalGridCache>,
        configuration: TerminalGridConfiguration,
        cx: &mut App,
    ) -> impl IntoElement + use<> {
        let eligible = configuration.terminal_input_focused
            && screen.cursor.visible
            && screen.cursor.blinking
            && screen.cursor.position.is_some()
            && !screen.text_blinking
            && screen.graphics.placements.is_empty()
            && configuration.preedit.is_none();
        let presentation_unchanged = eligible
            && configuration.presentation_operation.is_none()
            && configuration.graphics_attempt.is_none()
            && configuration.paint_fault.is_none()
            && self.cursor.batch.borrow().is_some()
            && self.grid.as_ref().is_some_and(|grid| {
                let previous = &grid.read(cx).0;
                previous.cursor_layer.is_some()
                    && Arc::ptr_eq(&previous.presentation, screen)
                    && previous.cache == cache
                    && previous.font_family == configuration.font_family
                    && previous.font_size == configuration.font_size
                    && previous.line_height == configuration.line_height
                    && previous.cell_width == configuration.cell_width
                    && previous.scale_factor == configuration.scale_factor
                    && previous.find_spans == configuration.find_spans
                    && previous.active_hyperlink == configuration.active_hyperlink
            });
        let phase = configuration.blink_phase_visible;
        let line_height = configuration.line_height;
        if !eligible {
            self.cursor.clear();
            self.grid = None;
            return TerminalGridElement::new(screen, cache, configuration, cx).into_any_element();
        }
        if !presentation_unchanged {
            self.cursor.clear();
            let mut element = TerminalGridElement::new(screen, cache, configuration, cx);
            element.cursor_style = element.cursor_preparation_style;
            element.blink_phase_visible = true;
            element.cursor_layer = Some(self.cursor.clone());
            // A new view identity invalidates the scene in this frame. A notify
            // issued during Render would only dirty the following frame.
            self.grid = Some(cx.new(|_| GridView(element)));
        }
        let cursor = self.cursor.clone();
        let grid = self.grid.as_ref().map(|grid| {
            AnyView::from(grid.clone()).cached(gpui::StyleRefinement {
                size: gpui::SizeRefinement {
                    width: Some(gpui::relative(1.0).into()),
                    height: Some(gpui::relative(1.0).into()),
                },
                ..Default::default()
            })
        });
        div()
            .relative()
            .size_full()
            .children(grid)
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        if !phase {
                            return;
                        }
                        let batch = cursor.batch.borrow().clone();
                        if let Some(batch) = batch
                            && batch.preflight(line_height, None, window, cx).is_ok()
                        {
                            #[cfg(test)]
                            {
                                let (grid, draws) = cursor.counts.get();
                                cursor.counts.set((grid, draws + 1));
                            }
                            let _ = batch.submit(batch.grid_bounds, line_height, window, cx);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}
