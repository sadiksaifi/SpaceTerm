//! Titlebar composition and sizing shared by its controls, Tab spacer, and resize edge.

use crate::ui::appearance::gpui_color;
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

/// Keep the frame's edge air when the host has no visible native controls.
pub(super) fn leading_clearance(fullscreen: bool, frame_space: Pixels, cx: &App) -> Pixels {
    cx.try_global::<crate::platform::window_frame::WindowFrameGeometry>()
        .and_then(|geometry| geometry.leading_titlebar_clearance(fullscreen))
        .unwrap_or(frame_space)
}
const SWITCHER_HORIZONTAL_PADDING: f32 = 10.0;
const SWITCHER_IDENTITY_GAP: f32 = 8.0;
const PIN_NAME_GAP: f32 = 5.0;
const WORKTREE_SEPARATOR_GAP: f32 = 4.0;
/// Diameter of the collapsed identity's status dot.
const STATUS_DOT_SIZE: f32 = 6.0;

/// The width of the collapsed identity's status mark: a dot, or a caption glyph under
/// Differentiate Without Color.
fn status_mark_size(appearance: &ChromeAppearance) -> Pixels {
    if appearance.capabilities.differentiate_without_color {
        appearance.icons.metrics(IconRole::Caption).glyph_size
    } else {
        appearance.spacing(STATUS_DOT_SIZE)
    }
}

fn trailing_reserve(appearance: &ChromeAppearance, cx: &App) -> Pixels {
    super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx).space()
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceChromeLayout {
    pub(super) width: Pixels,
    sidebar_visible: bool,
    fullscreen: bool,
    client_controls_width: Pixels,
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
            client_controls_width: spaceterm_ui::ClientWindowControls::width(
                spaceterm_ui::WindowControlSide::Left,
                window,
                cx,
            ),
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
        // The Active Worktree follows the name after a chevron, as a path does.
        let worktree_width = identity.worktree.as_ref().map_or(px(0.0), |worktree| {
            let worktree_width = appearance
                .typography
                .measure(TextRole::Body, worktree, window);
            spaceterm_ui::reserve_measured_width(
                worktree_width,
                [
                    appearance.spacing(PIN_NAME_GAP),
                    pin_size,
                    appearance.spacing(WORKTREE_SEPARATOR_GAP),
                ],
                window,
            )
        });
        let identity_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        let pin_width = if identity.pinned {
            spaceterm_ui::reserve_measured_width(
                px(0.0),
                [pin_size, appearance.spacing(PIN_NAME_GAP)],
                window,
            )
        } else {
            px(0.0)
        };
        // The status mark never gives up its room: a long name truncates before it.
        let status_width = if identity.status.is_some() {
            spaceterm_ui::reserve_measured_width(
                px(0.0),
                [
                    appearance.spacing(SWITCHER_IDENTITY_GAP),
                    status_mark_size(appearance),
                ],
                window,
            )
        } else {
            px(0.0)
        };
        // The collapsed switcher trigger stops at the Tab item maximum, so a long Workspace
        // name truncates where a long Tab title does. Outer spacing never consumes label room.
        let switcher_maximum =
            window.pixel_snap(appearance.spacing(super::tab_manager::TAB_ITEM_MAXIMUM_WIDTH));
        let chrome_theme = cx.global::<ComboBoxTheme>();
        let content_fixed = spaceterm_ui::reserve_measured_width(
            px(0.0),
            [
                appearance.spacing(SWITCHER_HORIZONTAL_PADDING),
                appearance.spacing(SWITCHER_HORIZONTAL_PADDING),
                identity_size,
                appearance.spacing(SWITCHER_IDENTITY_GAP),
                status_width,
            ],
            window,
        );
        let content_maximum =
            switcher_maximum - chrome_theme.custom_trigger_width(px(0.0), window) - content_fixed;
        let identity_width =
            spaceterm_ui::reserve_measured_width(name_width, [pin_width, worktree_width], window)
                .min(content_maximum.max(px(0.0)));
        let content_width =
            spaceterm_ui::reserve_measured_width(px(0.0), [identity_width, content_fixed], window);
        let edge_reserve = trailing_reserve(appearance, cx);
        let client_controls_width = spaceterm_ui::ClientWindowControls::width(
            spaceterm_ui::WindowControlSide::Left,
            window,
            cx,
        );
        let leading_width = if client_controls_width > px(0.0) {
            // The group width already includes the native window margin. Reserve its gap to
            // the toggle separately from the toggle's gap to the switcher.
            spaceterm_ui::reserve_measured_width(
                px(0.0),
                [client_controls_width, edge_reserve],
                window,
            )
        } else {
            leading_clearance(window.is_fullscreen(), edge_reserve, cx)
        };
        spaceterm_ui::reserve_measured_width(
            px(0.0),
            [
                leading_width,
                edge_reserve,
                edge_reserve,
                cx.global::<ButtonTheme>().icon_button_size(TOGGLE_SIZE),
                chrome_theme.custom_trigger_width(content_width, window),
            ],
            window,
        )
    }

    pub(super) fn render_controls(
        self,
        toggle: impl IntoElement,
        switcher: impl IntoElement,
        leading_controls: impl IntoElement,
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
            // Controls center in the chrome's height below the window edge, the same band Tab chips
            // use.
            .top(window_edge)
            .bottom_0()
            .left(if self.client_controls_width > px(0.0) {
                px(spaceterm_ui::DesktopWindowStyle::current(cx)
                    .control_metrics()
                    .edge_margin)
            } else {
                leading_clearance(self.fullscreen, edge_reserve, cx)
            })
            // Whichever control ends the top-left chrome stops where a selected sidebar row's chip
            // stops, so the identity area and the list under it share one trailing edge.
            .right(edge_reserve)
            .flex()
            .items_center()
            // The toggle keeps the frame's edge air on its right as well, in every mode, so it
            // never hugs the switcher beside it.
            .gap(edge_reserve)
            .when(self.client_controls_width > px(0.0), |controls| {
                controls.child(leading_controls)
            })
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

/// The one status the collapsed Workspace identity presents. An unavailable directory takes
/// precedence over the Remote connection because no Terminal Session can start until it resolves.
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
    #[cfg(any(test, feature = "developer-tools"))]
    pub(super) const ALL: [Self; 6] = [
        Self::Unavailable,
        Self::Connected,
        Self::Reconnecting,
        Self::Disconnected,
        Self::Closing,
        Self::Failed,
    ];

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

    pub(super) const fn label(self) -> &'static str {
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

    /// The glyph that replaces the dot under Differentiate Without Color.
    const fn glyph(self) -> IconName {
        match self {
            Self::Unavailable => IconName::TriangleAlert,
            Self::Connected => IconName::Check,
            Self::Reconnecting => IconName::RotateCw,
            Self::Disconnected | Self::Closing => IconName::Minus,
            Self::Failed => IconName::CircleAlert,
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

    /// The status mark: a dot, or the status's own glyph under Differentiate Without Color.
    pub(super) fn mark(
        self,
        appearance: &ChromeAppearance,
        hosts: WorkspaceChromeStatusHosts,
        hover_group: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        let paint = self.paint(appearance, hosts);
        let normal = gpui_color(paint.normal);
        let hovered = gpui_color(if appearance.active {
            paint.hovered
        } else {
            paint.normal
        });
        let size = status_mark_size(appearance);
        let mark = div()
            .id(self.selector())
            .debug_selector(move || self.selector().to_owned())
            .flex_none()
            .size(size);
        if appearance.capabilities.differentiate_without_color {
            mark.text_color(normal)
                .group_hover(hover_group, move |style| style.text_color(hovered))
                .child(
                    div()
                        .debug_selector(|| "workspace-switcher-status-glyph".to_owned())
                        .child(Icon::inherited(self.glyph(), size)),
                )
        } else {
            mark.rounded(size / 2.0)
                .bg(normal)
                .group_hover(hover_group, move |style| style.bg(hovered))
        }
    }

    /// The dot's paint on the chip's resting and hovered surfaces, at graphical-object contrast.
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
    /// The Active Worktree's folder name, in a Workspace that lists Worktrees.
    pub(super) worktree: Option<String>,
    pub(super) pinned: bool,
    pub(super) status: Option<WorkspaceChromeStatus>,
}

impl WorkspaceChromeIdentity {
    /// Renders the collapsed identity: the Workspace glyph, its name, and a trailing status mark.
    pub(super) fn render(
        self,
        switcher_color: Rgba,
        appearance: &ChromeAppearance,
        status_hosts: WorkspaceChromeStatusHosts,
    ) -> AnyElement {
        let status_group = "workspace-chrome-status";
        let status_mark = self
            .status
            .map(|status| status.mark(appearance, status_hosts, status_group));
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
            )
            .when_some(self.worktree, |chip, worktree| {
                chip.child(
                    div()
                        .debug_selector(|| "workspace-chip-worktree".to_owned())
                        .flex()
                        .flex_row()
                        .items_center()
                        .min_w_0()
                        .gap(appearance.spacing(WORKTREE_SEPARATOR_GAP))
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .child(div().flex_shrink_0().child(Icon::new(
                            IconName::ChevronRight,
                            appearance.icons.metrics(IconRole::Caption).glyph_size,
                            switcher_color,
                        )))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(switcher_color)
                                .child(worktree),
                        ),
                )
            });
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
            .children(status_mark)
            .into_any_element()
    }

    pub(super) fn accessibility_name(&self) -> String {
        let mut name = format!("Switch Workspace, {}", self.name);
        if let Some(worktree) = &self.worktree {
            name.push_str(&format!(", Worktree {worktree}"));
        }
        if let Some(status) = self.status {
            name.push_str(&format!(", {}", status.label()));
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{
        Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
        CompositionCapabilities, SystemAppearance, ThemeCatalog, builtin_chrome_base,
    };

    #[gpui::test]
    fn fullscreen_header_should_keep_edge_air_without_traffic_lights(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::platform::window_frame::{TrafficLightPlacement, WindowFrameGeometry};
        cx.update(|cx| {
            assert_eq!(leading_clearance(false, px(8.0), cx), px(8.0));
            let placement =
                TrafficLightPlacement::new(gpui::point(px(15.5), px(14.0)), px(41.0), px(78.0));
            cx.set_global(WindowFrameGeometry::default().with_traffic_lights(placement, placement));
            assert_eq!(leading_clearance(true, px(8.0), cx), px(8.0));
            assert_eq!(leading_clearance(false, px(8.0), cx), px(78.0));
        });
    }

    #[gpui::test]
    fn workspace_status_mark_does_not_repeat_the_switcher_status(cx: &mut gpui::TestAppContext) {
        use spaceterm_ui::a11y_testing::A11yTree;

        struct Probe;
        impl gpui::Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                let appearance = ChromeAppearance::default();
                let host = appearance.colors.title_bar_background;
                div()
                    .id("switcher-probe")
                    .role(gpui::accesskit::Role::ComboBox)
                    .aria_label("Switch Workspace, work, Disconnected")
                    .child(WorkspaceChromeStatus::Disconnected.mark(
                        &appearance,
                        WorkspaceChromeStatusHosts::new(host, host),
                        "switcher-probe",
                    ))
            }
        }
        cx.update(crate::ui::init).unwrap();
        let (_, cx) = cx.add_window_view(|_, _| Probe);
        let tree = A11yTree::read(cx);
        let switcher = tree.node("Switch Workspace, work, Disconnected");
        assert_eq!(switcher["aria"]["role"], "ComboBox");
        assert!(tree.children(switcher).is_empty());
    }

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
                for status in WorkspaceChromeStatus::ALL {
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
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..AppearancePreferences::default()
        };
        preferences.window.opacity = 0.0;
        let resolved = ThemeCatalog::default()
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
    fn differentiate_without_color_gives_each_status_color_its_own_glyph() {
        for colors in [
            builtin_chrome_base(Appearance::Dark),
            builtin_chrome_base(Appearance::Light),
        ] {
            for (index, status) in WorkspaceChromeStatus::ALL.into_iter().enumerate() {
                for other in WorkspaceChromeStatus::ALL.into_iter().skip(index + 1) {
                    if status.color(&colors) != other.color(&colors) {
                        assert_ne!(
                            std::mem::discriminant(&status.glyph()),
                            std::mem::discriminant(&other.glyph()),
                            "{status:?} and {other:?}"
                        );
                    }
                }
            }
        }
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
            worktree: None,
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
