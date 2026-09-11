//! Titlebar composition and sizing shared by its controls, Tab spacer, and resize edge.

use gpui::prelude::*;
use gpui::{AnyElement, App, Pixels, Rgba, Window, div, px};
use spaceterm_ui::{ButtonSize, ButtonTheme, ComboBoxTheme, CustomIconName, Icon, IconName};

use super::appearance::{ChromeAppearance, chrome};
use super::workspace_sidebar::SidebarLayout;

pub(super) const ICON_SIZE: f32 = 14.0;
pub(super) const TOGGLE_SIZE: ButtonSize = ButtonSize::Regular;

// Reserved native traffic-light region, with no additional leading gap.
const TRAFFIC_LIGHT_CLEARANCE: f32 = 78.0;
const CONTROL_TOP_INSET: f32 = 4.0;
const EXPANDED_MINIMUM_ACTION_GAP: f32 = 4.0;
const EXPANDED_TRAILING_GAP: f32 = 2.0;
const COLLAPSED_ACTION_GAP: f32 = 0.0;
const COLLAPSED_TRAILING_GAP: f32 = 4.0;
const SWITCHER_HORIZONTAL_PADDING: f32 = 10.0;
const SWITCHER_IDENTITY_GAP: f32 = 8.0;
const PIN_SIZE: f32 = 12.0;
const PIN_NAME_GAP: f32 = 5.0;
const NAME_TEXT_SIZE: f32 = 12.0;
// Name and optional pin share this budget. Outer spacing never consumes label room.
const NAME_AND_PIN_MAXIMUM_WIDTH: f32 = 58.0;

#[derive(Clone, Copy)]
pub(super) struct WorkspaceChromeLayout {
    pub(super) width: Pixels,
    sidebar_visible: bool,
}

impl WorkspaceChromeLayout {
    pub(super) fn resolve(
        sidebar: SidebarLayout,
        name: &str,
        pinned: bool,
        window: &Window,
        cx: &App,
    ) -> Self {
        Self {
            width: if sidebar.visible {
                sidebar.width
            } else {
                Self::collapsed_width(name, pinned, window, cx)
            },
            sidebar_visible: sidebar.visible,
        }
    }

    pub(super) fn collapsed_width(name: &str, pinned: bool, window: &Window, cx: &App) -> Pixels {
        let appearance = chrome(cx);
        let name_width = appearance.measure_emphasis(name, NAME_TEXT_SIZE, window);
        let pin_width = if pinned {
            appearance.spacing(PIN_SIZE + PIN_NAME_GAP)
        } else {
            px(0.0)
        };
        let identity_width =
            (name_width + pin_width).min(appearance.spacing(NAME_AND_PIN_MAXIMUM_WIDTH));
        let content_width = appearance.spacing(SWITCHER_HORIZONTAL_PADDING * 2.0)
            + appearance.spacing(ICON_SIZE + SWITCHER_IDENTITY_GAP)
            + identity_width;
        (px(TRAFFIC_LIGHT_CLEARANCE)
            + appearance.spacing(COLLAPSED_ACTION_GAP + COLLAPSED_TRAILING_GAP)
            + cx.global::<ButtonTheme>().icon_button_size(TOGGLE_SIZE)
            + cx.global::<ComboBoxTheme>()
                .custom_trigger_width(content_width))
        .ceil()
    }

    pub(super) fn render_controls(
        self,
        toggle: impl IntoElement,
        switcher: impl IntoElement,
        cx: &App,
    ) -> AnyElement {
        let appearance = chrome(cx);
        // The visible control owns clicks where it meets the sidebar resize target.
        let switcher = div()
            .min_w_0()
            .when(!self.sidebar_visible, |container| container.w_full())
            .occlude()
            .child(switcher);
        div()
            .absolute()
            .top(appearance.spacing(CONTROL_TOP_INSET))
            .left(px(TRAFFIC_LIGHT_CLEARANCE))
            .right(appearance.spacing(if self.sidebar_visible {
                EXPANDED_TRAILING_GAP
            } else {
                COLLAPSED_TRAILING_GAP
            }))
            .flex()
            .items_center()
            .gap(appearance.spacing(if self.sidebar_visible {
                EXPANDED_MINIMUM_ACTION_GAP
            } else {
                COLLAPSED_ACTION_GAP
            }))
            .child(toggle)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .child(switcher),
            )
            .into_any_element()
    }
}

pub(super) struct WorkspaceChromeIdentity {
    pub(super) name: String,
    pub(super) pinned: bool,
    pub(super) foreground: Rgba,
    pub(super) pin_color: Rgba,
}

impl WorkspaceChromeIdentity {
    pub(super) fn render(self, switcher_color: Rgba, appearance: &ChromeAppearance) -> AnyElement {
        let chip = div()
            .id("workspace-chip")
            .debug_selector(|| "workspace-chip".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(PIN_NAME_GAP))
            .font(appearance.emphasis.clone())
            .flex_1()
            .min_w_0()
            .when(self.pinned, |chip| {
                chip.child(
                    div()
                        .debug_selector(|| "workspace-chip-pin".to_owned())
                        .flex_shrink_0()
                        .child(Icon::new(
                            IconName::Pin,
                            appearance.spacing(PIN_SIZE),
                            self.pin_color,
                        )),
                )
            })
            .child(
                div()
                    .debug_selector(|| "workspace-chip-label".to_owned())
                    .min_w_0()
                    .truncate()
                    .text_size(appearance.text_size(NAME_TEXT_SIZE))
                    .text_color(self.foreground)
                    .child(self.name),
            );
        div()
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .px(appearance.spacing(SWITCHER_HORIZONTAL_PADDING))
            .gap(appearance.spacing(SWITCHER_IDENTITY_GAP))
            .child(
                div()
                    .debug_selector(|| "workspace-switcher-icon".to_owned())
                    .flex_shrink_0()
                    .child(Icon::custom(
                        CustomIconName::RectangleStack,
                        appearance.spacing(ICON_SIZE),
                        switcher_color,
                    )),
            )
            .child(chip)
            .into_any_element()
    }
}
