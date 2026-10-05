use std::rc::Rc;

use gpui::{KeyBinding, TestAppContext, VisualTestContext};

use super::*;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
};
use crate::ui::settings_window::test_support::MemoryStorage;

struct RecordingMovement;

impl WindowMovementFactory for RecordingMovement {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

fn install(cx: &mut TestAppContext) {
    let settings = crate::settings::Settings::load(MemoryStorage::with_document(
        &crate::settings::SettingsDocument::default(),
    ));
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        configure_window_chrome(Rc::new(RecordingMovement), cx);
    });
}

fn about_windows(cx: &mut TestAppContext) -> Vec<WindowHandle<AboutWindow>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<AboutWindow>())
        .collect()
}

fn open(cx: &mut TestAppContext) -> WindowHandle<AboutWindow> {
    cx.update(open_or_activate);
    cx.run_until_parked();
    let [window] = about_windows(cx)[..] else {
        panic!("About should open one window");
    };
    window
}

#[gpui::test]
fn a_second_request_activates_the_open_about_window(cx: &mut TestAppContext) {
    install(cx);
    let opened = open(cx);

    // A request from inside About itself must also find it, while its root view is leased.
    opened
        .update(cx, |_, _, cx| open_or_activate(cx))
        .expect("About should stay open");
    cx.update(open_or_activate);
    cx.run_until_parked();

    assert_eq!(about_windows(cx), [opened]);
}

#[gpui::test]
fn about_is_titled_by_the_running_build_and_closes_from_its_own_shortcuts(cx: &mut TestAppContext) {
    install(cx);
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new(
            "escape",
            CloseAboutWindow,
            Some(ABOUT_KEY_CONTEXT),
        )])
    });
    let opened = open(cx);
    let window = &mut VisualTestContext::from_window(opened.into(), cx);
    assert_eq!(
        window.window_title().as_deref(),
        Some(format!("About {}", About::current().name()).as_str())
    );

    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(about_windows(cx).is_empty());

    // Closing forgets the window, so the next request opens a fresh one.
    let reopened = open(cx);
    reopened
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(CloseAboutWindow), cx)
        })
        .expect("About should be open");
    cx.run_until_parked();
    assert!(about_windows(cx).is_empty());
}

#[gpui::test]
fn about_stacks_icon_name_version_description_and_copyright_on_one_center_line(
    cx: &mut TestAppContext,
) {
    install(cx);
    let opened = open(cx);
    let cx = &mut VisualTestContext::from_window(opened.into(), cx);
    cx.run_until_parked();

    let surface = cx.debug_bounds("about-window-surface").unwrap();
    let lines = [
        "about-icon",
        "about-name",
        "about-version",
        "about-description",
        "about-copyright",
    ]
    .map(|selector| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
    });
    assert_eq!(surface.size.width, px(WINDOW_WIDTH));
    assert_eq!(lines[0].size, size(px(ICON_SIZE), px(ICON_SIZE)));
    for line in &lines {
        assert!(
            (line.center().x - surface.center().x).abs() < px(0.5),
            "{line:?} is off the center line of {surface:?}"
        );
    }
    for pair in lines.windows(2) {
        assert!(pair[0].bottom() < pair[1].top(), "{pair:?} overlap");
    }
    // The window is exactly as tall as its content, which wraps inside the text inset.
    let bottom_inset = cx.update(|_, cx| crate::ui::appearance::chrome(cx).spacing(BOTTOM_INSET));
    assert!(
        (surface.bottom() - lines[4].bottom() - bottom_inset).abs() <= px(1.0),
        "{lines:?} in {surface:?}"
    );
    for line in &lines[1..] {
        assert!(line.left() >= surface.left() + px(TEXT_INSET));
    }
    assert!(
        opened
            .read_with(cx, |about, _| about.icon.is_some())
            .unwrap(),
        "the icon must draw"
    );
}

#[gpui::test]
fn client_decorated_about_offers_only_close_at_the_desktop_edge_inset(cx: &mut TestAppContext) {
    install(cx);
    let opened = open(cx);
    let cx = &mut VisualTestContext::from_window(opened.into(), cx);
    let server_size = cx.debug_bounds("about-window-surface").unwrap().size;
    // A client frame grows the window by its shadow inset and keeps the surface's size.
    let inset = px(crate::platform::window_chrome::CLIENT_FRAME_INSET * 2.0);
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    cx.simulate_resize(size(server_size.width + inset, server_size.height + inset));
    cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
        left: [None; 3],
        right: [
            Some(gpui::WindowButton::Minimize),
            Some(gpui::WindowButton::Maximize),
            Some(gpui::WindowButton::Close),
        ],
    }));
    cx.run_until_parked();

    let surface = cx.debug_bounds("about-window-surface").unwrap();
    assert_eq!(surface.size, server_size);
    let close = cx.debug_bounds("window-close").expect("About offers Close");
    let edge_margin = px(spaceterm_ui::DesktopWindowStyle::Adwaita
        .control_metrics()
        .edge_margin);
    assert!(cx.debug_bounds("window-minimize").is_none());
    assert!(cx.debug_bounds("window-maximize").is_none());
    assert_eq!(surface.right() - close.right(), edge_margin);
    assert_eq!(close.top() - surface.top(), edge_margin);
    let icon = cx.debug_bounds("about-icon").unwrap();
    assert!(
        close.left() > icon.right(),
        "Close {close:?} must not cover the icon {icon:?} in {surface:?}"
    );

    // Server decorations draw no client controls.
    cx.simulate_decorations(gpui::Decorations::Server);
    cx.run_until_parked();
    assert!(cx.debug_bounds("window-close").is_none());
}

#[gpui::test]
fn about_opens_as_a_fixed_size_modeless_panel(cx: &mut TestAppContext) {
    install(cx);
    cx.update(|cx| {
        let options = window_options("About".into(), About::current(), cx);
        assert!(!options.is_resizable);
        assert!(!options.is_minimizable);
        assert!(options.is_movable);
        assert_eq!(options.kind, WindowKind::Normal);
        let Some(WindowBounds::Windowed(bounds)) = options.window_bounds else {
            panic!("About opens windowed");
        };
        assert_eq!(bounds.size.width, px(WINDOW_WIDTH));
        assert_eq!(options.window_min_size, Some(bounds.size));
    });
}

#[gpui::test]
fn about_presents_each_fact_to_assistive_technology(cx: &mut TestAppContext) {
    install(cx);
    let opened = open(cx);
    let cx = &mut VisualTestContext::from_window(opened.into(), cx);
    cx.activate_accessibility();
    cx.run_until_parked();

    let tree: serde_json::Value = cx
        .update(|window, _| serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap());
    let labels = tree["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|node| node["aria"]["role"] == "Label")
        .filter_map(|node| node["aria"]["value"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let about = About::current();
    for fact in [
        about.name().to_owned(),
        about.version_line(),
        about.description().to_owned(),
        about.copyright().to_owned(),
    ] {
        assert_eq!(
            labels.iter().filter(|label| **label == fact).count(),
            1,
            "{fact:?} must be read once by a screen reader, found {labels:?}"
        );
    }
}
