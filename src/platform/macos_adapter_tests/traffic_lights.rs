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
        Arc::new(crate::settings::storage::testing::MemoryStorage::default()),
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
    candidate.appearance.window.density = ChromeDensity::Comfortable;
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();

    assert_eq!(
        (
            cx.traffic_light_position_updates(workspace.into()),
            cx.traffic_light_position_updates(settings_window.into()),
        ),
        (
            vec![point(px(15.5), px(14.0)), point(px(15.5), px(16.4))],
            vec![point(px(12.0), px(11.0)), point(px(12.0), px(13.4))],
        )
    );

    settings.cancel_preview(&token).unwrap();
    cx.run_until_parked();
    assert_eq!(
        (
            cx.traffic_light_position_updates(workspace.into()),
            cx.traffic_light_position_updates(settings_window.into())
        ),
        (
            vec![
                point(px(15.5), px(14.0)),
                point(px(15.5), px(16.4)),
                point(px(15.5), px(14.0))
            ],
            vec![
                point(px(12.0), px(11.0)),
                point(px(12.0), px(13.4)),
                point(px(12.0), px(11.0))
            ],
        )
    );
}
