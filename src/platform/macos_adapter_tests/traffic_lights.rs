use super::*;

#[gpui::test]
fn density_preview_should_reposition_open_workspace_and_settings_traffic_lights(
    cx: &mut gpui::TestAppContext,
) {
    use crate::appearance::ChromeDensity;
    use crate::platform::window_frame::{TrafficLightPlacement, WindowFrameGeometry};
    use crate::settings::SettingsDocument;
    use gpui::{point, px};

    let geometry = WindowFrameGeometry::new(Some(16.0))
        .with_outer_edge_width(1.0)
        .with_traffic_lights(
            TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(41.0), px(78.0)),
            TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0), px(78.0)),
        );
    let mut wiring = parts(Rc::default(), Rc::default());
    wiring.window_frame = geometry;
    let host = HostComposition::new(wiring).unwrap().with_appearance(
        Arc::new(EmptySettingsStorage),
        Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
    );
    let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
    cx.run_until_parked();
    cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
    cx.run_until_parked();
    let settings_window = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
            .expect("Settings window")
    });

    assert_eq!(
        (
            cx.traffic_light_position_updates(workspace.into()),
            cx.traffic_light_position_updates(settings_window.into()),
        ),
        (
            vec![point(px(15.5), px(14.0))],
            vec![point(px(12.0), px(11.0))],
        )
    );

    let settings = cx.update(|cx| {
        cx.global::<crate::ui::appearance_runtime::AppearanceRuntime>()
            .settings
            .clone()
    });
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = SettingsDocument::default();
    candidate.preferences.window.density = ChromeDensity::Comfortable;
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    let (workspace_expected, settings_expected) = cx.update(|cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        let workspace_height = crate::ui::WorkspaceFrame::for_appearance(appearance, cx)
            .top_chrome_height(appearance.top_height());
        (
            geometry
                .workspace_traffic_light_position(workspace_height)
                .unwrap(),
            geometry
                .sidebar_window_traffic_light_position(appearance.top_height())
                .unwrap(),
        )
    });

    assert_eq!(
        (
            cx.traffic_light_position_updates(workspace.into()),
            cx.traffic_light_position_updates(settings_window.into()),
        ),
        (
            vec![point(px(15.5), px(14.0)), workspace_expected],
            vec![point(px(12.0), px(11.0)), settings_expected],
        )
    );
}
