//! The Workspace Window's deliberate empty view.
//!
//! A person who detaches the window's final Workspace asked to present nothing, so nothing takes
//! its place: no replacement Workspace opens here and nothing is created on a server. The window
//! keeps its sidebar, its Workspace Switcher, and Client Settings, and the empty view points at
//! the switcher, which attaches detached Workspaces and creates new ones.
//!
//! Without usable server state the stage instead fills the window beneath its Window Controls
//! and presents launch recovery alone: no sidebar, resize handle, or Workspace creation.

use gpui::prelude::*;
use gpui::{AnyElement, AnyView, Context, Edges, Pixels, SharedString, Window, div, px};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, Tooltip};

use super::super::chrome_typography::{ChromeTextStyleExt, TextRole};
use super::super::workspace_frame::WorkspaceFrame;
use super::{SwitchWorkspace, WindowDragRegion, WorkspaceManager};
use crate::ui::appearance::gpui_color;

pub(super) const EMPTY_TITLE: &str = "No Workspaces Attached";
/// The detail while the server is reachable, so its detached Workspaces are in the switcher.
pub(super) const EMPTY_DETAIL: &str = "Choose a Workspace to attach, or create one.";
/// The detail while the server is unreachable, so the switcher lists no detached Workspace.
pub(super) const EMPTY_OFFLINE_DETAIL: &str = "Reconnect to choose a Workspace.";
pub(super) const EMPTY_ACTION: &str = "Choose Workspace…";

/// Where the sidebar stands while the empty view lays out beside it.
pub(super) struct EmptySidebar {
    pub(super) width: Pixels,
    pub(super) reveal: f32,
    pub(super) top_chrome_width: Pixels,
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
        let recovery = sidebar.is_none();
        let sidebar = sidebar.unwrap_or(EmptySidebar {
            width: px(0.0),
            reveal: 0.0,
            top_chrome_width: px(0.0),
        });
        // The stage's leading edge follows the sidebar as it shows or hides.
        let hidden_inset = frame.stage_leading_inset(false);
        let leading_inset =
            hidden_inset + (frame.stage_leading_inset(true) - hidden_inset) * sidebar.reveal;
        let drag_manager = cx.weak_entity();
        let switcher_manager = cx.weak_entity();
        let client_controls = matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ) && !window.is_fullscreen();
        let edge_margin = px(spaceterm_ui::DesktopWindowStyle::current(cx)
            .control_metrics()
            .edge_margin);
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
            None => self.render_empty_message(switcher_manager, cx),
        };
        div()
            .id("empty-workspace-stage")
            .debug_selector(|| "empty-workspace-stage".to_owned())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(top_height)
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .child(div().w(sidebar.top_chrome_width).h_full().flex_shrink_0())
                    .child(
                        div()
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .bg(gpui_color(
                                appearance.surface(crate::appearance::SurfaceRole::Base, title_bar),
                            ))
                            .child(band),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .relative()
                    .ml(sidebar.width * sidebar.reveal)
                    .border_color(stage_color)
                    .border_b(frame.window_edge_inset())
                    .border_r(frame.window_edge_inset())
                    .border_l(leading_inset)
                    .bg(stage_color)
                    .child(with_tab_traversal(message)),
            )
            .into_any_element()
    }

    /// The deliberately empty view's explanation and its one action.
    fn render_empty_message(
        &self,
        switcher_manager: gpui::WeakEntity<Self>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let appearance = super::super::appearance::chrome(cx);
        let detail = SharedString::new_static(self.empty_detail());
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let mut action = Button::new("empty-workspace-choose", EMPTY_ACTION)
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector("empty-workspace-choose")
            .tooltip(
                Tooltip::new("empty-workspace-choose-tooltip", "Switch Workspace")
                    .shortcut(presentation.shortcut(&SwitchWorkspace).unwrap_or_default()),
            )
            .on_activate(move |_, window, cx| {
                let _ = switcher_manager.update(cx, |manager, cx| {
                    manager.open_workspace_switcher(window, cx);
                });
            });
        if let Some(shortcut) = presentation.shortcut(&SwitchWorkspace) {
            action = action.shortcut(shortcut);
        }
        div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label(EMPTY_TITLE)
            .aria_description(detail.clone())
            .size_full()
            .track_focus(&self.empty_focus)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(appearance.spacing(12.0))
            .px(appearance.spacing(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .max_w(appearance.spacing(360.0))
                    .text_center()
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Section))
                            .text_color(gpui_color(appearance.colors.text))
                            .child(EMPTY_TITLE),
                    )
                    .child(
                        div()
                            .debug_selector(|| "empty-workspace-detail".to_owned())
                            .whitespace_normal()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(appearance.colors.text_secondary))
                            .child(detail),
                    ),
            )
            .child(action)
    }

    /// What the empty view says about detached Workspaces, which depends on whether the server
    /// that holds them is reachable.
    pub(super) fn empty_detail(&self) -> &'static str {
        if self.shared_reachable() {
            EMPTY_DETAIL
        } else {
            EMPTY_OFFLINE_DETAIL
        }
    }
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
