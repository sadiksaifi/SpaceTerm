use super::*;

#[test]
fn bundled_terminal_fonts_resolve_all_styles_and_nerd_glyphs() {
    use gpui::PlatformTextSystem as _;
    let native = Arc::new(gpui_macos::MacTextSystem::new());
    assert!(native.font_id(&gpui::font("SpaceTerm Default")).is_err());
    let mut cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        native.clone(),
    );
    cx.update(|cx| register_terminal_fonts(cx).unwrap());
    start(&mut cx);
    cx.update(|cx| {
        let appearance = crate::ui::appearance_runtime::current(cx);
        assert_eq!(
            appearance.terminal.typography.regular.primary_family,
            "SpaceTerm Default"
        );
        let text = cx.text_system();
        let mut faces = std::collections::HashSet::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut font = gpui::font("SpaceTerm Default");
            if bold {
                font.weight = gpui::FontWeight::BOLD;
            }
            if italic {
                font.style = gpui::FontStyle::Italic;
            }
            let id = native
                .font_id(&font)
                .expect("embedded family must resolve without fallback");
            faces.insert(id);
            for character in ['M', '\u{e0b0}', '\u{f120}', '\u{f015}'] {
                assert!(text.advance(id, gpui::px(18.0), character).is_ok());
            }
            let width = text.advance(id, gpui::px(18.0), 'M').unwrap().width;
            for character in ['i', '0', ' '] {
                assert_eq!(
                    text.advance(id, gpui::px(18.0), character).unwrap().width,
                    width
                );
            }
        }
        assert_eq!(
            faces.len(),
            4,
            "each terminal style must resolve its own face"
        );
    });
}

/// Re-applying an unchanged density costs no native work; the owner records the applied row.
#[gpui::test]
fn traffic_light_owner_should_apply_each_row_once(cx: &mut TestAppContext) {
    use crate::platform::window_frame::{TrafficLightPlacement, WindowFrameGeometry};
    use gpui::{point, px};

    let _ = start(cx);
    cx.update(|cx| {
        cx.set_global(
            WindowFrameGeometry::new(Some(16.0))
                .with_outer_edge_width(1.0)
                .with_traffic_lights(
                    TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(41.0), px(78.0)),
                    TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0), px(78.0)),
                ),
        );
    });
    let test_window = cx.add_window(|_, _| gpui::EmptyView);
    let mut owner = WindowTrafficLightOwner::workspace();
    test_window
        .update(cx, |_, window, cx| owner.apply(window, cx))
        .unwrap();
    assert_eq!(
        cx.traffic_light_position_updates(test_window.into()),
        vec![point(px(15.5), px(14.0))]
    );

    // A repeat apply with unchanged chrome records the same row without native work.
    test_window
        .update(cx, |_, window, cx| owner.apply(window, cx))
        .unwrap();
    assert_eq!(
        cx.traffic_light_position_updates(test_window.into()),
        vec![point(px(15.5), px(14.0))]
    );
}
