use gpui::prelude::*;
use gpui::{AnyElement, Rgba, px};
use spaceterm_ui::{Icon, IconName, MenuEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PaneActionMenuCommand {
    SplitRight,
    SplitDown,
    ToggleZoom,
    Close,
    PinDirectory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseTarget {
    Pane,
    Tab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaneActionMenuPresentation {
    split_right: &'static str,
    split_down: &'static str,
    toggle_zoom: &'static str,
    close: &'static str,
}

fn pane_action_menu_presentation(
    close_target: CloseTarget,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> PaneActionMenuPresentation {
    PaneActionMenuPresentation {
        split_right: presentation.shortcut(&crate::ui::SplitRight),
        split_down: presentation.shortcut(&crate::ui::SplitDown),
        toggle_zoom: presentation.shortcut(&crate::ui::TogglePaneZoom),
        close: match close_target {
            CloseTarget::Pane => presentation.shortcut(&crate::ui::ClosePane),
            CloseTarget::Tab => presentation.shortcut(&crate::ui::CloseTab),
        },
    }
}

pub(crate) fn pane_action_menu_entries(
    debug_prefix: &'static str,
    pin_directory: Option<bool>,
    zoomed: bool,
    zoom_enabled: bool,
    close_target: CloseTarget,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<MenuEntry<PaneActionMenuCommand>> {
    let (zoom_icon, zoom_label) = zoom_presentation(zoomed);
    let (close_label, close_selector) = close_presentation(close_target);
    let shortcuts = pane_action_menu_presentation(close_target, presentation);

    let mut entries = vec![
        MenuEntry::action("Split Right", PaneActionMenuCommand::SplitRight)
            .icon(menu_icon(pane_action_icon(
                PaneActionMenuCommand::SplitRight,
                zoomed,
            )))
            .shortcut(shortcuts.split_right)
            .debug_selector(format!("{debug_prefix}-row-split-right")),
        MenuEntry::action("Split Down", PaneActionMenuCommand::SplitDown)
            .icon(menu_icon(pane_action_icon(
                PaneActionMenuCommand::SplitDown,
                zoomed,
            )))
            .shortcut(shortcuts.split_down)
            .debug_selector(format!("{debug_prefix}-row-split-down")),
        MenuEntry::action(zoom_label, PaneActionMenuCommand::ToggleZoom)
            .icon(menu_icon(zoom_icon))
            .shortcut(shortcuts.toggle_zoom)
            .disabled(!zoom_enabled)
            .debug_selector(format!("{debug_prefix}-row-toggle-zoom")),
        MenuEntry::separator(),
        MenuEntry::action(close_label, PaneActionMenuCommand::Close)
            .icon(menu_icon(pane_action_icon(
                PaneActionMenuCommand::Close,
                zoomed,
            )))
            .shortcut(shortcuts.close)
            .destructive(true)
            .debug_selector(format!("{debug_prefix}-row-{close_selector}")),
    ];
    if let Some(enabled) = pin_directory {
        entries.insert(
            3,
            MenuEntry::action(
                "Pin workspace to this directory",
                PaneActionMenuCommand::PinDirectory,
            )
            .icon(menu_icon(IconName::Pin))
            .disabled(!enabled)
            .debug_selector(format!("{debug_prefix}-row-pin-directory")),
        );
    }
    entries
}

fn pane_action_icon(command: PaneActionMenuCommand, zoomed: bool) -> IconName {
    match command {
        PaneActionMenuCommand::SplitRight => IconName::Columns2,
        PaneActionMenuCommand::SplitDown => IconName::Rows2,
        PaneActionMenuCommand::ToggleZoom if zoomed => IconName::Minimize2,
        PaneActionMenuCommand::ToggleZoom => IconName::Maximize2,
        PaneActionMenuCommand::Close => IconName::X,
        PaneActionMenuCommand::PinDirectory => IconName::Pin,
    }
}

fn zoom_presentation(zoomed: bool) -> (IconName, &'static str) {
    let label = if zoomed { "Restore Panes" } else { "Zoom Pane" };
    (
        pane_action_icon(PaneActionMenuCommand::ToggleZoom, zoomed),
        label,
    )
}

fn close_presentation(close_target: CloseTarget) -> (&'static str, &'static str) {
    match close_target {
        CloseTarget::Pane => ("Close Pane", "close-pane"),
        CloseTarget::Tab => ("Close Tab", "close-tab"),
    }
}

pub(crate) fn menu_icon(name: IconName) -> impl Fn(Rgba) -> AnyElement {
    move |foreground| Icon::new(name, px(14.0), foreground).into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, Window, div};
    use spaceterm_ui::{Menu, MenuSize};

    use super::*;

    struct MenuTestOwner {
        zoomed: bool,
        zoom_enabled: bool,
        close_target: CloseTarget,
    }

    impl Render for MenuTestOwner {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(
                Menu::new(
                    "test-menu",
                    "Pane Actions",
                    pane_action_menu_entries(
                        "test-menu",
                        None,
                        self.zoomed,
                        self.zoom_enabled,
                        self.close_target,
                        &crate::desktop_profile::testing_presentation(),
                    ),
                )
                .size(MenuSize::Wide)
                .on_activate(|activation, _, _| {
                    assert_ne!(
                        *activation.action(),
                        PaneActionMenuCommand::ToggleZoom,
                        "disabled Zoom Pane command must not activate"
                    );
                }),
            )
        }
    }

    fn menu_window(
        cx: &mut TestAppContext,
        zoomed: bool,
        zoom_enabled: bool,
        close_target: CloseTarget,
    ) -> &mut gpui::VisualTestContext {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (_, cx) = cx.add_window_view(move |_, _| MenuTestOwner {
            zoomed,
            zoom_enabled,
            close_target,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx
    }

    #[test]
    fn split_entries_should_use_directional_layout_icons() {
        assert!(matches!(
            pane_action_icon(PaneActionMenuCommand::SplitRight, false),
            IconName::Columns2
        ));
        assert!(matches!(
            pane_action_icon(PaneActionMenuCommand::SplitDown, false),
            IconName::Rows2
        ));
    }

    #[test]
    fn zoom_entry_should_use_zoom_and_restore_icons_for_its_state() {
        let (zoom_icon, zoom_label) = zoom_presentation(false);
        let (restore_icon, restore_label) = zoom_presentation(true);

        assert!(matches!(zoom_icon, IconName::Maximize2));
        assert_eq!(zoom_label, "Zoom Pane");
        assert!(matches!(restore_icon, IconName::Minimize2));
        assert_eq!(restore_label, "Restore Panes");
    }

    #[test]
    fn close_entry_should_use_target_specific_label_and_shortcut() {
        assert_eq!(
            close_presentation(CloseTarget::Pane),
            ("Close Pane", "close-pane")
        );
        assert_eq!(
            close_presentation(CloseTarget::Tab),
            ("Close Tab", "close-tab")
        );
    }

    #[test]
    fn menu_surfaces_should_use_host_neutral_profile_shortcuts() {
        let profile = crate::desktop_profile::testing_presentation();
        assert_eq!(
            pane_action_menu_presentation(CloseTarget::Pane, &profile),
            PaneActionMenuPresentation {
                split_right: "Primary+D",
                split_down: "Primary+Shift+D",
                toggle_zoom: "Primary+Shift+Enter",
                close: "Primary+W",
            }
        );
        assert_eq!(
            pane_action_menu_presentation(CloseTarget::Tab, &profile).close,
            "Primary+Shift+W"
        );
    }

    #[gpui::test]
    fn disabled_zoom_entry_should_remain_visible_but_inert(cx: &mut TestAppContext) {
        let cx = menu_window(cx, false, false, CloseTarget::Pane);
        let trigger = cx.debug_bounds("Pane Actions").unwrap().center();
        cx.simulate_click(trigger, gpui::Modifiers::none());
        cx.run_until_parked();

        let zoom = cx
            .debug_bounds("test-menu-row-toggle-zoom")
            .expect("disabled Zoom Pane entry must remain visible")
            .center();
        cx.simulate_click(zoom, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn close_entry_should_keep_target_specific_selector(cx: &mut TestAppContext) {
        let cx = menu_window(cx, false, true, CloseTarget::Tab);
        let trigger = cx.debug_bounds("Pane Actions").unwrap().center();
        cx.simulate_click(trigger, gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-menu-row-close-tab").is_some());
    }
}
