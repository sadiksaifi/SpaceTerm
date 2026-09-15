//! Titlebar composition and sizing shared by its controls, Tab spacer, and resize edge.

use gpui::prelude::*;
use gpui::{AnyElement, App, Pixels, Rgba, Window, div, px};
use spaceterm_ui::{ButtonSize, ButtonTheme, ComboBoxTheme, CustomIconName, Icon, IconName};

use super::appearance::{ChromeAppearance, chrome};
use super::selection_chip::{CHIP_RADIUS, ChipPaint, ChipShape, SelectionChip};
use super::workspace_sidebar::SidebarLayout;
use super::workspace_status::{WorkspaceStatusPaint, resolve as resolve_workspace_status};
use crate::appearance::Color;
use crate::domain::{DirectoryAvailability, RemoteConnectionPhase};

pub(super) const ICON_SIZE: f32 = 14.0;
pub(super) const TOGGLE_SIZE: ButtonSize = ButtonSize::Regular;

// Reserved native traffic-light region, with no additional leading gap.
/// Space reserved for native window controls in app-owned top chrome.
pub(super) const TRAFFIC_LIGHT_CLEARANCE: f32 = 78.0;

/// Leading inset of the top-left chrome.
///
/// macOS hides the traffic lights while fullscreen, so the clearance guards nothing. The header
/// still keeps the frame's own edge air rather than running flush to the window edge.
pub(super) fn leading_clearance(fullscreen: bool, frame_space: Pixels) -> Pixels {
    if fullscreen {
        frame_space
    } else {
        px(TRAFFIC_LIGHT_CLEARANCE)
    }
}
const EXPANDED_MINIMUM_ACTION_GAP: f32 = 4.0;
const COLLAPSED_ACTION_GAP: f32 = 0.0;
const SWITCHER_HORIZONTAL_PADDING: f32 = 10.0;
const SWITCHER_IDENTITY_GAP: f32 = 8.0;
const PIN_SIZE: f32 = 12.0;
const PIN_NAME_GAP: f32 = 5.0;
const NAME_TEXT_SIZE: f32 = 12.0;
// Name and optional pin share this budget. Outer spacing never consumes label room.
const NAME_AND_PIN_MAXIMUM_WIDTH: f32 = 58.0;

/// The air the top-left chrome keeps at its trailing edge.
///
/// It is the Workspace frame's one measurement, the same margin a selected sidebar row's chip keeps
/// beside it, so the identity area and the list under it stop on one vertical.
fn trailing_reserve(appearance: &ChromeAppearance, cx: &App) -> Pixels {
    super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx).space()
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceChromeLayout {
    pub(super) width: Pixels,
    sidebar_visible: bool,
    fullscreen: bool,
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
            fullscreen: window.is_fullscreen(),
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
        let edge_reserve = trailing_reserve(appearance, cx);
        (leading_clearance(window.is_fullscreen(), edge_reserve)
            + appearance.spacing(COLLAPSED_ACTION_GAP)
            + edge_reserve
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
        let edge_reserve = trailing_reserve(appearance, cx);
        // Keep drag-region occlusion outside the tooltip target so the toggle cannot
        // block its own help. Its button preserves hover within this control container.
        let toggle = div().flex_none().block_mouse_except_scroll().child(toggle);
        // The visible control owns clicks where it meets the sidebar resize target.
        let switcher = div()
            .min_w_0()
            .when(!self.sidebar_visible, |container| container.w_full())
            .occlude()
            .child(switcher);
        div()
            .absolute()
            // The chrome now carries the frame's top space in its own height, so its controls ride
            // the middle of that height rather than a fixed inset from its upper edge. Tabs centre
            // in the same strip, so identity and Tabs stay on one line.
            .top_0()
            .bottom_0()
            .left(leading_clearance(self.fullscreen, edge_reserve))
            // Whichever control ends the top-left chrome stops where a selected sidebar row's chip
            // stops, so the identity area and the list under it share one trailing edge.
            .right(edge_reserve)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceChromeIdentity {
    pub(super) name: String,
    pub(super) pinned: bool,
    pub(super) availability: DirectoryAvailability,
    pub(super) remote_connection_phase: Option<RemoteConnectionPhase>,
}

impl WorkspaceChromeIdentity {
    pub(super) fn render(self, switcher_color: Rgba, appearance: &ChromeAppearance) -> AnyElement {
        let status = self.status(appearance);
        let status_normal = status.map(|status| gpui_color(status.paint.normal));
        let status_hovered = status.map(|status| gpui_color(status.paint.hovered));
        let status_selector = status.map(|status| status.selector);
        let status_group = "workspace-chrome-status";
        let surface = SelectionChip::new(
            ChipShape::symmetric(px(0.0), px(0.0), appearance.spacing(CHIP_RADIUS)),
            ChipPaint {
                fill: Some(appearance.colors.row_selected_background),
                rim: Some(appearance.colors.row_selected_border),
                hover_fill: Some(appearance.colors.row_selected_hover_background),
                hover_rim: Some(appearance.colors.row_selected_hover_border),
            }
            .raised(appearance),
        );
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
                            switcher_color,
                        )),
                )
            })
            .child(
                div()
                    .debug_selector(|| "workspace-chip-label".to_owned())
                    .min_w_0()
                    .truncate()
                    .text_size(appearance.text_size(NAME_TEXT_SIZE))
                    .text_color(status_normal.unwrap_or(switcher_color))
                    .group_hover(status_group, move |style| {
                        style.text_color(status_hovered.unwrap_or(switcher_color))
                    })
                    .child(self.name),
            );
        div()
            .relative()
            .flex()
            .items_center()
            .w_full()
            .h_full()
            .min_w_0()
            .px(appearance.spacing(SWITCHER_HORIZONTAL_PADDING))
            .gap(appearance.spacing(SWITCHER_IDENTITY_GAP))
            .group(status_group)
            .child(surface.render("workspace-switcher-chip".to_owned(), status_group))
            .child(
                div()
                    .debug_selector(|| "workspace-switcher-icon".to_owned())
                    .flex_shrink_0()
                    .child(render_identity_icon(
                        status_selector.unwrap_or("workspace-switcher-identity-icon"),
                        status_group,
                        status_normal.unwrap_or(switcher_color),
                        status_hovered.unwrap_or(switcher_color),
                        appearance.spacing(ICON_SIZE),
                    )),
            )
            .child(chip)
            .into_any_element()
    }

    fn status(&self, appearance: &ChromeAppearance) -> Option<WorkspaceChromeStatusPaint> {
        let (proposed, selector) =
            if matches!(self.availability, DirectoryAvailability::Unavailable { .. }) {
                (
                    appearance.colors.warning,
                    "workspace-switcher-status-unavailable",
                )
            } else {
                match self.remote_connection_phase? {
                    RemoteConnectionPhase::Connected => return None,
                    RemoteConnectionPhase::Reconnecting => (
                        appearance.colors.info,
                        "workspace-switcher-status-reconnecting",
                    ),
                    RemoteConnectionPhase::Disconnected => (
                        appearance.colors.icon_muted,
                        "workspace-switcher-status-disconnected",
                    ),
                    RemoteConnectionPhase::Closing => (
                        appearance.colors.icon_muted,
                        "workspace-switcher-status-closing",
                    ),
                    RemoteConnectionPhase::Failed => {
                        (appearance.colors.error, "workspace-switcher-status-failed")
                    }
                }
            };
        Some(WorkspaceChromeStatusPaint {
            selector,
            paint: resolve_workspace_status(
                proposed,
                appearance
                    .colors
                    .row_selected_background
                    .source_over(appearance.colors.title_bar_background),
                appearance
                    .colors
                    .row_selected_hover_background
                    .source_over(appearance.colors.title_bar_background),
                4.5,
            ),
        })
    }
}

/// The Workspace identity glyph, which the collapsed chip and the expanded chooser both present.
///
/// A bundled vector paints from its own resolved tint rather than from an inherited text color, so
/// the resting and hovered paints are drawn as two stacked glyphs and the hover swaps which one is
/// visible. Both are the one icon: the chip keeps the same identity in either sidebar state.
fn render_identity_icon(
    selector: &'static str,
    group: &'static str,
    normal: Rgba,
    hovered: Rgba,
    size: Pixels,
) -> AnyElement {
    let glyph = |tint: Rgba| Icon::custom(CustomIconName::RectangleStack, size, tint);
    div()
        .debug_selector(move || selector.to_owned())
        .relative()
        .flex()
        .flex_none()
        .size(size)
        .child(glyph(normal))
        .when(hovered != normal, |icon| {
            icon.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .opacity(0.0)
                    .group_hover(group, |style| style.opacity(1.0))
                    .child(glyph(hovered)),
            )
        })
        .into_any_element()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkspaceChromeStatusPaint {
    selector: &'static str,
    paint: WorkspaceStatusPaint,
}

fn gpui_color(color: Color) -> Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};

    #[test]
    fn fullscreen_header_should_keep_edge_air_without_traffic_lights() {
        assert_eq!(leading_clearance(true, px(8.0)), px(8.0));
        assert_eq!(
            leading_clearance(false, px(8.0)),
            px(TRAFFIC_LIGHT_CLEARANCE)
        );
    }

    #[test]
    fn unhealthy_identity_status_should_be_readable_on_resting_and_hovered_triggers() {
        for colors in [
            builtin_chrome_base(Appearance::Dark),
            builtin_chrome_base(Appearance::Light),
        ] {
            let appearance = ChromeAppearance {
                colors,
                ..ChromeAppearance::default()
            };
            let normal_background = appearance
                .colors
                .row_selected_background
                .source_over(appearance.colors.title_bar_background);
            let hovered_background = appearance
                .colors
                .row_selected_hover_background
                .source_over(appearance.colors.title_bar_background);
            for (availability, remote_connection_phase) in [
                (
                    DirectoryAvailability::Unavailable {
                        reason: "unavailable".to_owned(),
                    },
                    None,
                ),
                (
                    DirectoryAvailability::Available,
                    Some(RemoteConnectionPhase::Reconnecting),
                ),
                (
                    DirectoryAvailability::Available,
                    Some(RemoteConnectionPhase::Disconnected),
                ),
                (
                    DirectoryAvailability::Available,
                    Some(RemoteConnectionPhase::Failed),
                ),
            ] {
                let identity = WorkspaceChromeIdentity {
                    name: "Workspace".to_owned(),
                    pinned: false,
                    availability,
                    remote_connection_phase,
                };
                let status = identity
                    .status(&appearance)
                    .expect("unhealthy status paint");
                assert!(status.paint.normal.contrast_ratio(normal_background) >= 4.5);
                assert!(status.paint.hovered.contrast_ratio(hovered_background) >= 4.5);
            }
        }
    }
}
