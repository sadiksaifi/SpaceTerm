use gpui::prelude::*;
use gpui::{AnyElement, Rgba, px};
use gpui_symbols::Icon;
use spaceterm_ui::MenuEntry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PaneActionMenuCommand {
    SplitRight,
    SplitDown,
    ToggleZoom,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseTarget {
    Pane,
    Window,
}

pub(crate) fn pane_action_menu_entries(
    debug_prefix: &'static str,
    zoomed: bool,
    zoom_enabled: bool,
    close_target: CloseTarget,
) -> Vec<MenuEntry<PaneActionMenuCommand>> {
    let (zoom_icon, zoom_label) = zoom_presentation(zoomed);
    let (close_label, close_shortcut, close_selector) = close_presentation(close_target);

    vec![
        MenuEntry::action("Split Right", PaneActionMenuCommand::SplitRight)
            .icon(sf_symbol("rectangle.split.2x1"))
            .shortcut("⌘D")
            .debug_selector(format!("{debug_prefix}-row-split-right")),
        MenuEntry::action("Split Down", PaneActionMenuCommand::SplitDown)
            .icon(sf_symbol("rectangle.split.1x2"))
            .shortcut("⇧⌘D")
            .debug_selector(format!("{debug_prefix}-row-split-down")),
        MenuEntry::action(zoom_label, PaneActionMenuCommand::ToggleZoom)
            .icon(sf_symbol(zoom_icon))
            .shortcut("⇧⌘↩")
            .disabled(!zoom_enabled)
            .debug_selector(format!("{debug_prefix}-row-toggle-zoom")),
        MenuEntry::separator(),
        MenuEntry::action(close_label, PaneActionMenuCommand::Close)
            .icon(sf_symbol("xmark"))
            .shortcut(close_shortcut)
            .destructive(true)
            .debug_selector(format!("{debug_prefix}-row-{close_selector}")),
    ]
}

fn zoom_presentation(zoomed: bool) -> (&'static str, &'static str) {
    if zoomed {
        ("arrow.down.right.and.arrow.up.left", "Restore Panes")
    } else {
        ("arrow.up.left.and.arrow.down.right", "Zoom Pane")
    }
}

fn close_presentation(close_target: CloseTarget) -> (&'static str, &'static str, &'static str) {
    match close_target {
        CloseTarget::Pane => ("Close Pane", "⌘W", "close-pane"),
        CloseTarget::Window => ("Close Window", "⇧⌘W", "close-window"),
    }
}

pub(crate) fn sf_symbol(name: &'static str) -> impl Fn(Rgba) -> AnyElement {
    move |foreground| {
        Icon::new(name)
            .size(px(15.0))
            .color(foreground)
            .into_any_element()
    }
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
                        self.zoomed,
                        self.zoom_enabled,
                        self.close_target,
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
        cx.update(crate::ui::init);
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
    fn zoom_entry_should_use_restore_label_and_icon_when_zoomed() {
        assert_eq!(
            zoom_presentation(true),
            ("arrow.down.right.and.arrow.up.left", "Restore Panes")
        );
    }

    #[test]
    fn close_entry_should_use_target_specific_label_and_shortcut() {
        assert_eq!(
            close_presentation(CloseTarget::Pane),
            ("Close Pane", "⌘W", "close-pane")
        );
        assert_eq!(
            close_presentation(CloseTarget::Window),
            ("Close Window", "⇧⌘W", "close-window")
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
        let cx = menu_window(cx, false, true, CloseTarget::Window);
        let trigger = cx.debug_bounds("Pane Actions").unwrap().center();
        cx.simulate_click(trigger, gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-menu-row-close-window").is_some());
    }
}
