//! The status marks a Pane Caption and a Tab item present beside a Terminal's title.
//!
//! Both surfaces describe the same Terminal Session, so the attention mark and the OSC 9;4 progress
//! status are drawn here once. Each mark is typed from sanitized Terminal Metadata and never from
//! the title text, which stays opaque: a loader a program draws in its own cells or title is not
//! something host chrome can see or restate.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ElementId, Hsla, PathBuilder, Pixels, Point, Rgba, Task, Window,
    canvas, div, point, px,
};
use spaceterm_ui::{Icon, IconName};

use crate::terminal::metadata::{MetadataFreshness, ProgressMetadata, TerminalMetadataSnapshot};

/// The attention mark's diameter, before density scaling.
pub(crate) const ATTENTION_INDICATOR_SIZE: f32 = 6.0;

/// How far the ring's stroke sits inside the indicator's square, as a share of its size.
const PROGRESS_STROKE_SHARE: f32 = 0.14;
/// The resting ring behind a reported percentage, as a share of the foreground's opacity.
const PROGRESS_TRACK_OPACITY: f32 = 0.28;
/// The loader's arc length, in degrees.
const LOADER_SWEEP_DEGREES: f32 = 100.0;
/// How many positions the loader steps through per revolution.
const LOADER_STEPS: u8 = 12;
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

/// The steady attention mark shared by Pane Captions and Tab items.
pub(crate) fn attention_indicator(selector: String, size: Pixels, color: Rgba) -> AnyElement {
    div()
        .debug_selector(move || selector)
        .size(size)
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
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

/// The loader's position, advanced on its own clock while the loader stays on screen.
struct LoaderClock {
    step: u8,
    _tick: Task<()>,
}

fn loader(id: ElementId, size: Pixels) -> impl IntoElement {
    canvas(
        move |_, window: &mut Window, cx: &mut App| {
            window
                .use_keyed_state(id, cx, |_, cx| LoaderClock {
                    step: 0,
                    _tick: cx.spawn(async move |clock, cx| {
                        loop {
                            cx.background_executor().timer(LOADER_STEP).await;
                            let advanced = clock.update(cx, |clock: &mut LoaderClock, cx| {
                                clock.step = (clock.step + 1) % LOADER_STEPS;
                                cx.notify();
                            });
                            if advanced.is_err() {
                                break;
                            }
                        }
                    }),
                })
                .read(cx)
                .step
        },
        move |bounds, step: u8, window: &mut Window, _: &mut App| {
            let start = f32::from(step) * 360.0 / f32::from(LOADER_STEPS);
            let color = window.text_style().color;
            paint_arc(bounds, start, start + LOADER_SWEEP_DEGREES, color, window);
        },
    )
    .size(size)
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
