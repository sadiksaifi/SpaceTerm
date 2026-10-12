use std::cell::RefCell;
use std::collections::HashSet;

use gpui::{
    Context, Entity, FocusHandle, KeyDownEvent, Modifiers, Render, TestAppContext,
    VisualTestContext, px,
};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::*;
use crate::ui::pane_presentation::tests::{prepared_appearance, press};

const SERVER: ServerSectionId = ServerSectionId::new(0x5e);

/// A sidebar stand-in: one focused list holding one server's section, which answers keys on the
/// section's header the way the sidebar does and applies the expansion each request asks for.
struct SidebarHarness {
    focus: FocusHandle,
    model: ServerSectionModel,
    workspaces: usize,
    toggles: Rc<RefCell<Vec<bool>>>,
    keys: Rc<RefCell<Vec<ServerHeaderKey>>>,
    attaches: Rc<RefCell<usize>>,
}

impl Render for SidebarHarness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let toggles = Rc::clone(&self.toggles);
        let attaches = Rc::clone(&self.attaches);
        let section = ServerSection::new(self.model.clone())
            .selected(true)
            .emphasized(self.focus.is_focused(window))
            .set_position(1, 1)
            .workspaces(
                (0..self.workspaces)
                    .map(|index| {
                        div()
                            .debug_selector(move || format!("workspace-{index}"))
                            .h(px(40.0))
                            .child(format!("Workspace {index}"))
                            .into_any_element()
                    })
                    .collect(),
            )
            .on_toggle(move |expanded, _, cx| {
                toggles.borrow_mut().push(expanded);
                let _ = owner.update(cx, |harness, cx| {
                    harness.model.expanded = expanded;
                    cx.notify();
                });
            })
            .on_attach(move |_, _| *attaches.borrow_mut() += 1);
        div().size_full().child(
            div()
                .id("sidebar")
                .track_focus(&self.focus)
                .w(px(280.0))
                .on_key_down(cx.listener(|harness, event: &KeyDownEvent, window, cx| {
                    match harness.model.header_key(&event.keystroke) {
                        Some(key) => {
                            harness.keys.borrow_mut().push(key);
                            if let ServerHeaderKey::SetExpanded(expanded) = key {
                                harness.model.expanded = expanded;
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }
                        None if event.keystroke.key == "tab" => {
                            window.focus_next(cx);
                            cx.stop_propagation();
                        }
                        None => {}
                    }
                }))
                .child(ControlHost::Panel.mount(section)),
        )
    }
}

struct Sidebar<'a> {
    harness: Entity<SidebarHarness>,
    toggles: Rc<RefCell<Vec<bool>>>,
    keys: Rc<RefCell<Vec<ServerHeaderKey>>>,
    attaches: Rc<RefCell<usize>>,
    cx: &'a mut VisualTestContext,
}

impl Sidebar<'_> {
    fn update(&mut self, change: impl FnOnce(&mut SidebarHarness)) {
        self.harness.update(self.cx, |harness, cx| {
            change(harness);
            cx.notify();
        });
        self.cx.run_until_parked();
    }

    fn shows(&mut self, part: &str) -> bool {
        self.cx.debug_bounds(selector(part)).is_some()
    }

    fn click(&mut self, part: &str) {
        let point = self
            .cx
            .debug_bounds(selector(part))
            .unwrap_or_else(|| panic!("{part} should be on screen"))
            .center();
        self.cx.simulate_click(point, Modifiers::none());
        self.cx.run_until_parked();
    }

    fn header(&mut self) -> serde_json::Value {
        A11yTree::read(self.cx).node("Studio").clone()
    }

    fn expanded(&mut self) -> bool {
        self.harness
            .read_with(self.cx, |harness, _| harness.model.expanded)
    }
}

fn selector(part: &str) -> &'static str {
    SERVER.selector(part).leak()
}

fn sidebar(
    connection: ServerConnection,
    expanded: bool,
    workspaces: usize,
    cx: &mut TestAppContext,
) -> Sidebar<'_> {
    cx.update(crate::ui::init).unwrap();
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let keys = Rc::new(RefCell::new(Vec::new()));
    let attaches = Rc::new(RefCell::new(0));
    let (harness, cx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        SidebarHarness {
            focus,
            model: ServerSectionModel {
                id: SERVER,
                name: DisplayName::reported("Studio"),
                connection,
                expanded,
            },
            workspaces,
            toggles: Rc::clone(&toggles),
            keys: Rc::clone(&keys),
            attaches: Rc::clone(&attaches),
        }
    });
    cx.run_until_parked();
    Sidebar {
        harness,
        toggles,
        keys,
        attaches,
        cx,
    }
}

#[test]
fn every_connection_has_its_own_glyph_and_label_so_color_never_carries_it_alone() {
    let labels = ServerConnection::ALL
        .iter()
        .map(|connection| connection.label())
        .collect::<HashSet<_>>();
    // Both update directions share a label; the explanation says which side to update.
    assert_eq!(labels.len(), ServerConnection::ALL.len() - 1);
    let glyphs = ServerConnection::ALL
        .iter()
        .map(|connection| connection.glyph().map(IconName::unicode))
        .collect::<HashSet<_>>();
    assert_eq!(glyphs.len(), ServerConnection::ALL.len() - 1);
    assert_eq!(
        ServerConnection::Connecting.glyph().map(IconName::unicode),
        None
    );
}

#[test]
fn an_incompatible_server_says_which_side_to_update() {
    let application = crate::application_identity::ApplicationIdentity::current().display_name();
    let studio = DisplayName::reported("Studio");
    assert_eq!(
        ServerConnection::Incompatible(UpdateRequired::ThisComputer)
            .explanation(&studio)
            .as_deref(),
        Some(format!("Update {application} on this computer to connect.").as_str())
    );
    assert_eq!(
        ServerConnection::Incompatible(UpdateRequired::Server)
            .explanation(&studio)
            .as_deref(),
        Some(format!("Update {application} on Studio to connect.").as_str())
    );
    assert_eq!(
        ServerConnection::Incompatible(UpdateRequired::Server)
            .explanation(&DisplayName::unnamed())
            .as_deref(),
        Some(format!("Update {application} on the server to connect.").as_str())
    );
    for connection in [
        ServerConnection::Connected,
        ServerConnection::Connecting,
        ServerConnection::Offline,
        ServerConnection::Stopped,
    ] {
        assert_eq!(connection.explanation(&studio), None, "{connection:?}");
    }
}

#[test]
fn a_header_names_an_unnamed_server_and_bounds_a_reported_name() {
    let model = |name| ServerSectionModel {
        id: SERVER,
        name,
        connection: ServerConnection::Connected,
        expanded: true,
    };
    assert_eq!(model(DisplayName::reported("  ")).title(), "Unnamed Server");
    let long = "s".repeat(200);
    assert!(
        model(DisplayName::reported(&long)).title().chars().count()
            <= crate::ui::pane_presentation::MAXIMUM_DISPLAY_NAME_CHARS
    );
}

#[test]
fn header_keys_expand_enter_and_collapse_like_a_source_list_group() {
    let key = |expanded, keystroke: &str| {
        ServerSectionModel {
            id: SERVER,
            name: DisplayName::unnamed(),
            connection: ServerConnection::Connected,
            expanded,
        }
        .header_key(&Keystroke::parse(keystroke).unwrap())
    };
    assert_eq!(
        key(false, "right"),
        Some(ServerHeaderKey::SetExpanded(true))
    );
    assert_eq!(key(true, "right"), Some(ServerHeaderKey::EnterSection));
    assert_eq!(key(true, "left"), Some(ServerHeaderKey::SetExpanded(false)));
    assert_eq!(key(false, "left"), None);
    assert_eq!(
        key(true, "enter"),
        Some(ServerHeaderKey::SetExpanded(false))
    );
    assert_eq!(
        key(false, "space"),
        Some(ServerHeaderKey::SetExpanded(true))
    );
    for keystroke in [
        "up",
        "down",
        "tab",
        "cmd-right",
        "alt-left",
        "shift-space",
        "a",
    ] {
        assert_eq!(key(false, keystroke), None, "{keystroke}");
        assert_eq!(key(true, keystroke), None, "{keystroke}");
    }
}

#[test]
fn connection_text_stays_readable_on_a_resting_or_selected_header_in_light_and_dark() {
    for appearance in [
        crate::appearance::Appearance::Light,
        crate::appearance::Appearance::Dark,
    ] {
        let chrome = prepared_appearance(appearance);
        for (selected, emphasized) in [(false, false), (true, false), (true, true)] {
            let colors = header_colors(selected, emphasized, 0.0, &chrome);
            let background = if selected {
                colors.row_selected_background
            } else {
                colors.row_background
            };
            for connection in ServerConnection::ALL {
                let Some(tone) = connection.tone(&colors) else {
                    continue;
                };
                for (part, minimum) in [("glyph", 3.0), ("label", 4.5)] {
                    let paint = workspace_row_status_paint(tone, selected, minimum, &colors);
                    let ratio = paint.normal.contrast_ratio(background);
                    assert!(
                        ratio >= minimum,
                        "{appearance:?} {connection:?} {part} selected={selected} \
                         emphasized={emphasized}: {ratio:.2}"
                    );
                }
            }
        }
    }
}

#[gpui::test]
fn a_header_publishes_its_server_connection_and_disclosure(cx: &mut TestAppContext) {
    let mut sidebar = sidebar(ServerConnection::Offline, true, 2, cx);
    let header = sidebar.header();
    assert_eq!(header["aria"]["role"], "TreeItem");
    assert_eq!(header["aria"]["description"], "Offline");
    assert_eq!(header["aria"]["expanded"], true);
    assert_eq!(header["aria"]["level"], 1);
    assert_eq!(header["aria"]["selected"], true);
    assert!(sidebar.shows("glyph"));
    assert!(sidebar.shows("status"));
    assert!(sidebar.shows("header-expanded"));
    assert!(sidebar.cx.debug_bounds("workspace-1").is_some());
    assert!(!sidebar.shows("empty"));

    for connection in ServerConnection::ALL {
        sidebar.update(|harness| harness.model.connection = connection);
        let header = sidebar.header();
        let description = header["aria"]["description"].as_str().unwrap().to_owned();
        assert!(
            description.starts_with(connection.label()),
            "{connection:?}: {description}"
        );
        assert!(sidebar.shows("glyph"), "{connection:?}");
        assert!(sidebar.shows("status"), "{connection:?}");
        // An expanded section shows how to resolve an incompatible connection.
        assert_eq!(
            sidebar.shows("explanation"),
            matches!(connection, ServerConnection::Incompatible(_)),
            "{connection:?}"
        );
    }
}

#[gpui::test]
fn the_header_collapses_by_pointer_keyboard_and_accessibility(cx: &mut TestAppContext) {
    let mut sidebar = sidebar(ServerConnection::Connected, true, 1, cx);

    sidebar.click("header-expanded");
    assert!(!sidebar.expanded());
    assert!(sidebar.shows("header-collapsed"));
    assert!(sidebar.cx.debug_bounds("workspace-0").is_none());
    assert_eq!(sidebar.header()["aria"]["expanded"], false);
    sidebar.click("header-collapsed");
    assert!(sidebar.expanded());
    assert_eq!(*sidebar.toggles.borrow(), [false, true]);

    press(sidebar.cx, "left");
    assert!(!sidebar.expanded());
    press(sidebar.cx, "left right");
    assert!(sidebar.expanded());
    press(sidebar.cx, "right space enter");
    assert_eq!(
        std::mem::take(&mut *sidebar.keys.borrow_mut()),
        [
            ServerHeaderKey::SetExpanded(false),
            ServerHeaderKey::SetExpanded(true),
            ServerHeaderKey::EnterSection,
            ServerHeaderKey::SetExpanded(false),
            ServerHeaderKey::SetExpanded(true),
        ]
    );
    assert!(sidebar.cx.debug_bounds("workspace-0").is_some());

    sidebar.toggles.borrow_mut().clear();
    let header = sidebar.header();
    perform(sidebar.cx, &header, accesskit::Action::Collapse);
    sidebar.cx.run_until_parked();
    assert!(!sidebar.expanded());
    let header = sidebar.header();
    perform(sidebar.cx, &header, accesskit::Action::Expand);
    sidebar.cx.run_until_parked();
    assert!(sidebar.expanded());
    let header = sidebar.header();
    perform(sidebar.cx, &header, accesskit::Action::Click);
    sidebar.cx.run_until_parked();
    assert!(!sidebar.expanded());
    assert_eq!(*sidebar.toggles.borrow(), [false, true, false]);
}

#[gpui::test]
fn an_empty_section_offers_to_attach_a_workspace_while_its_server_is_connected(
    cx: &mut TestAppContext,
) {
    let mut sidebar = sidebar(ServerConnection::Connected, true, 0, cx);
    assert!(sidebar.shows("empty"));
    let tree = A11yTree::read(sidebar.cx);
    assert!(tree.find_text("No Workspaces attached").is_some());
    assert_ne!(tree.node("Attach Workspace…")["aria"]["disabled"], true);

    sidebar.click("attach");
    assert_eq!(*sidebar.attaches.borrow(), 1);
    // Attaching never toggles the section.
    assert!(sidebar.toggles.borrow().is_empty());
    press(sidebar.cx, "tab");
    assert_eq!(
        A11yTree::read(sidebar.cx)
            .focused()
            .map(|node| node["aria"]["label"].clone()),
        Some("Attach Workspace…".into())
    );
    press(sidebar.cx, "enter");
    assert_eq!(*sidebar.attaches.borrow(), 2);
    let tree = A11yTree::read(sidebar.cx);
    perform(
        sidebar.cx,
        tree.node("Attach Workspace…"),
        accesskit::Action::Click,
    );
    sidebar.cx.run_until_parked();
    assert_eq!(*sidebar.attaches.borrow(), 3);

    // A server that can't attach keeps the action visible and unavailable.
    for connection in [
        ServerConnection::Connecting,
        ServerConnection::Offline,
        ServerConnection::Incompatible(UpdateRequired::ThisComputer),
        ServerConnection::Stopped,
    ] {
        sidebar.update(|harness| harness.model.connection = connection);
        assert!(sidebar.shows("empty"), "{connection:?}");
        assert_eq!(
            A11yTree::read(sidebar.cx).node("Attach Workspace…")["aria"]["disabled"],
            true,
            "{connection:?}"
        );
        sidebar.click("attach");
        assert_eq!(*sidebar.attaches.borrow(), 3, "{connection:?}");
    }

    // A collapsed section hides its empty state, and Workspaces replace it.
    sidebar.update(|harness| harness.model.expanded = false);
    assert!(!sidebar.shows("empty"));
    sidebar.update(|harness| {
        harness.model.expanded = true;
        harness.workspaces = 1;
    });
    assert!(!sidebar.shows("empty"));
    assert!(sidebar.cx.debug_bounds("workspace-0").is_some());
}

#[gpui::test]
fn a_long_server_name_truncates_before_the_connection_or_disclosure(cx: &mut TestAppContext) {
    let mut sidebar = sidebar(ServerConnection::Connecting, false, 0, cx);
    sidebar.update(|harness| {
        harness.model.name = DisplayName::reported(&"Workstation on the third floor ".repeat(4));
    });
    let header = sidebar
        .cx
        .debug_bounds(selector("header-collapsed"))
        .unwrap();
    assert_eq!(header.size.width, px(280.0));
    let name = sidebar.cx.debug_bounds(selector("name")).unwrap();
    let status = sidebar.cx.debug_bounds(selector("status")).unwrap();
    let disclosure = sidebar.cx.debug_bounds(selector("disclosure")).unwrap();
    assert!(name.right() <= status.left(), "{name:?} {status:?}");
    assert!(
        status.right() <= disclosure.left(),
        "{status:?} {disclosure:?}"
    );
    assert!(
        disclosure.right() <= header.right(),
        "{disclosure:?} {header:?}"
    );
    assert!(name.size.width > px(0.0));
}
