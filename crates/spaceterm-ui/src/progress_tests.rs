use gpui::prelude::*;
use gpui::{Context, Render, TestAppContext, VisualTestContext, Window, div, px, rgba};

use crate::progress::spinner_frame_index;
use crate::{
    DeterminateProgress, FrameSpinner, ProgressBar, ProgressMetrics, ProgressMotion, ProgressPaint,
    ProgressRing, ProgressSize, ProgressSizes, ProgressState, ProgressTheme,
};

const BAR_WIDTH: f32 = 240.0;

fn test_theme(motion: ProgressMotion) -> ProgressTheme {
    ProgressTheme::new(
        ProgressPaint::new(rgba(0x303030ff), rgba(0x5599ffff)),
        ProgressSizes::new(
            ProgressMetrics::new(px(4.0), px(2.0), px(20.0), px(2.0)),
            ProgressMetrics::new(px(8.0), px(4.0), px(32.0), px(4.0)),
        ),
        motion,
    )
}

#[derive(Clone, Copy)]
enum FixtureKind {
    Bar { right_to_left: bool },
    Ring,
    Spinner,
}

struct ProgressFixture {
    kind: FixtureKind,
    state: ProgressState,
    size: ProgressSize,
}

impl Render for ProgressFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let indicator = match self.kind {
            FixtureKind::Bar { right_to_left } => {
                ProgressBar::new("test-progress", "Copying files", self.state)
                    .size(self.size)
                    .right_to_left(right_to_left)
                    .debug_selector("test-progress")
                    .into_any_element()
            }
            FixtureKind::Ring => {
                let ProgressState::Determinate(progress) = self.state else {
                    panic!("ring fixtures require determinate progress")
                };
                ProgressRing::new("test-progress", "Copying files", progress)
                    .size(self.size)
                    .debug_selector("test-progress")
                    .into_any_element()
            }
            FixtureKind::Spinner => FrameSpinner::new("test-progress", "Copying files")
                .size(self.size)
                .debug_selector("test-progress")
                .into_any_element(),
        };
        div().w(px(BAR_WIDTH)).child(indicator)
    }
}

fn fixture_window(
    cx: &mut TestAppContext,
    kind: FixtureKind,
    state: ProgressState,
    size: ProgressSize,
    motion: ProgressMotion,
) -> &mut VisualTestContext {
    cx.set_global(test_theme(motion));
    let (_, cx) = cx.add_window_view(move |_, _| ProgressFixture { kind, state, size });
    cx.run_until_parked();
    cx
}

fn determinate(value: f64) -> ProgressState {
    ProgressState::Determinate(
        DeterminateProgress::new(value).expect("finite progress should normalize"),
    )
}

#[gpui::test]
fn determinate_bar_uses_the_normalized_fraction(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: false,
        },
        determinate(0.25),
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    let track = cx
        .debug_bounds("test-progress-track")
        .expect("progress track should render");
    let indicator = cx
        .debug_bounds("test-progress-indicator")
        .expect("progress indicator should render");

    assert_eq!(indicator.size.width, track.size.width * 0.25);
}

#[gpui::test]
fn zero_progress_keeps_the_track_and_zero_width_indicator(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: false,
        },
        determinate(0.0),
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    assert!(cx.debug_bounds("test-progress-track").is_some());
    let indicator = cx
        .debug_bounds("test-progress-indicator")
        .expect("zero progress indicator should preserve semantic structure");

    assert_eq!(indicator.size.width, px(0.0));
}

#[gpui::test]
fn maximum_progress_remains_visible_until_its_owner_removes_it(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: false,
        },
        determinate(1.0),
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    let track = cx
        .debug_bounds("test-progress-track")
        .expect("maximum progress track should remain rendered");
    let indicator = cx
        .debug_bounds("test-progress-indicator")
        .expect("maximum progress indicator should remain rendered");

    assert_eq!(indicator.size.width, track.size.width);
}

#[gpui::test]
fn right_to_left_bar_fills_from_the_trailing_physical_edge(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: true,
        },
        determinate(0.4),
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    let track = cx
        .debug_bounds("test-progress-track")
        .expect("progress track should render");
    let indicator = cx
        .debug_bounds("test-progress-indicator")
        .expect("progress indicator should render");

    assert_eq!(indicator.right(), track.right());
}

#[gpui::test]
fn named_sizes_select_their_installed_geometry(cx: &mut TestAppContext) {
    let compact = fixture_window(
        cx,
        FixtureKind::Ring,
        determinate(0.5),
        ProgressSize::Compact,
        ProgressMotion::Standard,
    )
    .debug_bounds("test-progress-track")
    .expect("compact progress ring should render")
    .size;

    let cx = fixture_window(
        cx,
        FixtureKind::Ring,
        determinate(0.5),
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );
    let regular = cx
        .debug_bounds("test-progress-track")
        .expect("regular progress ring should render")
        .size;

    assert_eq!(compact, gpui::size(px(20.0), px(20.0)));
    assert_eq!(regular, gpui::size(px(32.0), px(32.0)));
}

#[gpui::test]
fn frame_spinner_named_sizes_are_stable_across_frames(cx: &mut TestAppContext) {
    let compact = fixture_window(
        cx,
        FixtureKind::Spinner,
        ProgressState::Indeterminate,
        ProgressSize::Compact,
        ProgressMotion::Standard,
    )
    .debug_bounds("test-progress")
    .expect("compact frame spinner should render")
    .size;
    let regular = fixture_window(
        cx,
        FixtureKind::Spinner,
        ProgressState::Indeterminate,
        ProgressSize::Regular,
        ProgressMotion::Standard,
    )
    .debug_bounds("test-progress")
    .expect("regular frame spinner should render")
    .size;

    assert_eq!(compact, gpui::size(px(20.0), px(20.0)));
    assert_eq!(regular, gpui::size(px(32.0), px(32.0)));
}

#[test]
fn frame_spinner_advances_and_wraps_at_bounded_intervals() {
    assert_eq!(spinner_frame_index(0.0), 0);
    assert_eq!(spinner_frame_index(0.124), 0);
    assert_eq!(spinner_frame_index(0.125), 1);
    assert_eq!(spinner_frame_index(0.999), 7);
    assert_eq!(spinner_frame_index(1.0), 7);
}

#[gpui::test]
fn indeterminate_bar_keeps_a_visible_activity_mark_with_reduced_motion(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: false,
        },
        ProgressState::Indeterminate,
        ProgressSize::Regular,
        ProgressMotion::Reduced,
    );

    assert!(cx.debug_bounds("test-progress-activity").is_some());
    assert!(cx.debug_bounds("test-progress-reduced-motion").is_some());
}

#[gpui::test]
fn standard_indeterminate_bar_uses_the_animated_activity_structure(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Bar {
            right_to_left: false,
        },
        ProgressState::Indeterminate,
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    assert!(cx.debug_bounds("test-progress-activity").is_some());
    assert!(cx.debug_bounds("test-progress-reduced-motion").is_none());
}

#[gpui::test]
fn frame_spinner_keeps_a_static_frame_with_reduced_motion(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Spinner,
        ProgressState::Indeterminate,
        ProgressSize::Regular,
        ProgressMotion::Reduced,
    );

    assert!(cx.debug_bounds("test-progress-reduced-motion").is_some());
    assert!(cx.debug_bounds("test-progress-frame").is_some());
}

#[gpui::test]
fn standard_frame_spinner_uses_the_animated_frame_structure(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Spinner,
        ProgressState::Indeterminate,
        ProgressSize::Regular,
        ProgressMotion::Standard,
    );

    assert!(cx.debug_bounds("test-progress-frame").is_some());
    assert!(cx.debug_bounds("test-progress-reduced-motion").is_none());
}
