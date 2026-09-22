//! Titlebar composition and sizing shared by its controls, Tab spacer, and resize edge.

use gpui::prelude::*;
use gpui::{AnyElement, App, Pixels, Rgba, Window, div, px};
use spaceterm_ui::{ButtonSize, ButtonTheme, ComboBoxTheme, CustomIconName, Icon, IconName};

use super::appearance::{ChromeAppearance, chrome};
use super::chrome_icons::IconRole;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::workspace_sidebar::SidebarLayout;
use super::workspace_status::{WorkspaceStatusPaint, resolve as resolve_workspace_status};
use crate::appearance::Color;
use crate::domain::{DirectoryAvailability, RemoteConnectionPhase};

pub(super) const TOGGLE_SIZE: ButtonSize = ButtonSize::Regular;

// Reserved native traffic-light region, with no additional leading gap.
/// Space reserved for native window controls in app-owned top chrome.
pub(super) const TRAFFIC_LIGHT_CLEARANCE: f32 = 78.0;

/// Leading inset of the top-left chrome.
///
/// Fullscreen hides the native window controls, so the clearance guards nothing. The header still
/// keeps the frame's own edge air rather than running flush to the window edge.
pub(super) fn leading_clearance(fullscreen: bool, frame_space: Pixels) -> Pixels {
    if fullscreen {
        frame_space
    } else {
        px(TRAFFIC_LIGHT_CLEARANCE)
    }
}
const SWITCHER_HORIZONTAL_PADDING: f32 = 10.0;
const SWITCHER_IDENTITY_GAP: f32 = 8.0;
const PIN_NAME_GAP: f32 = 5.0;

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
        let name_width = appearance
            .typography
            .measure(TextRole::BodyEmphasis, name, window);
        let pin_size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let identity_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        let pin_width = if pinned {
            pin_size + appearance.spacing(PIN_NAME_GAP)
        } else {
            px(0.0)
        };
        // The collapsed switcher trigger stops at the Tab item maximum, so a long Workspace
        // name truncates where a long Tab title does. Outer spacing never consumes label room.
        let switcher_maximum = appearance.spacing(super::tab_manager::TAB_ITEM_MAXIMUM_WIDTH);
        let chrome_theme = cx.global::<ComboBoxTheme>();
        let content_maximum = switcher_maximum
            - chrome_theme.custom_trigger_width(px(0.0))
            - appearance.spacing(SWITCHER_HORIZONTAL_PADDING * 2.0)
            - identity_size
            - appearance.spacing(SWITCHER_IDENTITY_GAP);
        let identity_width = (name_width + pin_width).min(content_maximum.max(px(0.0)));
        let content_width = appearance.spacing(SWITCHER_HORIZONTAL_PADDING * 2.0)
            + identity_size
            + appearance.spacing(SWITCHER_IDENTITY_GAP)
            + identity_width;
        let edge_reserve = trailing_reserve(appearance, cx);
        (leading_clearance(window.is_fullscreen(), edge_reserve)
            + edge_reserve
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
        let window_edge =
            super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx).window_edge();
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
            // the middle of that height below the window's own edge rather than a fixed inset from
            // its upper edge. Tab chips centre in the same band, so identity and Tabs stay on one
            // line.
            .top(window_edge)
            .bottom_0()
            .left(leading_clearance(self.fullscreen, edge_reserve))
            // Whichever control ends the top-left chrome stops where a selected sidebar row's chip
            // stops, so the identity area and the list under it share one trailing edge.
            .right(edge_reserve)
            .flex()
            .items_center()
            // The toggle keeps the frame's edge air on its right as well, in every mode, so it
            // never hugs the switcher beside it.
            .gap(edge_reserve)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceChromeStatusHosts {
    normal: Color,
    hovered: Color,
}

impl WorkspaceChromeStatusHosts {
    pub(super) const fn new(normal: Color, hovered: Color) -> Self {
        Self { normal, hovered }
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
    pub(super) fn render(
        self,
        switcher_color: Rgba,
        appearance: &ChromeAppearance,
        status_hosts: WorkspaceChromeStatusHosts,
    ) -> AnyElement {
        let status = self.status(appearance, status_hosts);
        let badge = self.badge();
        let status_normal = status.map(|status| gpui_color(status.paint.normal));
        let status_hovered = status.map(|status| {
            gpui_color(if appearance.active {
                status.paint.hovered
            } else {
                status.paint.normal
            })
        });
        let status_selector = status.map(|status| status.selector);
        let status_group = "workspace-chrome-status";
        let chip = div()
            .id("workspace-chip")
            .debug_selector(|| "workspace-chip".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(PIN_NAME_GAP))
            .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
            .flex_1()
            .min_w_0()
            .when(self.pinned, |chip| {
                chip.child(
                    div()
                        .debug_selector(|| "workspace-chip-pin".to_owned())
                        .flex_shrink_0()
                        .child(Icon::new(
                            IconName::Pin,
                            appearance.icons.metrics(IconRole::Caption).glyph_size,
                            switcher_color,
                        )),
                )
            })
            .child(
                div()
                    .debug_selector(|| "workspace-chip-label".to_owned())
                    .min_w_0()
                    .truncate()
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
            .child(
                div()
                    .debug_selector(|| "workspace-switcher-icon".to_owned())
                    .flex_shrink_0()
                    .child(render_identity_icon(
                        status_selector.unwrap_or("workspace-switcher-identity-icon"),
                        status_group,
                        status_normal.unwrap_or(switcher_color),
                        status_hovered.unwrap_or(switcher_color),
                        appearance.icons.metrics(IconRole::Chrome).glyph_size,
                        badge.map(|badge| WorkspaceChromeBadgePaint {
                            icon: badge.icon,
                            selector: badge.selector,
                            normal: status_normal.unwrap_or(switcher_color),
                            hovered: status_hovered.unwrap_or(switcher_color),
                            normal_host: gpui_color(status_hosts.normal),
                            hovered_host: gpui_color(status_hosts.hovered),
                        }),
                        appearance.icons.metrics(IconRole::Caption).glyph_size,
                    )),
            )
            .child(chip)
            .into_any_element()
    }

    pub(super) fn accessibility_name(&self) -> String {
        let status = if matches!(self.availability, DirectoryAvailability::Unavailable { .. }) {
            Some("Directory unavailable")
        } else {
            self.remote_connection_phase.map(|phase| match phase {
                RemoteConnectionPhase::Connected => "Connected remote",
                RemoteConnectionPhase::Reconnecting => "Reconnecting",
                RemoteConnectionPhase::Disconnected => "Disconnected",
                RemoteConnectionPhase::Closing => "Closing",
                RemoteConnectionPhase::Failed => "Connection failed",
            })
        };
        status.map_or_else(
            || format!("Switch Workspace, {}", self.name),
            |status| format!("Switch Workspace, {}, {status}", self.name),
        )
    }

    fn badge(&self) -> Option<WorkspaceChromeBadge> {
        if matches!(self.availability, DirectoryAvailability::Unavailable { .. }) {
            return Some(WorkspaceChromeBadge {
                icon: IconName::TriangleAlert,
                selector: "workspace-switcher-status-unavailable-badge",
            });
        }
        self.remote_connection_phase.map(|phase| match phase {
            RemoteConnectionPhase::Connected => WorkspaceChromeBadge {
                icon: IconName::Globe,
                selector: "workspace-switcher-status-connected-badge",
            },
            RemoteConnectionPhase::Reconnecting => WorkspaceChromeBadge {
                icon: IconName::Info,
                selector: "workspace-switcher-status-reconnecting-badge",
            },
            RemoteConnectionPhase::Disconnected => WorkspaceChromeBadge {
                icon: IconName::TriangleAlert,
                selector: "workspace-switcher-status-disconnected-badge",
            },
            RemoteConnectionPhase::Closing => WorkspaceChromeBadge {
                icon: IconName::Pause,
                selector: "workspace-switcher-status-closing-badge",
            },
            RemoteConnectionPhase::Failed => WorkspaceChromeBadge {
                icon: IconName::TriangleAlert,
                selector: "workspace-switcher-status-failed-badge",
            },
        })
    }

    fn status(
        &self,
        appearance: &ChromeAppearance,
        hosts: WorkspaceChromeStatusHosts,
    ) -> Option<WorkspaceChromeStatusPaint> {
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
            paint: resolve_workspace_status(proposed, hosts.normal, hosts.hovered, 4.5),
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
    badge: Option<WorkspaceChromeBadgePaint>,
    badge_glyph_size: Pixels,
) -> AnyElement {
    let glyph = |tint: Rgba| Icon::custom(CustomIconName::RectangleStack, size, tint);
    let badge_size = badge_glyph_size + px(2.0);
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
        .when_some(badge, |icon, badge| {
            icon.child(
                div()
                    .debug_selector(move || badge.selector.to_owned())
                    .absolute()
                    .right(px(-4.0))
                    .bottom(px(-3.0))
                    .size(badge_size)
                    .rounded(badge_size / 2.0)
                    .border(px(super::chrome_geometry::HAIRLINE))
                    .border_color(badge.normal_host)
                    .text_color(badge.normal)
                    .group_hover(group, move |style| {
                        style
                            .border_color(badge.hovered_host)
                            .text_color(badge.hovered)
                    })
                    .child(Icon::inherited(badge.icon, badge_glyph_size)),
            )
        })
        .into_any_element()
}

#[derive(Clone, Copy, Debug)]
struct WorkspaceChromeBadge {
    icon: IconName,
    selector: &'static str,
}

#[derive(Clone, Copy)]
struct WorkspaceChromeBadgePaint {
    icon: IconName,
    selector: &'static str,
    normal: Rgba,
    hovered: Rgba,
    normal_host: Rgba,
    hovered_host: Rgba,
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
    use crate::appearance::{
        Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
        ChromeColorOverrides, ChromeScheme, CompositionCapabilities, CustomScheme, SchemeCatalog,
        SchemeId, SchemeMetadata, SystemAppearance, builtin_chrome_base,
    };

    #[test]
    fn fullscreen_header_should_keep_edge_air_without_traffic_lights() {
        assert_eq!(leading_clearance(true, px(8.0)), px(8.0));
        assert_eq!(
            leading_clearance(false, px(8.0)),
            px(TRAFFIC_LIGHT_CLEARANCE)
        );
    }

    #[test]
    fn unhealthy_identity_status_should_be_readable_on_each_title_bar_host() {
        for colors in [
            builtin_chrome_base(Appearance::Dark),
            builtin_chrome_base(Appearance::Light),
        ] {
            let appearance = ChromeAppearance {
                colors,
                ..ChromeAppearance::default()
            };
            for host in [
                appearance.colors.title_bar_background,
                appearance.colors.title_bar_inactive_background,
            ] {
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
                        .status(&appearance, WorkspaceChromeStatusHosts::new(host, host))
                        .expect("unhealthy status paint");
                    assert!(status.paint.normal.contrast_ratio(host) >= 4.5);
                    assert!(status.paint.hovered.contrast_ratio(host) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn unhealthy_identity_status_should_read_on_distinct_painted_chip_hosts() {
        let scheme_id = SchemeId::new("test.workspace-status-hosts").unwrap();
        let custom = CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: scheme_id.clone(),
            name: "Workspace Status Hosts".to_owned(),
            appearance: Appearance::Light,
            metadata: SchemeMetadata::default(),
            colors: ChromeColorOverrides {
                background: Some(Color::rgb(0xffffff)),
                title_bar_background: Some(Color::rgb(0xffffff)),
                tab_active_background: Some(Color::rgb(0x101010)),
                tab_active_hover_background: Some(Color::rgb(0x101010)),
                warning: Some(Color::rgb(0xffffff)),
                ..ChromeColorOverrides::default()
            },
        }));
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..AppearancePreferences::default()
        };
        preferences.chrome.schemes.light = scheme_id;
        preferences.background.transparency = 1.0;
        let resolved = SchemeCatalog::from_custom_schemes(&[custom])
            .unwrap()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::available(Appearance::Light)
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .unwrap();
        let appearance = ChromeAppearance::prepare(&resolved.chrome);
        let surface = super::super::tab_manager::active_tab_surface(&appearance, true);
        let title_bar_host =
            appearance.control_host_background(spaceterm_ui::ControlHost::TitleBar);
        let painted_normal = surface
            .fill
            .map_or(title_bar_host, |fill| fill.source_over(title_bar_host));
        let painted_hovered = surface
            .hover_fill
            .or(surface.fill)
            .map_or(title_bar_host, |fill| fill.source_over(title_bar_host));
        let identity = WorkspaceChromeIdentity {
            name: "Workspace".to_owned(),
            pinned: false,
            availability: DirectoryAvailability::Unavailable {
                reason: "unavailable".to_owned(),
            },
            remote_connection_phase: None,
        };

        let status = identity
            .status(
                &appearance,
                WorkspaceChromeStatusHosts::new(painted_normal, painted_hovered),
            )
            .expect("unhealthy status paint");

        assert!(status.paint.normal.contrast_ratio(painted_normal) >= 4.5);
        assert!(status.paint.hovered.contrast_ratio(painted_hovered) >= 4.5);
    }

    #[test]
    fn collapsed_workspace_identity_carries_connection_and_availability_badges() {
        let identity = |availability, remote_connection_phase| WorkspaceChromeIdentity {
            name: "Workspace".to_owned(),
            pinned: false,
            availability,
            remote_connection_phase,
        };
        let cases = [
            (
                RemoteConnectionPhase::Connected,
                IconName::Globe,
                "Connected remote",
            ),
            (
                RemoteConnectionPhase::Reconnecting,
                IconName::Info,
                "Reconnecting",
            ),
            (
                RemoteConnectionPhase::Disconnected,
                IconName::TriangleAlert,
                "Disconnected",
            ),
            (RemoteConnectionPhase::Closing, IconName::Pause, "Closing"),
            (
                RemoteConnectionPhase::Failed,
                IconName::TriangleAlert,
                "Connection failed",
            ),
        ];
        for (phase, icon, status) in cases {
            let identity = identity(DirectoryAvailability::Available, Some(phase));
            let badge = identity.badge().expect("remote Workspace badge");
            assert_eq!(
                std::mem::discriminant(&badge.icon),
                std::mem::discriminant(&icon)
            );
            assert_eq!(
                identity.accessibility_name(),
                format!("Switch Workspace, Workspace, {status}")
            );
        }

        let local = identity(DirectoryAvailability::Available, None);
        assert!(local.badge().is_none());
        assert_eq!(local.accessibility_name(), "Switch Workspace, Workspace");
        let unavailable = identity(
            DirectoryAvailability::Unavailable {
                reason: "unavailable".to_owned(),
            },
            None,
        );
        assert_eq!(
            std::mem::discriminant(&unavailable.badge().expect("unavailable badge").icon),
            std::mem::discriminant(&IconName::TriangleAlert)
        );
        assert_eq!(
            unavailable.accessibility_name(),
            "Switch Workspace, Workspace, Directory unavailable"
        );
    }
}
