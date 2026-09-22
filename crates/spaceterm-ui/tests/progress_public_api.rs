use gpui::{ElementId, px, rgba};
use spaceterm_ui::{
    DeterminateProgress, FrameSpinner, ProgressBar, ProgressMetrics, ProgressMotion, ProgressPaint,
    ProgressRing, ProgressSize, ProgressSizes, ProgressState, ProgressTheme,
};

const _: fn() = || {
    let determinate = DeterminateProgress::new(0.4).expect("finite progress should normalize");
    let bar = ProgressBar::new(
        ElementId::Name("public-progress-bar".into()),
        "Downloading update",
        ProgressState::Determinate(determinate),
    )
    .size(ProgressSize::Compact)
    .right_to_left(false)
    .debug_selector("public-progress-bar");
    let ring = ProgressRing::new(
        "public-progress-ring",
        "Connecting to remote host",
        determinate,
    )
    .size(ProgressSize::Regular)
    .inherited()
    .debug_selector("public-progress-ring");
    let spinner = FrameSpinner::new("public-frame-spinner", "Connecting to remote host")
        .size(ProgressSize::Regular)
        .debug_selector("public-frame-spinner");

    let compact = ProgressMetrics::new(px(3.0), px(2.0), px(20.0), px(2.0));
    let regular = ProgressMetrics::new(px(6.0), px(3.0), px(28.0), px(3.0));
    let theme = ProgressTheme::new(
        ProgressPaint::new(rgba(0x303030ff), rgba(0x5599ffff)),
        ProgressSizes::new(compact, regular),
        ProgressMotion::Standard,
    );

    let _ = (bar, ring, spinner, theme);
};
