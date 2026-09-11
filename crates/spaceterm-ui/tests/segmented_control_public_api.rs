use gpui::{ElementId, IntoElement as _, Styled as _, Window, div, px};
use spaceterm_ui::{
    MAXIMUM_SEGMENTED_OPTIONS, SegmentedActivationSource, SegmentedBuildError, SegmentedControl,
    SegmentedControlTheme, SegmentedMetrics, SegmentedOption, SegmentedPaint, SegmentedPaints,
    SegmentedSize, SegmentedSizes, SegmentedValuePaints, Tooltip,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicMode {
    Light,
    Dark,
}

const _: fn() = || {
    let _: usize = MAXIMUM_SEGMENTED_OPTIONS;

    let result = SegmentedControl::new(
        ElementId::Name("public-segmented".into()),
        "Appearance",
        &PublicMode::Dark,
        vec![
            SegmentedOption::new(PublicMode::Light, "Light")
                .disabled(false)
                .debug_selector("public-segmented-light")
                .preview(|color, extent| div().w(extent).h(extent).bg(color).into_any_element()),
            SegmentedOption::new(PublicMode::Dark, "Dark"),
        ],
    );
    let _: Option<SegmentedBuildError> = result.as_ref().err().copied();
    let control = result
        .expect("two options are within the bounded option set")
        .size(SegmentedSize::Card)
        .disabled(false)
        .tab_stop(true)
        .full_width(true)
        .right_to_left(false)
        .debug_selector("public-segmented")
        .tooltip(Tooltip::new(
            "public-segmented-tooltip",
            "Application appearance",
        ))
        .on_change(|change, _: &mut Window, _| {
            let _: Option<&PublicMode> = change.previous();
            let _: &PublicMode = change.requested();
            let _: SegmentedActivationSource = change.source();
        });

    let paint = SegmentedPaint::new(
        gpui::rgba(0x00000000),
        gpui::rgba(0xffffffff),
        gpui::rgba(0x00000000),
    );
    let _: gpui::Rgba = paint.background();
    let _: gpui::Rgba = paint.label();
    let _: gpui::Rgba = paint.border();
    let values = SegmentedValuePaints::new(paint, paint);
    let metrics = SegmentedMetrics::new(px(24.0), px(52.0), px(56.0), px(8.0))
        .horizontal_padding(px(10.0))
        .radius(px(5.0))
        .border_width(px(1.0))
        .focus_gap(px(2.0))
        .preview_gap(px(6.0))
        .typography(px(12.0), 1.2);
    let theme = SegmentedControlTheme::new(
        SegmentedPaints::new(values, values, values, values),
        SegmentedSizes::new(metrics, metrics),
        gpui::rgba(0x00000010),
        gpui::rgba(0x00000014),
        gpui::rgba(0x2277ddff),
    )
    .selected_shadow(spaceterm_ui::ControlShadow::none())
    .scaled_metrics(1.0, 1.0);

    let _ = (control, theme);
};
