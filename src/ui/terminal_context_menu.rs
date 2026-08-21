use gpui::prelude::*;
use gpui::{AnyElement, Context, MouseButton, Pixels, WeakEntity, Window, div, px};

use crate::terminal::NativeContextActions;
use crate::theme::ACTIVE_THEME;

pub(crate) const TERMINAL_CONTEXT_MENU_WIDTH: f32 = 220.0;
pub(crate) const TERMINAL_CONTEXT_MENU_HEIGHT: f32 =
    MENU_ROW_HEIGHT * MENU_ITEM_COUNT as f32 + MENU_BORDER_SIZE * 2.0;

const MENU_ROW_HEIGHT: f32 = 28.0;
const MENU_ITEM_COUNT: usize = 3;
const MENU_BORDER_SIZE: f32 = 1.0;
const MENU_CORNER_RADIUS: f32 = 8.0;
const MENU_INNER_CORNER_RADIUS: f32 = MENU_CORNER_RADIUS - MENU_BORDER_SIZE;
const MENU_SHORTCUT_TEXT_SIZE: f32 = 11.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TerminalContextMenuCommand {
    Copy,
    OpenLink,
    QuickLook,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuItemSpec {
    command: TerminalContextMenuCommand,
    label: &'static str,
    shortcut: &'static str,
    enabled: bool,
}

pub(crate) fn render_terminal_context_menu<T, Handler, Dismiss>(
    left: Pixels,
    top: Pixels,
    actions: NativeContextActions,
    owner: WeakEntity<T>,
    handler: Handler,
    dismiss: Dismiss,
) -> AnyElement
where
    T: 'static,
    Handler: Fn(&mut T, TerminalContextMenuCommand, &mut Window, &mut Context<T>) + Clone + 'static,
    Dismiss: Fn(&mut T, &mut Window, &mut Context<T>) + Clone + 'static,
{
    let mut menu = div()
        .id("terminal-context-menu")
        .debug_selector(|| "terminal-context-menu".to_owned())
        .absolute()
        .left(left)
        .top(top)
        .w(px(TERMINAL_CONTEXT_MENU_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(MENU_CORNER_RADIUS))
        .border(px(MENU_BORDER_SIZE))
        .border_color(gpui_color(ACTIVE_THEME.border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .occlude();

    for (index, spec) in menu_items(actions).into_iter().enumerate() {
        let foreground = if spec.enabled {
            ACTIVE_THEME.text
        } else {
            ACTIVE_THEME.text_disabled
        };
        let command_name = spec.command.debug_name();
        let enabled = spec.enabled;
        let row = div()
            .id(spec.command as usize)
            .debug_selector(move || {
                format!(
                    "terminal-context-menu-row-{command_name}-{}",
                    if enabled { "enabled" } else { "disabled" }
                )
            })
            .h(px(MENU_ROW_HEIGHT))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_size(px(13.0))
            .text_color(gpui_color(foreground))
            .when(index == 0, |row| {
                row.rounded_t(px(MENU_INNER_CORNER_RADIUS))
            })
            .when(index == MENU_ITEM_COUNT - 1, |row| {
                row.rounded_b(px(MENU_INNER_CORNER_RADIUS))
            });
        let row = if spec.enabled {
            row.cursor_pointer()
                .hover(|row| row.bg(gpui_color(ACTIVE_THEME.element_hover)))
                // The Terminal Pane absorbs bubble-phase pointer events while
                // this menu is open, so dispatch the hovered row on release
                // during capture before that parent handler runs.
                .capture_any_mouse_up({
                    let owner = owner.clone();
                    let handler = handler.clone();
                    move |event, window, cx| {
                        if event.button != MouseButton::Left {
                            return;
                        }
                        let handler = handler.clone();
                        let _ = owner.update(cx, |owner, cx| {
                            handler(owner, spec.command, window, cx);
                        });
                        cx.stop_propagation();
                    }
                })
        } else {
            row
        };

        menu = menu.child(
            row.child(spec.label).child(div().flex_grow()).child(
                div()
                    .text_size(px(MENU_SHORTCUT_TEXT_SIZE))
                    .text_color(gpui_color(if spec.enabled {
                        ACTIVE_THEME.icon
                    } else {
                        ACTIVE_THEME.icon_disabled
                    }))
                    .child(spec.shortcut),
            ),
        );
    }

    let dismiss_layer = div()
        .absolute()
        .inset_0()
        // Dismiss outside the occluding menu before the Terminal Pane's
        // bubble-phase pointer handler can absorb the event.
        .capture_any_mouse_down(move |_, window, cx| {
            let dismiss = dismiss.clone();
            let _ = owner.update(cx, |owner, cx| dismiss(owner, window, cx));
            cx.stop_propagation();
        })
        .occlude();

    div()
        .id("terminal-context-menu-layer")
        .debug_selector(|| "terminal-context-menu-layer".to_owned())
        .absolute()
        .inset_0()
        .child(dismiss_layer)
        .child(menu)
        .into_any_element()
}

impl TerminalContextMenuCommand {
    const fn debug_name(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::OpenLink => "open-link",
            Self::QuickLook => "quick-look",
        }
    }
}

fn menu_items(actions: NativeContextActions) -> [MenuItemSpec; MENU_ITEM_COUNT] {
    [
        MenuItemSpec {
            command: TerminalContextMenuCommand::Copy,
            label: "Copy",
            shortcut: "⌘C",
            enabled: actions.copy,
        },
        MenuItemSpec {
            command: TerminalContextMenuCommand::OpenLink,
            label: "Open Link",
            shortcut: "",
            enabled: actions.open_link,
        },
        MenuItemSpec {
            command: TerminalContextMenuCommand::QuickLook,
            label: "Quick Look",
            shortcut: "",
            enabled: actions.quick_look,
        },
    ]
}

fn gpui_color(color: crate::theme::Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_rows_follow_authoritative_action_eligibility() {
        let items = menu_items(NativeContextActions {
            copy: true,
            open_link: false,
            quick_look: false,
        });

        assert!(items[0].enabled);
        assert!(!items[1].enabled);
        assert!(!items[2].enabled);
    }
}
