use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, DispatchPhase, Empty, EventEmitter, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, Task, Window, canvas, div, px,
    rgba,
};

use crate::theme::ACTIVE_THEME;

const THUMB_WIDTH: f32 = 5.0;
const HITBOX_HORIZONTAL_PADDING: f32 = 4.0;
const HITBOX_WIDTH: f32 = THUMB_WIDTH + HITBOX_HORIZONTAL_PADDING * 2.0;
const THUMB_RIGHT_INSET: f32 = 4.0;
const MINIMUM_THUMB_HEIGHT: f32 = 24.0;
const OVERLAY_SCROLLBAR_HIDE_DELAY: Duration = Duration::from_secs(2);

pub(super) trait ScrollOffset: Copy + std::fmt::Debug + PartialEq + 'static {
    fn as_f64(self) -> f64;
    fn for_progress(maximum: Self, progress: f64) -> Self;
}

impl ScrollOffset for u64 {
    fn as_f64(self) -> f64 {
        self as f64
    }

    fn for_progress(maximum: Self, progress: f64) -> Self {
        if progress <= 0.0 {
            0
        } else if progress >= 1.0 {
            maximum
        } else {
            (progress * maximum as f64).round() as u64
        }
    }
}

impl ScrollOffset for f32 {
    fn as_f64(self) -> f64 {
        f64::from(self)
    }

    fn for_progress(maximum: Self, progress: f64) -> Self {
        (progress as f32 * maximum).clamp(0.0, maximum)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum OverlayScrollbarEvent<O> {
    InteractionStarted,
    OffsetRequested(O),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ScrollMetrics<O> {
    track_top_px: f32,
    track_height_px: f32,
    viewport_fraction: f64,
    maximum_offset: O,
    current_offset: O,
}

impl ScrollMetrics<u64> {
    pub(super) fn for_rows(
        track_top_px: f32,
        track_height_px: f32,
        total_rows: u64,
        visible_rows: u64,
        offset_rows: u64,
    ) -> Option<Self> {
        if !valid_track(track_top_px, track_height_px)
            || visible_rows == 0
            || total_rows <= visible_rows
        {
            return None;
        }

        let maximum_offset = total_rows.saturating_sub(visible_rows);
        Some(Self {
            track_top_px,
            track_height_px,
            viewport_fraction: visible_rows as f64 / total_rows as f64,
            maximum_offset,
            current_offset: offset_rows.min(maximum_offset),
        })
    }
}

impl ScrollMetrics<f32> {
    pub(super) fn for_pixels(
        track_top_px: f32,
        track_height_px: f32,
        maximum_offset_px: f32,
        offset_px: f32,
    ) -> Option<Self> {
        if !valid_track(track_top_px, track_height_px)
            || !maximum_offset_px.is_finite()
            || maximum_offset_px <= 0.0
            || !offset_px.is_finite()
        {
            return None;
        }

        let track_height = f64::from(track_height_px);
        let maximum_offset = f64::from(maximum_offset_px);
        Some(Self {
            track_top_px,
            track_height_px,
            viewport_fraction: track_height / (track_height + maximum_offset),
            maximum_offset: maximum_offset_px,
            current_offset: offset_px.clamp(0.0, maximum_offset_px),
        })
    }
}

impl<O: ScrollOffset> ScrollMetrics<O> {
    fn with_offset(self, offset: O) -> Self {
        Self {
            current_offset: offset,
            ..self
        }
    }

    fn offset_for_progress(self, progress: f64) -> Option<O> {
        if !progress.is_finite() {
            return None;
        }
        let progress = progress.clamp(0.0, 1.0);
        Some(O::for_progress(self.maximum_offset, progress))
    }

    fn same_scroll_range(self, other: Self) -> bool {
        self.track_top_px == other.track_top_px
            && self.track_height_px == other.track_height_px
            && self.viewport_fraction == other.viewport_fraction
            && self.maximum_offset == other.maximum_offset
    }
}

fn valid_track(track_top_px: f32, track_height_px: f32) -> bool {
    track_top_px.is_finite() && track_height_px.is_finite() && track_height_px > 0.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThumbGeometry {
    top_px: f32,
    height_px: f32,
    track_top_px: f32,
    track_height_px: f32,
}

impl ThumbGeometry {
    fn for_metrics<O: ScrollOffset>(metrics: ScrollMetrics<O>) -> Self {
        let track_height = f64::from(metrics.track_height_px);
        let minimum_height = f64::from(MINIMUM_THUMB_HEIGHT.min(metrics.track_height_px));
        let height = (metrics.viewport_fraction * track_height).clamp(minimum_height, track_height);
        let maximum_offset = metrics.maximum_offset.as_f64();
        let progress = if maximum_offset > 0.0 {
            metrics.current_offset.as_f64() / maximum_offset
        } else {
            0.0
        }
        .clamp(0.0, 1.0);

        Self {
            top_px: ((track_height - height) * progress) as f32,
            height_px: height as f32,
            track_top_px: metrics.track_top_px,
            track_height_px: metrics.track_height_px,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScrollbarDrag<O> {
    grab_fraction: f64,
    track_top_px: f32,
    track_height_px: f32,
    target_offset: O,
    offset_valid: bool,
    reset_on_finish: bool,
}

pub(super) struct OverlayScrollbar<O: ScrollOffset> {
    name: &'static str,
    metrics: Option<ScrollMetrics<O>>,
    visible: bool,
    hovered: bool,
    drag: Option<ScrollbarDrag<O>>,
    visibility_generation: u64,
    _hide_task: Option<Task<()>>,
}

impl<O: ScrollOffset> OverlayScrollbar<O> {
    pub(super) fn new(name: &'static str) -> Self {
        Self {
            name,
            metrics: None,
            visible: false,
            hovered: false,
            drag: None,
            visibility_generation: 0,
            _hide_task: None,
        }
    }

    pub(super) fn sync(&mut self, metrics: Option<ScrollMetrics<O>>, cx: &mut Context<Self>) {
        self.set_metrics(metrics, cx);
    }

    pub(super) fn reveal(&mut self, metrics: Option<ScrollMetrics<O>>, cx: &mut Context<Self>) {
        self.set_metrics(metrics, cx);
        self.reveal_current(cx);
    }

    fn set_metrics(&mut self, metrics: Option<ScrollMetrics<O>>, cx: &mut Context<Self>) {
        let Some(metrics) = metrics else {
            if let Some(drag) = &mut self.drag {
                drag.offset_valid = false;
                drag.reset_on_finish = true;
                self.invalidate_hide();
                cx.notify();
                return;
            }
            self.reset(cx);
            return;
        };

        let drag_invalidated = self.drag.is_some()
            && self
                .metrics
                .is_some_and(|previous| !previous.same_scroll_range(metrics));
        if let Some(drag) = &mut self.drag {
            drag.reset_on_finish = false;
        }
        if drag_invalidated {
            if let Some(drag) = &mut self.drag {
                drag.offset_valid = false;
                drag.reset_on_finish = false;
            }
            self.invalidate_hide();
        }
        if self.metrics != Some(metrics) {
            self.metrics = Some(metrics);
            if self.visible {
                cx.notify();
            }
        }
    }

    fn reveal_current(&mut self, cx: &mut Context<Self>) {
        if self.metrics.is_none() {
            self.reset(cx);
            return;
        }

        self.visible = true;
        self.invalidate_hide();
        self.schedule_hide(cx);
        cx.notify();
    }

    pub(super) fn reset(&mut self, cx: &mut Context<Self>) {
        let had_metrics = self.metrics.take().is_some();
        let had_drag = self.drag.take().is_some();
        let changed = had_metrics || self.visible || self.hovered || had_drag;
        self.visible = false;
        self.hovered = false;
        self.invalidate_hide();
        if changed {
            cx.notify();
        }
    }

    fn invalidate_hide(&mut self) {
        self.visibility_generation = self.visibility_generation.wrapping_add(1);
        self._hide_task.take();
    }

    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        if self.drag.is_some() || self.hovered {
            return;
        }

        let generation = self.visibility_generation;
        self._hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(OVERLAY_SCROLLBAR_HIDE_DELAY)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.visibility_generation == generation && this.drag.is_none() && !this.hovered
                {
                    this.visible = false;
                    cx.notify();
                }
            });
        }));
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.hovered == hovered {
            return;
        }

        self.hovered = hovered;
        self.invalidate_hide();
        if hovered {
            self.visible = true;
            cx.notify();
        } else {
            self.reveal_current(cx);
        }
    }

    fn begin_drag(
        &mut self,
        pointer_y: Pixels,
        thumb_bounds: gpui::Bounds<Pixels>,
        geometry: ThumbGeometry,
        cx: &mut Context<Self>,
    ) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let pointer_y = f32::from(pointer_y);
        let thumb_top = f32::from(thumb_bounds.origin.y);
        if !pointer_y.is_finite()
            || !thumb_top.is_finite()
            || !geometry.height_px.is_finite()
            || geometry.height_px <= 0.0
        {
            return;
        }
        self.invalidate_hide();
        self.visible = true;
        self.drag = Some(ScrollbarDrag {
            grab_fraction: (f64::from(pointer_y - thumb_top) / f64::from(geometry.height_px))
                .clamp(0.0, 1.0),
            track_top_px: thumb_top - geometry.top_px,
            track_height_px: geometry.track_height_px,
            target_offset: metrics.current_offset,
            offset_valid: true,
            reset_on_finish: false,
        });
        cx.emit(OverlayScrollbarEvent::InteractionStarted);
        cx.notify();
    }

    fn move_drag(&mut self, pointer_y: Pixels, cx: &mut Context<Self>) -> bool {
        let (Some(metrics), Some(drag)) = (self.metrics, self.drag) else {
            return false;
        };
        if !drag.offset_valid {
            return true;
        }
        let geometry = ThumbGeometry::for_metrics(metrics.with_offset(drag.target_offset));
        let movable_height = (drag.track_height_px - geometry.height_px).max(0.0);
        if movable_height <= f32::EPSILON {
            return true;
        }

        let pointer_y = f32::from(pointer_y);
        if !pointer_y.is_finite() {
            return true;
        }
        let pointer_in_track = pointer_y - drag.track_top_px;
        let thumb_top = (f64::from(pointer_in_track)
            - drag.grab_fraction * f64::from(geometry.height_px))
        .clamp(0.0, f64::from(movable_height));
        let Some(offset) = metrics.offset_for_progress(thumb_top / f64::from(movable_height))
        else {
            return true;
        };
        if offset == drag.target_offset {
            return true;
        }

        if let Some(drag) = &mut self.drag {
            drag.target_offset = offset;
        }
        cx.emit(OverlayScrollbarEvent::OffsetRequested(offset));
        cx.notify();
        true
    }

    fn finish_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if drag.reset_on_finish {
            self.reset(cx);
        } else {
            self.reveal_current(cx);
        }
        true
    }

    fn geometry(&self) -> Option<ThumbGeometry> {
        if !self.visible {
            return None;
        }
        let metrics = self.metrics?;
        Some(ThumbGeometry::for_metrics(match self.drag {
            Some(drag) if drag.offset_valid => metrics.with_offset(drag.target_offset),
            None => metrics,
            Some(_) => metrics,
        }))
    }

    fn render_thumb(&self, geometry: ThumbGeometry, cx: &mut Context<Self>) -> AnyElement {
        let scrollbar = cx.entity().downgrade();
        let hover_scrollbar = scrollbar.clone();
        let down_scrollbar = scrollbar.clone();
        let move_scrollbar = scrollbar.clone();
        let up_scrollbar = scrollbar;
        let thumb_id: SharedString = format!("{}-thumb", self.name).into();
        let hitbox_id: SharedString = format!("{}-thumb-hitbox", self.name).into();
        let dragging = self.drag.is_some();
        let thumb_color = if dragging {
            ACTIVE_THEME.icon
        } else {
            ACTIVE_THEME.scrollbar_thumb_background
        };
        let hover_color = if dragging {
            ACTIVE_THEME.icon
        } else {
            ACTIVE_THEME.icon_accent
        };
        let group = thumb_id.clone();
        let hover_group = group.clone();
        let thumb_debug = thumb_id.clone();
        let hitbox_debug = hitbox_id.clone();

        div()
            .group(group)
            .id(hitbox_id)
            .debug_selector(move || hitbox_debug.to_string())
            .absolute()
            .top(px(geometry.track_top_px + geometry.top_px))
            .right_0()
            .w(px(HITBOX_WIDTH))
            .h(px(geometry.height_px))
            .block_mouse_except_scroll()
            .cursor_default()
            .on_hover(move |hovered, _, cx| {
                let _ = hover_scrollbar.update(cx, |scrollbar, cx| {
                    scrollbar.set_hovered(*hovered, cx);
                });
            })
            .child(
                div()
                    .id(thumb_id)
                    .debug_selector(move || thumb_debug.to_string())
                    .absolute()
                    .right(px(THUMB_RIGHT_INSET))
                    .w(px(THUMB_WIDTH))
                    .h_full()
                    .rounded(px(THUMB_WIDTH / 2.0))
                    .bg(rgba(thumb_color.rgba_hex()))
                    .group_hover(hover_group, move |thumb| {
                        thumb.bg(rgba(hover_color.rgba_hex()))
                    }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |thumb_bounds, _, window, _| {
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }
                            let _ = down_scrollbar.update(cx, |scrollbar, cx| {
                                scrollbar.begin_drag(event.position.y, thumb_bounds, geometry, cx);
                            });
                            cx.stop_propagation();
                        });

                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble || !event.dragging() {
                                return;
                            }
                            let handled = move_scrollbar
                                .update(cx, |scrollbar, cx| {
                                    scrollbar.move_drag(event.position.y, cx)
                                })
                                .unwrap_or(false);
                            if handled {
                                cx.stop_propagation();
                            }
                        });

                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                                return;
                            }
                            let handled = up_scrollbar
                                .update(cx, |scrollbar, cx| scrollbar.finish_drag(cx))
                                .unwrap_or(false);
                            if handled {
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

impl<O: ScrollOffset> EventEmitter<OverlayScrollbarEvent<O>> for OverlayScrollbar<O> {}

impl<O: ScrollOffset> Render for OverlayScrollbar<O> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.geometry() {
            Some(geometry) => self.render_thumb(geometry, cx),
            None => Empty.into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[test]
    fn row_metrics_should_map_top_middle_and_bottom_geometry() {
        let top = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0).unwrap();
        let middle = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 40).unwrap();
        let bottom = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 80).unwrap();

        assert_eq!(
            (
                ThumbGeometry::for_metrics(top),
                ThumbGeometry::for_metrics(middle),
                ThumbGeometry::for_metrics(bottom),
            ),
            (
                ThumbGeometry {
                    top_px: 0.0,
                    height_px: 40.0,
                    track_top_px: 0.0,
                    track_height_px: 200.0,
                },
                ThumbGeometry {
                    top_px: 80.0,
                    height_px: 40.0,
                    track_top_px: 0.0,
                    track_height_px: 200.0,
                },
                ThumbGeometry {
                    top_px: 160.0,
                    height_px: 40.0,
                    track_top_px: 0.0,
                    track_height_px: 200.0,
                },
            )
        );
    }

    #[test]
    fn pixel_metrics_should_enforce_the_minimum_thumb_height() {
        let metrics = ScrollMetrics::for_pixels(0.0, 200.0, 9_800.0, 4_000.0).unwrap();
        let geometry = ThumbGeometry::for_metrics(metrics);

        assert_eq!(geometry.height_px, MINIMUM_THUMB_HEIGHT);
        assert!(geometry.top_px > 0.0);
    }

    #[test]
    fn metrics_should_reject_invalid_and_unscrollable_ranges() {
        assert!(ScrollMetrics::for_rows(0.0, 200.0, 20, 20, 0).is_none());
        assert!(ScrollMetrics::for_rows(0.0, f32::NAN, 100, 20, 0).is_none());
        assert!(ScrollMetrics::for_pixels(0.0, 200.0, 0.0, 0.0).is_none());
        assert!(ScrollMetrics::for_pixels(0.0, 200.0, f32::INFINITY, 0.0).is_none());
    }

    #[test]
    fn row_progress_should_reach_exact_u64_endpoints() {
        let metrics = ScrollMetrics::for_rows(0.0, 200.0, u64::MAX, 1, 0).unwrap();

        assert_eq!(
            (
                metrics.offset_for_progress(0.0),
                metrics.offset_for_progress(1.0),
            ),
            (Some(0), Some(u64::MAX - 1))
        );
    }

    #[test]
    fn row_progress_should_remain_monotonic_at_u64_scale() {
        let metrics = ScrollMetrics::for_rows(0.0, 200.0, u64::MAX, 1, 0).unwrap();
        let middle = metrics.offset_for_progress(0.5).unwrap();
        let after_middle = metrics.offset_for_progress(0.500_000_000_1).unwrap();

        assert!(middle < after_middle && after_middle < u64::MAX - 1);
    }

    #[test]
    fn pixel_progress_should_map_to_exact_endpoints() {
        let metrics = ScrollMetrics::for_pixels(0.0, 200.0, 800.0, 0.0).unwrap();

        assert_eq!(
            (
                metrics.offset_for_progress(0.0),
                metrics.offset_for_progress(1.0),
            ),
            (Some(0.0), Some(800.0))
        );
        assert!(metrics.offset_for_progress(f64::NAN).is_none());
    }

    #[gpui::test]
    fn reset_should_clear_an_active_drag_when_metrics_exist(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let metrics = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0);
            scrollbar.reveal(metrics, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );
            scrollbar.begin_drag(px(10.0), thumb_bounds, geometry, cx);
            assert!(scrollbar.drag.is_some());

            scrollbar.reset(cx);

            assert!(scrollbar.metrics.is_none());
            assert!(scrollbar.drag.is_none());
            assert!(!scrollbar.visible && !scrollbar.hovered);
        });
    }

    #[gpui::test]
    fn changed_scroll_range_should_cancel_an_active_drag(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let initial = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0);
            scrollbar.reveal(initial, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );
            scrollbar.begin_drag(px(10.0), thumb_bounds, geometry, cx);

            scrollbar.sync(ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 10), cx);
            assert!(
                scrollbar.drag.is_some(),
                "offset updates must preserve the drag"
            );

            scrollbar.sync(ScrollMetrics::for_rows(0.0, 240.0, 100, 20, 10), cx);
            assert!(scrollbar.drag.is_some_and(|drag| !drag.offset_valid));
            assert!(scrollbar.move_drag(px(120.0), cx));
            assert!(scrollbar.finish_drag(cx));
            assert!(scrollbar.drag.is_none());
        });
    }

    #[gpui::test]
    fn unscrollable_metrics_should_preserve_capture_until_mouse_up(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let metrics = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0);
            scrollbar.reveal(metrics, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );
            scrollbar.begin_drag(px(10.0), thumb_bounds, geometry, cx);

            scrollbar.sync(None, cx);

            assert!(scrollbar.metrics.is_some());
            assert!(
                scrollbar
                    .drag
                    .is_some_and(|drag| { !drag.offset_valid && drag.reset_on_finish })
            );
            assert!(scrollbar.move_drag(px(120.0), cx));
            assert!(scrollbar.finish_drag(cx));
            assert!(scrollbar.metrics.is_none());
            assert!(scrollbar.geometry().is_none());
        });
    }

    #[gpui::test]
    fn restored_metrics_should_survive_the_end_of_an_invalidated_drag(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let metrics = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0);
            scrollbar.reveal(metrics, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );
            scrollbar.begin_drag(px(10.0), thumb_bounds, geometry, cx);

            scrollbar.sync(None, cx);
            scrollbar.sync(metrics, cx);

            assert!(
                scrollbar
                    .drag
                    .is_some_and(|drag| { !drag.offset_valid && !drag.reset_on_finish })
            );
            assert!(scrollbar.finish_drag(cx));
            assert_eq!(scrollbar.metrics, metrics);
            assert!(scrollbar.visible);
        });
    }

    #[gpui::test]
    fn invalid_pointer_coordinates_should_not_start_or_move_a_drag(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let metrics = ScrollMetrics::for_rows(0.0, 200.0, 100, 20, 0);
            scrollbar.reveal(metrics, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );

            scrollbar.begin_drag(px(f32::NAN), thumb_bounds, geometry, cx);
            assert!(scrollbar.drag.is_none());

            scrollbar.begin_drag(px(10.0), thumb_bounds, geometry, cx);
            let target = scrollbar.drag.unwrap().target_offset;
            assert!(scrollbar.move_drag(px(f32::NAN), cx));
            assert_eq!(scrollbar.drag.unwrap().target_offset, target);
        });
    }

    #[gpui::test]
    fn a_track_smaller_than_the_minimum_thumb_should_keep_its_offset(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("test-scrollbar"));
        scrollbar.update(cx, |scrollbar, cx| {
            let metrics = ScrollMetrics::for_rows(0.0, 10.0, 100, 20, 40);
            scrollbar.reveal(metrics, cx);
            let geometry = scrollbar.geometry().unwrap();
            let thumb_bounds = gpui::bounds(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(HITBOX_WIDTH), px(geometry.height_px)),
            );
            scrollbar.begin_drag(px(5.0), thumb_bounds, geometry, cx);
            let target = scrollbar.drag.unwrap().target_offset;

            assert!(scrollbar.move_drag(px(9.0), cx));
            assert_eq!(scrollbar.drag.unwrap().target_offset, target);
        });
    }

    #[gpui::test]
    fn stale_hide_should_not_hide_a_newly_revealed_scrollbar(cx: &mut TestAppContext) {
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("test-scrollbar"));
        let metrics = ScrollMetrics::for_pixels(0.0, 200.0, 800.0, 0.0);
        scrollbar.update(cx, |scrollbar, cx| {
            scrollbar.reveal(metrics, cx);
        });
        cx.executor()
            .advance_clock(OVERLAY_SCROLLBAR_HIDE_DELAY / 2);
        scrollbar.update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
        cx.executor()
            .advance_clock(OVERLAY_SCROLLBAR_HIDE_DELAY / 2 + Duration::from_millis(1));
        cx.run_until_parked();

        assert!(scrollbar.read_with(cx, |scrollbar, _| scrollbar.visible));
    }

    #[gpui::test]
    fn hover_should_retain_visibility_until_the_pointer_leaves(cx: &mut TestAppContext) {
        let (scrollbar, cx) =
            cx.add_window_view(|_, _| OverlayScrollbar::<f32>::new("test-scrollbar"));
        let metrics = ScrollMetrics::for_pixels(0.0, 200.0, 800.0, 0.0);
        scrollbar.update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
        cx.run_until_parked();

        let thumb = cx
            .debug_bounds("test-scrollbar-thumb-hitbox")
            .expect("the scrollbar hitbox was not rendered");
        cx.simulate_mouse_move(thumb.center(), None, gpui::Modifiers::none());
        cx.executor()
            .advance_clock(OVERLAY_SCROLLBAR_HIDE_DELAY + Duration::from_millis(1));
        cx.run_until_parked();
        assert!(scrollbar.read_with(cx, |scrollbar, _| {
            scrollbar.visible && scrollbar.hovered
        }));

        cx.simulate_mouse_move(
            gpui::point(thumb.origin.x - px(20.0), thumb.center().y),
            None,
            gpui::Modifiers::none(),
        );
        cx.executor()
            .advance_clock(OVERLAY_SCROLLBAR_HIDE_DELAY + Duration::from_millis(1));
        cx.run_until_parked();
        assert!(scrollbar.read_with(cx, |scrollbar, _| {
            !scrollbar.visible && !scrollbar.hovered
        }));
    }
}
