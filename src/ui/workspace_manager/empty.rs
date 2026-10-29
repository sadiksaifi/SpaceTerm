//! The Workspace Window's deliberate empty view.
//!
//! A person who detaches the window's final Workspace asked to present nothing, so nothing takes
//! its place: no replacement Workspace opens here and nothing is created on a server. The window
//! keeps its sidebar, its Workspace Switcher, and Client Settings, and the empty view points at
//! the switcher, which attaches detached Workspaces and creates new ones.
//!
//! The stage is drawn as an empty Pane: the Pane's rim and corner radius on the Pane's surface,
//! so the content area keeps its edge beside a sidebar of the same color. A window with Panes
//! draws no such edge, because each Pane draws its own.
//!
//! Without usable server state the stage instead fills the window beneath its Window Controls
//! and presents launch recovery alone: no sidebar, resize handle, or Workspace creation.

use gpui::prelude::*;
use gpui::{AnyElement, AnyView, Context, Edges, Pixels, SharedString, Window, div, px};
use spaceterm_ui::{Button, CustomIconName, EmptyStateTone, IconName, Tooltip};

use super::super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::super::sidebar_motion::SidebarMotion;
use super::super::workspace_creation::WorkspaceCreation;
use super::super::workspace_frame::WorkspaceFrame;
use super::launch::LaunchAction;
use super::{SwitchWorkspace, WindowDragRegion, WorkspaceManager};
use crate::ui::appearance::gpui_color;

pub(super) const EMPTY_TITLE: &str = "No Workspaces Attached";
/// The detail while the server is reachable, so its detached Workspaces are in the switcher.
pub(super) const EMPTY_DETAIL: &str = "Choose a Workspace to attach, or create one.";
pub(super) const EMPTY_ACTION: &str = "Choose Workspace\u{2026}";
pub(super) const EMPTY_CREATE: &str = "New Local Workspace";
/// The title while this computer's server is unreachable, so the switcher lists none of its
/// Workspaces.
pub(super) const OFFLINE_TITLE: &str = "Server Not Connected";

/// Where the sidebar stands while the empty view lays out beside it.
pub(super) struct EmptySidebar {
    pub(super) reveal: f32,
    pub(super) top_chrome_width: Pixels,
    /// Where the top-left chrome's toggle and switcher end, and how far down they reach.
    pub(super) controls_width: Pixels,
    pub(super) controls_bottom: Pixels,
}

impl WorkspaceManager {
    /// The stage presented while the window has no Workspace, beside the sidebar and under the
    /// top-left chrome, with the title bar's trailing controls the Tab strip would carry. Without
    /// `sidebar` it fills the window and carries the leading Window Controls itself.
    pub(super) fn render_empty_workspace(
        &self,
        sidebar: Option<EmptySidebar>,
        update_control: AnyView,
        close: spaceterm_ui::WindowCloseHandler,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let appearance = super::super::appearance::chrome(cx);
        let frame = WorkspaceFrame::for_appearance(appearance, cx);
        let top_height = frame.top_chrome_height(appearance.top_height());
        let title_bar = if window.is_window_active() {
            appearance.colors.title_bar_background
        } else {
            appearance.colors.title_bar_inactive_background
        };
        let stage_color = gpui_color(appearance.surface(
            crate::appearance::SurfaceRole::Base,
            super::super::workspace_frame::base_surface(&appearance.colors),
        ));
        // The configured terminal background, as every Pane's surface takes it.
        let terminal_background = super::super::appearance_runtime::current(cx)
            .terminal
            .colors
            .background;
        let recovery = sidebar.is_none();
        let sidebar = sidebar.unwrap_or(EmptySidebar {
            reveal: 0.0,
            top_chrome_width: px(0.0),
            controls_width: px(0.0),
            controls_bottom: px(0.0),
        });
        // The stage's leading edge follows the sidebar as it shows or hides.
        let hidden_inset = frame.stage_leading_inset(false);
        let leading_inset =
            hidden_inset + (frame.stage_leading_inset(true) - hidden_inset) * sidebar.reveal;
        let drag_manager = cx.weak_entity();
        let message_manager = cx.weak_entity();
        let client_controls = matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ) && !window.is_fullscreen();
        let edge_margin = px(spaceterm_ui::DesktopWindowStyle::current(cx)
            .control_metrics()
            .edge_margin);
        // The band above the stage holds nothing unless window controls or the update pill sit
        // there. Without a sidebar the stage runs under the leading controls, and while the
        // sidebar hides the top-left chrome slides over the stage, so only a shown sidebar frees
        // the band.
        let band_occupied = recovery
            || self.update_control.read(cx).shows_pill(cx)
            || (client_controls
                && spaceterm_ui::ClientWindowControls::width(
                    spaceterm_ui::WindowControlSide::Right,
                    window,
                    cx,
                ) > px(0.0));
        // The band empties and fills on the sidebar's curve, and at once under Reduce Motion, so
        // the stage never jumps when the update pill or the Window Controls come or go.
        let band = {
            let mut motion = self.empty_band.borrow_mut();
            let motion = motion.get_or_insert_with(|| SidebarMotion::new(!band_occupied));
            motion.expect_change();
            motion.present(!band_occupied, cx)
        };
        if band.moving {
            window.request_animation_frame();
        }
        let band_free = band.reveal * sidebar.reveal;
        let stage_top = frame.stage_top_inset(appearance.top_height(), band_free);
        // The stage's top-left corner travels beside the switcher, a frame space after it, while
        // its top edge is level with the controls. Its top edge eases with the sidebar, and only
        // once it has dropped below the controls does the corner ease on to the window's edge,
        // so the stage never passes under the switcher.
        let below = frame.stage_top_inset(appearance.top_height(), 0.0);
        let clear = ((below - sidebar.controls_bottom)
            / (below - frame.stage_top_inset(appearance.top_height(), 1.0)))
        .clamp(0.0, 1.0);
        let beside_controls = if clear > 0.0 {
            let progress = (sidebar.reveal / clear).min(1.0);
            progress * progress * (3.0 - 2.0 * progress)
        } else {
            1.0
        };
        let stage_leading = sidebar.controls_width * beside_controls;
        let band = WindowDragRegion::new(
            "empty-workspace-drag-region",
            "Move Window",
            div()
                .size_full()
                .pt(frame.window_edge())
                .flex()
                .flex_row()
                .items_center()
                // The top-left chrome carries the leading controls, except during recovery.
                .when(recovery && client_controls, |band| {
                    band.child(
                        div().flex_none().pl(edge_margin).child(
                            spaceterm_ui::ControlHost::TitleBar.mount(
                                spaceterm_ui::ClientWindowControls::new(close.clone())
                                    .side(spaceterm_ui::WindowControlSide::Left)
                                    .surface_color(gpui_color(title_bar)),
                            ),
                        ),
                    )
                })
                .child(div().flex_1().min_w_0())
                .child(
                    div()
                        .flex_none()
                        .h_full()
                        .flex()
                        .items_center()
                        .child(update_control),
                )
                .when(client_controls, |band| {
                    band.child(
                        div().flex_none().pr(edge_margin).child(
                            spaceterm_ui::ControlHost::TitleBar.mount(
                                spaceterm_ui::ClientWindowControls::new(close)
                                    .surface_color(gpui_color(title_bar)),
                            ),
                        ),
                    )
                }),
        )
        .middle_activation(matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ))
        .status(self.window_drag_status.clone())
        // The sidebar's resize target overlaps the band's leading edge only beside the sidebar.
        .pointer_insets(if recovery {
            Edges::default()
        } else {
            Edges {
                left: super::super::control_theme::resize_handle::spacious_target_half_thickness(
                    cx,
                ),
                ..Edges::default()
            }
        })
        .debug_selector("empty-workspace-drag-region")
        .on_event(move |event, window, cx| {
            let event = *event;
            drag_manager
                .update(cx, |manager, cx| {
                    manager.handle_operating_system_window_drag_event(event, window, cx)
                })
                .unwrap_or_default()
        });
        let message = match self.launch_presentation() {
            Some(launch) if self.launch_is_quiet(&launch, cx) => {
                self.render_launch_quiet(&self.empty_focus)
            }
            Some(launch) => self.render_launch_message(launch, &self.empty_focus, cx),
            None => self.render_empty_message(message_manager, cx),
        };
        let stage = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .ml(stage_leading)
            .border_color(stage_color)
            .border_b(frame.window_edge_inset())
            .border_r(frame.window_edge_inset())
            .border_l(leading_inset)
            .bg(stage_color)
            .child(
                // An empty Pane: its surface, rim, and corner radius, with no content.
                div()
                    .id("empty-workspace-pane")
                    .debug_selector(|| "empty-workspace-pane".to_owned())
                    .size_full()
                    .overflow_hidden()
                    .rounded(frame.pane_radius())
                    .bg(gpui_color(appearance.pane_surface(terminal_background)))
                    .border(frame.pane_rim_width())
                    .border_color(gpui_color(appearance.pane_rim_on(terminal_background)))
                    .child(with_tab_traversal(message)),
            );
        // The stage lies beneath the band, which stays the window's drag region and paints its
        // title-bar surface only while it covers nothing of the stage.
        div()
            .id("empty-workspace-stage")
            .debug_selector(|| "empty-workspace-stage".to_owned())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .debug_selector(|| "empty-workspace-stage-area".to_owned())
                    .absolute()
                    .top(stage_top)
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .flex_col()
                    .child(stage),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(top_height)
                    .flex()
                    .flex_row()
                    .child(div().w(sidebar.top_chrome_width).h_full().flex_shrink_0())
                    .child(
                        div()
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .when(band_free == 0.0, |band| {
                                band.bg(gpui_color(
                                    appearance
                                        .surface(crate::appearance::SurfaceRole::Base, title_bar),
                                ))
                            })
                            .child(band),
                    ),
            )
            .into_any_element()
    }
}

impl WorkspaceManager {
    /// The deliberately empty view: what it presents and how to attach or create a Workspace,
    /// or, while this computer's server is unreachable, how to reach it again.
    fn render_empty_message(
        &self,
        manager: gpui::WeakEntity<Self>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let reachable = self.shared_reachable();
        let place = presentation.wording().this_computer_phrase;
        let local = self.shared.local();
        let stopped = local.and_then(|shared| shared.connection_error())
            == Some(spaceterm_server::ServerError::Stopped);
        let (title, detail): (&'static str, SharedString) = if reachable {
            // A reachable server ends any reconnect the person asked for.
            self.launch.reconnect_from.set(None);
            (EMPTY_TITLE, EMPTY_DETAIL.into())
        } else if stopped {
            (OFFLINE_TITLE, stopped_detail(place))
        } else {
            (OFFLINE_TITLE, offline_detail(place))
        };
        let reconnect = self.reconnect_status(cx);
        let state = if reachable {
            let choose_manager = manager.clone();
            // A push button names its command only; the help tag shows the Shortcut, as a menu
            // does.
            let choose = Button::new("empty-workspace-choose", EMPTY_ACTION)
                .tab_stop(true)
                .debug_selector("empty-workspace-choose")
                .tooltip(
                    Tooltip::new("empty-workspace-choose-tooltip", "Choose Workspace")
                        .shortcut(presentation.shortcut(&SwitchWorkspace).unwrap_or_default()),
                )
                .on_activate(move |_, window, cx| {
                    let _ = choose_manager.update(cx, |manager, cx| {
                        manager.open_workspace_switcher(window, cx);
                    });
                });
            let create = Button::new("empty-workspace-create", EMPTY_CREATE)
                .tab_stop(true)
                .debug_selector("empty-workspace-create")
                .on_activate(move |_, window, cx| {
                    let _ = manager.update(cx, |manager, cx| {
                        manager.run_workspace_creation(WorkspaceCreation::Local, window, cx);
                    });
                });
            crate::ui::empty_state::empty_state(title, cx)
                .custom_icon(CustomIconName::RectangleStack)
                .primary(choose)
                .link(create)
        } else {
            let action = |action: LaunchAction| {
                let owner = manager.clone();
                Button::new(action.selector(), action.label())
                    .tab_stop(true)
                    .debug_selector(action.selector())
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |manager, cx| {
                            manager.run_launch_action(action, window, cx);
                        });
                    })
            };
            // A stopped server answers no reconnect: only Start reverses Stop.
            let primary = if stopped {
                action(LaunchAction::Start)
            } else {
                let owner = manager.clone();
                Button::new(RECONNECT_SELECTOR, LaunchAction::Retry.label())
                    .tab_stop(true)
                    .disabled(matches!(reconnect, Some(Reconnect::Trying)))
                    .debug_selector(RECONNECT_SELECTOR)
                    .on_activate(move |_, _, cx| {
                        let _ = owner.update(cx, |manager, cx| manager.reconnect_local(cx));
                    })
            };
            let state = crate::ui::empty_state::empty_state(title, cx)
                .icon(IconName::TriangleAlert)
                .tone(EmptyStateTone::Warning)
                .primary(primary)
                .link(action(LaunchAction::Settings));
            match reconnect.filter(|_| !stopped) {
                Some(status) => state.footer(status.render(cx)),
                None => state,
            }
        };
        div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label(title)
            .aria_description(detail.clone())
            .size_full()
            .track_focus(&self.empty_focus)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(crate::ui::appearance::chrome(cx).spacing(24.0))
            .child(
                state.detail_element(
                    div()
                        .debug_selector(|| "empty-workspace-detail".to_owned())
                        .child(detail),
                ),
            )
    }
}

/// Why the window shows no Workspace while this computer's server is unreachable.
impl WorkspaceManager {
    /// Asks this computer's server connection to reconnect now, and remembers the failed
    /// attempts so far, so the view can say whether this attempt failed.
    fn reconnect_local(&mut self, cx: &mut Context<Self>) {
        let Some(shared) = self.shared.local() else {
            return;
        };
        self.launch
            .reconnect_from
            .set(Some(shared.failed_attempts()));
        shared.retry_connection();
        cx.notify();
    }

    /// Where the reconnect the person asked for stands, if they asked for one.
    fn reconnect_status(&self, _: &Context<Self>) -> Option<Reconnect> {
        let from = self.launch.reconnect_from.get()?;
        let shared = self.shared.local()?;
        Some(if shared.failed_attempts() > from {
            Reconnect::Failed(shared.connection_error())
        } else {
            Reconnect::Trying
        })
    }
}

pub(super) const RECONNECT_SELECTOR: &str = "empty-workspace-reconnect";
const RECONNECTING: &str = "Reconnecting\u{2026}";

/// The reconnect the person asked for from the empty view.
#[derive(Clone, Copy)]
enum Reconnect {
    Trying,
    /// The attempt ended without a connection, for the reason the connection gave.
    Failed(Option<spaceterm_server::ServerError>),
}

impl Reconnect {
    fn render(self, cx: &Context<WorkspaceManager>) -> AnyElement {
        let appearance = crate::ui::appearance::chrome(cx);
        let content = match self {
            Self::Trying => div()
                .flex()
                .items_center()
                .gap(appearance.spacing(6.0))
                .child(
                    spaceterm_ui::ProgressRing::new(
                        "empty-workspace-reconnect-progress",
                        RECONNECTING,
                        spaceterm_ui::ProgressState::Indeterminate,
                    )
                    .size(spaceterm_ui::ProgressSize::Compact),
                )
                .child(gpui::Text::new(
                    "empty-workspace-reconnect-progress-label".into(),
                    RECONNECTING.into(),
                ))
                .into_any_element(),
            Self::Failed(error) => {
                let note: SharedString = match error {
                    Some(error) => format!("Still not connected. {error}").into(),
                    None => "Still not connected.".into(),
                };
                gpui::Text::new("empty-workspace-reconnect-failed".into(), note).into_any_element()
            }
        };
        div()
            .debug_selector(|| "empty-workspace-reconnect-status".to_owned())
            .mt(appearance.spacing(12.0))
            .chrome_text(appearance.typography.style(TextRole::Secondary))
            .text_color(gpui_color(appearance.colors.text_secondary))
            .flex()
            .justify_center()
            .child(content)
            .into_any_element()
    }
}

/// Why the window shows no Workspace while this computer's server is stopped. `place` is the
/// host's phrase for this computer.
pub(super) fn stopped_detail(place: &str) -> SharedString {
    format!("You stopped the server on {place}. Start it to use its Workspaces.").into()
}

/// `place` is the host's phrase for this computer, such as "this Mac".
pub(super) fn offline_detail(place: &str) -> SharedString {
    format!(
        "SpaceTerm can\u{2019}t reach the server on {place}, so its Workspaces aren\u{2019}t \
         available."
    )
    .into()
}

/// Plain Tab moves from the view's own focus to its action, as it does on the screens that
/// replace a shared Pane.
fn with_tab_traversal(surface: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    surface.on_key_down(|event: &gpui::KeyDownEvent, window, cx| {
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key != "tab"
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
        {
            return;
        }
        if modifiers.shift {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        window.prevent_default();
        cx.stop_propagation();
    })
}
