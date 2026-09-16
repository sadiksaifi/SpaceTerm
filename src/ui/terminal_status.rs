//! The status marks a Pane Caption and a Tab item present beside a Terminal's title.
//!
//! Both surfaces describe the same Terminal Session, so the attention cue and the OSC 9;4 progress
//! status are drawn here once. Each mark is typed from sanitized Terminal Metadata and never from
//! the title text, which stays opaque: a loader a program draws in its own cells or title is not
//! something host chrome can see or restate.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    LayoutId, PathBuilder, Pixels, Point, Rgba, Task, Window, canvas, div, point, px,
};
use spaceterm_ui::{Icon, IconName};

use crate::terminal::metadata::{MetadataFreshness, ProgressMetadata, TerminalMetadataSnapshot};

/// How many positions one breath of the attention cue steps through.
const BREATH_STEPS: u32 = 24;
/// How long the attention cue rests on each position, for a breath of about 1.7 seconds.
const BREATH_STEP: Duration = Duration::from_millis(70);
/// How many breaths the attention cue takes before it rests in the warning color.
///
/// The breathing draws the eye when attention arrives. Resting afterwards keeps an unread Tab in
/// the background from repainting its window for as long as it stays unread.
const BREATHS: u32 = 6;

/// How far the ring's stroke sits inside the indicator's square, as a share of its size.
const PROGRESS_STROKE_SHARE: f32 = 0.14;
/// The resting ring behind a reported percentage, as a share of the foreground's opacity.
const PROGRESS_TRACK_OPACITY: f32 = 0.28;
/// The loader's arc length, in degrees.
const LOADER_SWEEP_DEGREES: f32 = 100.0;
/// How many positions the loader steps through per revolution.
const LOADER_STEPS: u32 = 12;
/// How long the loader rests on each position.
///
/// Stepping keeps an indeterminate Session from repainting its window at the display's full rate
/// for as long as the program leaves the status up.
const LOADER_STEP: Duration = Duration::from_millis(80);

/// The OSC 9;4 status a Terminal Session last reported, as host chrome presents it.
///
/// A Session whose metadata has gone stale reports nothing, so an exited program never leaves a
/// loader behind in its Pane or Tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalProgress {
    #[default]
    None,
    /// Work with a reported completion percentage, clamped to 0 through 100.
    Normal(u8),
    /// Work in progress whose completion is unknown.
    Indeterminate,
    /// Work that reported a failure.
    Error,
    /// Work that reported it is paused.
    Paused,
}

impl TerminalProgress {
    pub(crate) fn from_metadata(metadata: &TerminalMetadataSnapshot) -> Self {
        if metadata.freshness != MetadataFreshness::Live {
            return Self::None;
        }
        match metadata.progress {
            ProgressMetadata::None => Self::None,
            ProgressMetadata::Normal(percent) => Self::Normal(percent.min(100)),
            ProgressMetadata::Indeterminate => Self::Indeterminate,
            ProgressMetadata::Error(_) => Self::Error,
            ProgressMetadata::Paused(_) => Self::Paused,
        }
    }

    /// The state's stable name, used to identify the rendered mark.
    const fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Normal(_) => Some("normal"),
            Self::Indeterminate => Some("indeterminate"),
            Self::Error => Some("error"),
            Self::Paused => Some("paused"),
        }
    }
}

/// A Terminal glyph that breathes into `attention_color` while its Session asks for attention.
///
/// The glyph at rest inherits the surrounding text color, so it keeps following the host's active,
/// inactive, and hovered paints. Attention layers the same glyph in `attention_color` over it and
/// fades that layer in and out, then leaves it fully shown. `selector` names the attention layer.
pub(crate) fn attention_glyph(
    icon: IconName,
    size: Pixels,
    attention: Option<(ElementId, String, Rgba)>,
) -> AnyElement {
    let glyph = div()
        .relative()
        .size(size)
        .flex_shrink_0()
        .child(Icon::inherited(icon, size));
    let Some((id, selector, color)) = attention else {
        return glyph.into_any_element();
    };
    glyph
        .child(Stepped::new(
            id,
            BREATH_STEP,
            // The last breath stops at its peak, so settling never jumps.
            Some(BREATH_STEPS * (BREATHS - 1) + BREATH_STEPS / 2 + 1),
            move |step| {
                // Each breath rises from nothing to full and back, ending on full once settled.
                let opacity = step.map_or(1.0, |step| {
                    let phase = (step % BREATH_STEPS) as f32 / BREATH_STEPS as f32;
                    (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0
                });
                div()
                    .debug_selector(move || selector)
                    .absolute()
                    .inset_0()
                    .opacity(opacity)
                    .child(Icon::new(icon, size, color))
                    .into_any_element()
            },
        ))
        .into_any_element()
}

/// Draws one progress status in a square of `size`, or nothing when no progress is reported.
///
/// Normal and indeterminate progress take the surrounding text color, so they follow the host's
/// active, inactive, and hovered paints. Error takes `error_color`, which the host resolves for
/// its own surfaces. `selector_prefix` names the mark as `{prefix}-{state}`, and `id` keys the
/// loader's clock so it survives across frames.
pub(crate) fn progress_indicator(
    progress: TerminalProgress,
    id: ElementId,
    selector_prefix: &str,
    size: Pixels,
    error_color: Rgba,
) -> Option<AnyElement> {
    let name = progress.name()?;
    let selector = format!("{selector_prefix}-{name}");
    let mark = match progress {
        TerminalProgress::None => return None,
        TerminalProgress::Normal(percent) => progress_ring(percent, size).into_any_element(),
        TerminalProgress::Indeterminate => loader(id, size).into_any_element(),
        TerminalProgress::Error => {
            Icon::new(IconName::CircleX, size, error_color).into_any_element()
        }
        TerminalProgress::Paused => Icon::inherited(IconName::CirclePause, size).into_any_element(),
    };
    Some(
        div()
            .debug_selector(move || selector)
            .size(size)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(mark)
            .into_any_element(),
    )
}

fn progress_ring(percent: u8, size: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let color = window.text_style().color;
            let track = Hsla {
                a: color.a * PROGRESS_TRACK_OPACITY,
                ..color
            };
            paint_arc(bounds, 0.0, 360.0, track, window);
            paint_arc(bounds, 0.0, f32::from(percent) * 3.6, color, window);
        },
    )
    .size(size)
}

fn loader(id: ElementId, size: Pixels) -> impl IntoElement {
    Stepped::new(id, LOADER_STEP, None, move |step| {
        let start = step.unwrap_or(0) as f32 * 360.0 / LOADER_STEPS as f32;
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let color = window.text_style().color;
                paint_arc(bounds, start, start + LOADER_SWEEP_DEGREES, color, window);
            },
        )
        .size(size)
        .into_any_element()
    })
}

/// Rebuilds its child from a step that advances on a coarse clock while it stays on screen.
///
/// Only the owning view is notified on each step, at the step's pace rather than the display's.
/// A bounded clock stops after `limit` steps and then renders `None`, so a settled animation costs
/// nothing more. The clock restarts when the element leaves the screen and returns.
struct Stepped {
    id: ElementId,
    interval: Duration,
    limit: Option<u32>,
    render: Option<Box<dyn FnOnce(Option<u32>) -> AnyElement>>,
}

impl Stepped {
    fn new(
        id: ElementId,
        interval: Duration,
        limit: Option<u32>,
        render: impl FnOnce(Option<u32>) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id,
            interval,
            limit,
            render: Some(Box::new(render)),
        }
    }
}

/// The step a [`Stepped`] element renders, or `None` once a bounded clock has finished.
struct StepClock {
    step: Option<u32>,
    _tick: Task<()>,
}

impl StepClock {
    fn start(interval: Duration, limit: Option<u32>, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            step: Some(0),
            _tick: cx.spawn(async move |clock, cx| {
                loop {
                    cx.background_executor().timer(interval).await;
                    let running = clock.update(cx, |clock: &mut StepClock, cx| {
                        let next = clock.step.map(|step| step.wrapping_add(1));
                        clock.step = next.filter(|next| limit.is_none_or(|limit| *next < limit));
                        cx.notify();
                        clock.step.is_some()
                    });
                    if !matches!(running, Ok(true)) {
                        break;
                    }
                }
            }),
        }
    }
}

impl IntoElement for Stepped {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Stepped {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let (interval, limit) = (self.interval, self.limit);
        let step = window
            .use_keyed_state("clock", cx, move |_, cx| {
                StepClock::start(interval, limit, cx)
            })
            .read(cx)
            .step;
        let render = self.render.take().expect("a Stepped element lays out once");
        let mut child = render(step);
        (child.request_layout(window, cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

/// Strokes the arc from `start` to `end`, in degrees clockwise from the top of `bounds`.
fn paint_arc(bounds: Bounds<Pixels>, start: f32, end: f32, color: Hsla, window: &mut Window) {
    let sweep = (end - start).clamp(0.0, 360.0);
    if sweep <= 0.0 || color.a <= 0.0 {
        return;
    }
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let stroke = side * PROGRESS_STROKE_SHARE;
    let radius = (side - stroke) / 2.0;
    let center = bounds.center();
    let at = |degrees: f32| -> Point<Pixels> {
        let radians = degrees.to_radians();
        point(
            center.x + px(radius * radians.sin()),
            center.y - px(radius * radians.cos()),
        )
    };
    let radii = point(px(radius), px(radius));
    let mut path = PathBuilder::stroke(px(stroke));
    path.move_to(at(start));
    // An arc cannot end where it starts, so a full ring is drawn as two halves.
    if sweep >= 360.0 {
        path.arc_to(radii, px(0.0), false, true, at(start + 180.0));
        path.arc_to(radii, px(0.0), false, true, at(start));
    } else {
        path.arc_to(radii, px(0.0), sweep > 180.0, true, at(start + sweep));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::terminal::metadata::MetadataTracker;

    fn metadata(
        progress: ProgressMetadata,
        freshness: MetadataFreshness,
    ) -> TerminalMetadataSnapshot {
        let tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/",
            "zsh",
            Default::default(),
            std::time::Instant::now(),
        );
        let mut snapshot = Arc::unwrap_or_clone(tracker.snapshot());
        snapshot.progress = progress;
        snapshot.freshness = freshness;
        snapshot
    }

    /// A bounded animation settles and stops ticking, so a settled cue costs no further frames.
    #[gpui::test]
    fn bounded_step_clock_should_settle_and_stop(cx: &mut gpui::TestAppContext) {
        let interval = Duration::from_millis(10);
        let clock = cx.new(|cx| StepClock::start(interval, Some(3), cx));
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let _observation = cx.update(|cx| {
            let notifications = std::rc::Rc::clone(&notifications);
            cx.observe(&clock, move |_, _| {
                notifications.set(notifications.get() + 1)
            })
        });
        let mut steps = vec![clock.read_with(cx, |clock, _| clock.step)];
        for _ in 0..6 {
            cx.executor().advance_clock(interval);
            cx.run_until_parked();
            steps.push(clock.read_with(cx, |clock, _| clock.step));
        }
        assert_eq!(steps, [Some(0), Some(1), Some(2), None, None, None, None]);
        assert_eq!(notifications.get(), 3);
    }

    #[test]
    fn progress_should_present_each_reported_state_only_while_metadata_is_live() {
        for (reported, presented) in [
            (ProgressMetadata::None, TerminalProgress::None),
            (ProgressMetadata::Normal(42), TerminalProgress::Normal(42)),
            (
                ProgressMetadata::Indeterminate,
                TerminalProgress::Indeterminate,
            ),
            (ProgressMetadata::Error(30), TerminalProgress::Error),
            (ProgressMetadata::Paused(70), TerminalProgress::Paused),
        ] {
            assert_eq!(
                TerminalProgress::from_metadata(&metadata(reported, MetadataFreshness::Live)),
                presented
            );
            assert_eq!(
                TerminalProgress::from_metadata(&metadata(reported, MetadataFreshness::Stale)),
                TerminalProgress::None
            );
        }
    }
}
