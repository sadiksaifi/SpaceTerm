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
use crate::domain::RemoteConnectionPhase;

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
/// Diameter of the collapsed identity's status dot.
const STATUS_DOT_SIZE: f32 = 6.0;

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
        identity: &WorkspaceChromeIdentity,
        window: &Window,
        cx: &App,
    ) -> Self {
        Self {
            width: if sidebar.visible {
                sidebar.width
            } else {
                Self::collapsed_width(identity, window, cx)
            },
            sidebar_visible: sidebar.visible,
            fullscreen: window.is_fullscreen(),
        }
    }

    pub(super) fn collapsed_width(
        identity: &WorkspaceChromeIdentity,
        window: &Window,
        cx: &App,
    ) -> Pixels {
        let appearance = chrome(cx);
        let name_width =
            appearance
                .typography
                .measure(TextRole::BodyEmphasis, &identity.name, window);
        let pin_size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let identity_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        let pin_width = if identity.pinned {
            pin_size + appearance.spacing(PIN_NAME_GAP)
        } else {
            px(0.0)
        };
        // The status dot never gives up its room: a long name truncates before it.
        let status_width = if identity.status.is_some() {
            appearance.spacing(SWITCHER_IDENTITY_GAP + STATUS_DOT_SIZE)
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
            - appearance.spacing(SWITCHER_IDENTITY_GAP)
            - status_width;
        let identity_width = (name_width + pin_width).min(content_maximum.max(px(0.0)));
        let content_width = appearance.spacing(SWITCHER_HORIZONTAL_PADDING * 2.0)
            + identity_size
            + appearance.spacing(SWITCHER_IDENTITY_GAP)
            + identity_width
            + status_width;
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

/// The one status the collapsed Workspace identity presents.
///
/// An unavailable directory takes precedence over the Remote connection, because no Terminal can
/// start in the Workspace until it is resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceChromeStatus {
    Unavailable,
    Connected,
    Reconnecting,
    Disconnected,
    Closing,
    Failed,
}

impl WorkspaceChromeStatus {
    pub(super) fn resolve(
        available: bool,
        remote_connection_phase: Option<RemoteConnectionPhase>,
    ) -> Option<Self> {
        if !available {
            return Some(Self::Unavailable);
        }
        remote_connection_phase.map(|phase| match phase {
            RemoteConnectionPhase::Connected => Self::Connected,
            RemoteConnectionPhase::Reconnecting => Self::Reconnecting,
            RemoteConnectionPhase::Disconnected => Self::Disconnected,
            RemoteConnectionPhase::Closing => Self::Closing,
            RemoteConnectionPhase::Failed => Self::Failed,
        })
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Directory unavailable",
            Self::Connected => "Connected remote",
            Self::Reconnecting => "Reconnecting",
            Self::Disconnected => "Disconnected",
            Self::Closing => "Closing",
            Self::Failed => "Connection failed",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Unavailable => "workspace-switcher-status-unavailable",
            Self::Connected => "workspace-switcher-status-connected",
            Self::Reconnecting => "workspace-switcher-status-reconnecting",
            Self::Disconnected => "workspace-switcher-status-disconnected",
            Self::Closing => "workspace-switcher-status-closing",
            Self::Failed => "workspace-switcher-status-failed",
        }
    }

    fn color(self, colors: &crate::appearance::ChromeColors) -> Color {
        match self {
            Self::Unavailable => colors.warning,
            Self::Connected => colors.success,
            Self::Reconnecting => colors.info,
            Self::Disconnected | Self::Closing => colors.icon_muted,
            Self::Failed => colors.error,
        }
    }

    /// The dot's paint on the chip's resting and hovered surfaces.
    ///
    /// The dot is a graphical object, so it meets graphical-object contrast against each surface.
    fn paint(
        self,
        appearance: &ChromeAppearance,
        hosts: WorkspaceChromeStatusHosts,
    ) -> WorkspaceStatusPaint {
        resolve_workspace_status(
            self.color(&appearance.colors),
            hosts.normal,
            hosts.hovered,
            3.0,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceChromeIdentity {
    pub(super) name: String,
    pub(super) pinned: bool,
    pub(super) status: Option<WorkspaceChromeStatus>,
}

impl WorkspaceChromeIdentity {
    /// Renders the collapsed identity: the Workspace glyph, its name, and a trailing status dot.
    ///
    /// The glyph and name keep the title bar's own paint in every state. Only the dot carries the
    /// status, so the identity reads the same for a Local Workspace and a healthy Remote one.
    pub(super) fn render(
        self,
        switcher_color: Rgba,
        appearance: &ChromeAppearance,
        status_hosts: WorkspaceChromeStatusHosts,
    ) -> AnyElement {
        let status_group = "workspace-chrome-status";
        let active = appearance.active;
        let status_dot = self.status.map(|status| {
            let paint = status.paint(appearance, status_hosts);
            let normal = gpui_color(paint.normal);
            let hovered = gpui_color(if active { paint.hovered } else { paint.normal });
            let size = appearance.spacing(STATUS_DOT_SIZE);
            div()
                .debug_selector(move || status.selector().to_owned())
                .flex_none()
                .size(size)
                .rounded(size / 2.0)
                .bg(normal)
                .group_hover(status_group, move |style| style.bg(hovered))
        });
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
                    .text_color(switcher_color)
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
                    .child(Icon::custom(
                        CustomIconName::RectangleStack,
                        appearance.icons.metrics(IconRole::Chrome).glyph_size,
                        switcher_color,
                    )),
            )
            .child(chip)
            .children(status_dot)
            .into_any_element()
    }

    pub(super) fn accessibility_name(&self) -> String {
        self.status.map_or_else(
            || format!("Switch Workspace, {}", self.name),
            |status| format!("Switch Workspace, {}, {}", self.name, status.label()),
        )
    }
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

    const STATUSES: [WorkspaceChromeStatus; 6] = [
        WorkspaceChromeStatus::Unavailable,
        WorkspaceChromeStatus::Connected,
        WorkspaceChromeStatus::Reconnecting,
        WorkspaceChromeStatus::Disconnected,
        WorkspaceChromeStatus::Closing,
        WorkspaceChromeStatus::Failed,
    ];

    #[test]
    fn status_dot_should_be_readable_on_each_title_bar_host() {
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
                for status in STATUSES {
                    let paint =
                        status.paint(&appearance, WorkspaceChromeStatusHosts::new(host, host));
                    assert!(paint.normal.contrast_ratio(host) >= 3.0, "{status:?}");
                    assert!(paint.hovered.contrast_ratio(host) >= 3.0, "{status:?}");
                }
            }
        }
    }

    #[test]
    fn status_dot_should_read_on_distinct_painted_chip_hosts() {
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

        let paint = WorkspaceChromeStatus::Unavailable.paint(
            &appearance,
            WorkspaceChromeStatusHosts::new(painted_normal, painted_hovered),
        );

        assert!(paint.normal.contrast_ratio(painted_normal) >= 3.0);
        assert!(paint.hovered.contrast_ratio(painted_hovered) >= 3.0);
    }

    #[test]
    fn collapsed_identity_should_present_one_status_with_unavailable_first() {
        assert_eq!(WorkspaceChromeStatus::resolve(true, None), None);
        assert_eq!(
            WorkspaceChromeStatus::resolve(false, Some(RemoteConnectionPhase::Connected)),
            Some(WorkspaceChromeStatus::Unavailable)
        );
        for (phase, status) in [
            (
                RemoteConnectionPhase::Connected,
                WorkspaceChromeStatus::Connected,
            ),
            (
                RemoteConnectionPhase::Reconnecting,
                WorkspaceChromeStatus::Reconnecting,
            ),
            (
                RemoteConnectionPhase::Disconnected,
                WorkspaceChromeStatus::Disconnected,
            ),
            (
                RemoteConnectionPhase::Closing,
                WorkspaceChromeStatus::Closing,
            ),
            (RemoteConnectionPhase::Failed, WorkspaceChromeStatus::Failed),
        ] {
            assert_eq!(
                WorkspaceChromeStatus::resolve(true, Some(phase)),
                Some(status)
            );
        }

        let identity = |status| WorkspaceChromeIdentity {
            name: "Workspace".to_owned(),
            pinned: false,
            status,
        };
        assert_eq!(
            identity(None).accessibility_name(),
            "Switch Workspace, Workspace"
        );
        for (status, label) in [
            (WorkspaceChromeStatus::Unavailable, "Directory unavailable"),
            (WorkspaceChromeStatus::Connected, "Connected remote"),
            (WorkspaceChromeStatus::Reconnecting, "Reconnecting"),
            (WorkspaceChromeStatus::Disconnected, "Disconnected"),
            (WorkspaceChromeStatus::Closing, "Closing"),
            (WorkspaceChromeStatus::Failed, "Connection failed"),
        ] {
            assert_eq!(
                identity(Some(status)).accessibility_name(),
                format!("Switch Workspace, Workspace, {label}")
            );
        }
    }
}
