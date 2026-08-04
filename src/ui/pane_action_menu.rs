use gpui::prelude::*;
use gpui::{AnyElement, Context, WeakEntity, Window, div, px, rgba};
use gpui_symbols::Icon;

use crate::theme::{ACTIVE_THEME, Color};

pub(crate) const PANE_ACTION_MENU_WIDTH: f32 = 248.0;
pub(crate) const PANE_ACTION_MENU_HEIGHT: f32 =
    MENU_ROW_HEIGHT * MENU_ITEM_COUNT as f32 + MENU_SEPARATOR_SIZE + MENU_BORDER_SIZE * 2.0;

const MENU_ROW_HEIGHT: f32 = 28.0;
const MENU_ITEM_COUNT: usize = 4;
const MENU_SEPARATOR_SIZE: f32 = 1.0;
const MENU_BORDER_SIZE: f32 = 1.0;
const MENU_CORNER_RADIUS: f32 = 8.0;
const MENU_INNER_CORNER_RADIUS: f32 = MENU_CORNER_RADIUS - MENU_BORDER_SIZE;
const MENU_SHORTCUT_TEXT_SIZE: f32 = 11.0;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuItemSpec {
    command: PaneActionMenuCommand,
    icon: &'static str,
    label: &'static str,
    shortcut: &'static str,
    destructive: bool,
    separator_before: bool,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuRowPosition {
    First,
    Middle,
    Last,
}

pub(crate) fn render_pane_action_menu<T, Handler>(
    menu_id: (&'static str, u64),
    zoomed: bool,
    zoom_enabled: bool,
    close_target: CloseTarget,
    owner: WeakEntity<T>,
    handler: Handler,
) -> AnyElement
where
    T: 'static,
    Handler: Fn(&mut T, PaneActionMenuCommand, &mut Window, &mut Context<T>) + Clone + 'static,
{
    let (debug_prefix, target_id) = menu_id;
    let mut menu = div()
        .id(menu_id)
        .debug_selector(move || format!("{debug_prefix}-{target_id}"))
        .w(px(PANE_ACTION_MENU_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(MENU_CORNER_RADIUS))
        .border(px(MENU_BORDER_SIZE))
        .border_color(gpui_color(ACTIVE_THEME.border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .occlude();

    for (index, spec) in menu_items(zoomed, zoom_enabled, close_target)
        .into_iter()
        .enumerate()
    {
        if spec.separator_before {
            menu = menu.child(
                div()
                    .h(px(MENU_SEPARATOR_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            );
        }
        let position = if index == 0 {
            MenuRowPosition::First
        } else if index == MENU_ITEM_COUNT - 1 {
            MenuRowPosition::Last
        } else {
            MenuRowPosition::Middle
        };
        menu = menu.child(render_menu_row(
            spec,
            position,
            debug_prefix,
            close_target,
            owner.clone(),
            handler.clone(),
        ));
    }

    menu.into_any_element()
}

fn render_menu_row<T, Handler>(
    spec: MenuItemSpec,
    position: MenuRowPosition,
    debug_prefix: &'static str,
    close_target: CloseTarget,
    owner: WeakEntity<T>,
    handler: Handler,
) -> AnyElement
where
    T: 'static,
    Handler: Fn(&mut T, PaneActionMenuCommand, &mut Window, &mut Context<T>) + Clone + 'static,
{
    let (foreground, icon_color, shortcut_color) = menu_item_colors(spec);
    let command_name = spec.command.debug_name(close_target);
    let row = div()
        .id(spec.command as usize)
        .debug_selector(move || format!("{debug_prefix}-row-{command_name}"))
        .h(px(MENU_ROW_HEIGHT))
        .px(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(px(13.0))
        .text_color(gpui_color(foreground))
        .when(position == MenuRowPosition::First, |row| {
            row.rounded_t(px(MENU_INNER_CORNER_RADIUS))
        })
        .when(position == MenuRowPosition::Last, |row| {
            row.rounded_b(px(MENU_INNER_CORNER_RADIUS))
        });
    let row = if spec.enabled {
        row.cursor_pointer()
            .hover(|row| row.bg(gpui_color(ACTIVE_THEME.element_hover)))
            .on_click(move |_, window, cx| {
                let handler = handler.clone();
                let _ = owner.update(cx, |owner, cx| {
                    handler(owner, spec.command, window, cx);
                });
                cx.stop_propagation();
            })
    } else {
        row
    };

    row.child(
        div()
            .w(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(spec.icon)
                    .size(px(15.0))
                    .color(gpui_color(icon_color)),
            ),
    )
    .child(spec.label)
    .child(div().flex_grow())
    .child(
        div()
            .text_size(px(MENU_SHORTCUT_TEXT_SIZE))
            .text_color(gpui_color(shortcut_color))
            .child(spec.shortcut),
    )
    .into_any_element()
}

fn menu_item_colors(spec: MenuItemSpec) -> (Color, Color, Color) {
    if !spec.enabled {
        return (
            ACTIVE_THEME.text_disabled,
            ACTIVE_THEME.icon_disabled,
            ACTIVE_THEME.icon_disabled,
        );
    }
    if spec.destructive {
        return (ACTIVE_THEME.error, ACTIVE_THEME.error, ACTIVE_THEME.icon);
    }
    (ACTIVE_THEME.text, ACTIVE_THEME.icon, ACTIVE_THEME.icon)
}

impl PaneActionMenuCommand {
    fn debug_name(self, close_target: CloseTarget) -> &'static str {
        match self {
            Self::SplitRight => "split-right",
            Self::SplitDown => "split-down",
            Self::ToggleZoom => "toggle-zoom",
            Self::Close => match close_target {
                CloseTarget::Pane => "close-pane",
                CloseTarget::Window => "close-window",
            },
        }
    }
}

fn menu_items(
    zoomed: bool,
    zoom_enabled: bool,
    close_target: CloseTarget,
) -> [MenuItemSpec; MENU_ITEM_COUNT] {
    let (close_label, close_shortcut) = match close_target {
        CloseTarget::Pane => ("Close Pane", "⌘W"),
        CloseTarget::Window => ("Close Window", "⇧⌘W"),
    };

    [
        MenuItemSpec {
            command: PaneActionMenuCommand::SplitRight,
            icon: "rectangle.split.2x1",
            label: "Split Right",
            shortcut: "⌘D",
            destructive: false,
            separator_before: false,
            enabled: true,
        },
        MenuItemSpec {
            command: PaneActionMenuCommand::SplitDown,
            icon: "rectangle.split.1x2",
            label: "Split Down",
            shortcut: "⇧⌘D",
            destructive: false,
            separator_before: false,
            enabled: true,
        },
        MenuItemSpec {
            command: PaneActionMenuCommand::ToggleZoom,
            icon: if zoomed {
                "arrow.down.right.and.arrow.up.left"
            } else {
                "arrow.up.left.and.arrow.down.right"
            },
            label: if zoomed { "Restore Panes" } else { "Zoom Pane" },
            shortcut: "⇧⌘↩",
            destructive: false,
            separator_before: false,
            enabled: zoom_enabled,
        },
        MenuItemSpec {
            command: PaneActionMenuCommand::Close,
            icon: "xmark",
            label: close_label,
            shortcut: close_shortcut,
            destructive: true,
            separator_before: true,
            enabled: true,
        },
    ]
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use gpui::{Render, TestAppContext, Window};

    use super::*;

    struct MenuTestOwner;

    impl Render for MenuTestOwner {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            render_pane_action_menu(
                ("test-menu", 1),
                false,
                false,
                CloseTarget::Pane,
                cx.entity().downgrade(),
                |_, _, _, _| panic!("disabled Zoom Pane command must not run"),
            )
        }
    }

    #[test]
    fn menu_items_should_use_restore_label_and_icon_when_zoomed() {
        let zoom_item = menu_items(true, true, CloseTarget::Pane)[2];

        assert_eq!(
            (zoom_item.label, zoom_item.icon),
            ("Restore Panes", "arrow.down.right.and.arrow.up.left")
        );
    }

    #[test]
    fn menu_items_should_show_command_w_for_close_pane() {
        let close_item = menu_items(false, true, CloseTarget::Pane)[3];

        assert_eq!(
            (close_item.label, close_item.shortcut),
            ("Close Pane", "⌘W")
        );
    }

    #[test]
    fn menu_items_should_show_command_shift_w_for_close_window() {
        let close_item = menu_items(false, true, CloseTarget::Window)[3];

        assert_eq!(
            (close_item.label, close_item.shortcut),
            ("Close Window", "⇧⌘W")
        );
    }

    #[gpui::test]
    fn disabled_zoom_row_should_not_dispatch_its_command(cx: &mut TestAppContext) {
        let (_owner, cx) = cx.add_window_view(|_, _| MenuTestOwner);
        let zoom_row = cx
            .debug_bounds("test-menu-row-toggle-zoom")
            .map(|bounds| bounds.center())
            .expect("the disabled Zoom Pane row was not rendered");

        cx.simulate_click(zoom_row, gpui::Modifiers::none());
    }
}
