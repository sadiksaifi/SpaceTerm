use gpui::prelude::*;
use gpui::{Context, Render, TestAppContext, VisualTestContext, Window, div, px, rgba};

use crate::progress::{SPINNER_FRAMES, spinner_dot_bounds, spinner_frame_index};
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
        div()
            .w(px(BAR_WIDTH))
            .text_color(rgba(0x123456ff))
            .child(indicator)
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
fn frame_spinner_is_square_in_every_size_and_motion_mode(cx: &mut TestAppContext) {
    for (size, expected) in [
        (ProgressSize::Compact, px(20.0)),
        (ProgressSize::Regular, px(32.0)),
    ] {
        for motion in [ProgressMotion::Standard, ProgressMotion::Reduced] {
            let cx = fixture_window(
                cx,
                FixtureKind::Spinner,
                ProgressState::Indeterminate,
                size,
                motion,
            );
            let root_selector = match motion {
                ProgressMotion::Standard => "test-progress",
                ProgressMotion::Reduced => "test-progress-reduced-motion",
            };
            let root = cx
                .debug_bounds(root_selector)
                .expect("frame spinner root should render");
            let frame = cx
                .debug_bounds("test-progress-frame")
                .expect("frame spinner frame should render");

            assert_eq!(root.size, gpui::size(expected, expected));
            assert_eq!(frame, root);
            assert_eq!(frame.size.width, frame.size.height);
        }
    }
}

#[test]
fn every_spinner_frame_paints_round_separated_dots_inside_the_square() {
    for extent in [px(12.0), px(18.0), px(20.0), px(32.0)] {
        let frame = gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(extent, extent));
        for index in 0..SPINNER_FRAMES.len() {
            let dots = spinner_dot_bounds(frame, index).collect::<Vec<_>>();

            assert!(
                dots.len() >= 3,
                "frame {index} should light most of the ring"
            );
            assert!(dots.iter().all(|dot| dot.size.width == dot.size.height));
            assert!(dots.iter().all(|dot| dot.is_contained_within(&frame)));

            for (first, second) in dots.iter().zip(dots.iter().skip(1)) {
                assert!(
                    !first.intersects(second),
                    "frame {index} should keep every pair of dots apart"
                );
            }
        }
    }
}

#[test]
fn the_spinner_sequence_stays_inside_the_six_dot_ring() {
    // The laid-out cell is three rows tall, so a frame lighting the fourth Braille row would paint
    // below the square the spinner occupies.
    for (index, frame) in SPINNER_FRAMES.into_iter().enumerate() {
        let mask = u32::from(frame) - 0x2800;

        assert_eq!(mask & 0b1100_0000, 0, "frame {index} lights the fourth row");
        assert!(mask.count_ones() >= 3, "frame {index} lights too little");
    }
}

#[test]
fn spinner_dots_share_one_pitch_and_sit_centered_inside_the_square() {
    let extent = px(24.0);
    let frame = gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(extent, extent));
    let mut lattice = Vec::new();

    for index in 0..SPINNER_FRAMES.len() {
        for dot in spinner_dot_bounds(frame, index) {
            if !lattice.contains(&dot) {
                lattice.push(dot);
            }
        }
    }

    // The sequence lights the six-dot ring, so the artwork is two columns of three rows.
    assert_eq!(lattice.len(), 6);

    let mut columns = axis(lattice.iter().map(|dot| dot.origin.x));
    let mut rows = axis(lattice.iter().map(|dot| dot.origin.y));
    columns.dedup();
    rows.dedup();

    assert_eq!(columns.len(), 2);
    assert_eq!(rows.len(), 3);

    let diameter = lattice[0].size.width;
    let column_pitch = columns[1] - columns[0];
    let row_pitch = rows[1] - rows[0];

    assert_close(column_pitch, row_pitch, "one pitch governs both axes");
    assert_close(rows[2] - rows[1], row_pitch, "rows are evenly spaced");
    assert!(
        column_pitch > diameter,
        "neighboring dots keep a visible gap"
    );

    // The cell is inset to a glyph's ink height and centered on both axes, so the spinner keeps
    // the optical weight of the icons and reported glyphs that share its slot.
    let cell_height = rows[2] + diameter - rows[0];
    assert!(
        cell_height < extent * 0.8,
        "the cell should stay well inside the square: {cell_height:?} of {extent:?}"
    );
    assert_close(
        rows[0],
        extent - (rows[2] + diameter),
        "the three rows are centered",
    );
    assert_close(
        columns[0],
        extent - (columns[1] + diameter),
        "the two columns are centered",
    );
}

fn axis(values: impl Iterator<Item = gpui::Pixels>) -> Vec<gpui::Pixels> {
    let mut values = values.collect::<Vec<_>>();
    values.sort_by(|left, right| left.partial_cmp(right).expect("dot offsets are finite"));
    values
}

/// Compares two painted offsets within the tolerance single-precision layout arithmetic carries.
fn assert_close(left: gpui::Pixels, right: gpui::Pixels, message: &str) {
    assert!(
        (left - right).abs() < px(0.01),
        "{message}: {left:?} and {right:?}"
    );
}

#[gpui::test]
fn frame_spinner_paints_only_with_the_inherited_foreground(cx: &mut TestAppContext) {
    let cx = fixture_window(
        cx,
        FixtureKind::Spinner,
        ProgressState::Indeterminate,
        ProgressSize::Compact,
        ProgressMotion::Reduced,
    );
    let quads = cx.update(|window, _| window.painted_quads());
    let expected = gpui::Background::from(rgba(0x123456ff));

    assert!(!quads.is_empty());
    assert!(quads.iter().all(|quad| quad.background == expected));
}

#[test]
fn frame_spinner_advances_and_wraps_at_bounded_intervals() {
    assert_eq!(spinner_frame_index(0.0), 0);
    assert_eq!(spinner_frame_index(0.099), 0);
    assert_eq!(spinner_frame_index(0.1), 1);
    assert_eq!(spinner_frame_index(0.999), 9);
    assert_eq!(spinner_frame_index(1.0), 9);
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
