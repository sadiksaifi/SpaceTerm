//! Workspace sidebar presentation and transient interaction lifecycle.
//! Workspace policy stays with the application; the sidebar emits typed requests.
mod text;
mod view;

use super::{
    NewRemoteWorkspace, TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
    WORKSPACE_SIDEBAR_MINIMUM_WIDTH,
};
use crate::domain::{RemoteConnectionPhase, WorkspaceId};
use crate::theme::{ACTIVE_THEME, Color};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DispatchPhase, Entity, EntityId, EventEmitter, FocusHandle, Font,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle,
    SharedString, TextRun, WeakEntity, Window, canvas, div, point, px, rgba,
};
use spaceterm_ui::{
    ButtonSize, ButtonVariant, ContextMenu, CustomIconName, Icon, IconButton, IconName, MenuEntry,
    MenuLifecycleEvent, MenuSize, OverlayScrollbar, OverlayScrollbarEvent, ResizeAxis,
    ResizeFinishReason, ResizeHandle, ResizeHandleEvent, ResizeHandleTarget, ResizeInputSource,
    ScrollMetrics, TextInput, TextInputEvent, TextInputVariant, Tooltip, TooltipTargetVisibility,
    dismiss_active_menu,
};

const CHROME_DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;

#[derive(Clone)]
pub(super) enum SidebarEvent {
    Activate {
        workspace_id: WorkspaceId,
        focus_pane: bool,
    },
    Command {
        workspace_id: WorkspaceId,
        command: WorkspaceMenuCommand,
    },
    Rename {
        workspace_id: WorkspaceId,
        name: String,
    },
    NewLocalWorkspace,
    NewRemoteWorkspace,
    FocusChanged,
    LayoutChanged,
    FocusPane,
}
impl EventEmitter<SidebarEvent> for WorkspaceSidebar {}

pub(super) const WORKSPACE_CREATION_ICON_SIZE: f32 = 18.0;
pub(super) const SIDEBAR_ROW_HEIGHT: f32 = 58.0;
pub(super) const SIDEBAR_ROW_HORIZONTAL_PADDING: f32 = 12.0;
pub(super) const SIDEBAR_ROW_ICON_SIZE: f32 = 14.0;
pub(super) const SIDEBAR_NAME_TEXT_SIZE: f32 = 13.0;
pub(super) const NEW_WORKSPACE_BUTTON_HEIGHT: f32 = 40.0;
pub(super) const SIDEBAR_MAXIMUM_WIDTH: f32 = 420.0;
pub(super) const TERMINAL_CONTENT_MINIMUM_WIDTH: f32 = 240.0;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceMenuCommand {
    NewTab,
    PinDirectory,
    UnpinDirectory,
    Reconnect,
    Close,
}

#[derive(Clone, Copy)]
enum RowMenuCommand {
    Workspace(WorkspaceMenuCommand),
    Rename,
}

struct WorkspaceRenameState {
    workspace_id: WorkspaceId,
    initial_value: String,
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
    context_menu_open: bool,
}

#[derive(Clone)]
pub(super) struct WorkspaceRowViewModel {
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: SharedString,
    pub(super) path: SharedString,
    pub(super) machine: Option<SharedString>,
    pub(super) tooltip: SharedString,
    pub(super) pinned: bool,
    pub(super) remote_connection_phase: Option<RemoteConnectionPhase>,
    pub(super) available: bool,
    pub(super) tab_count: usize,
    pub(super) pane_count: usize,
    pub(super) active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SidebarLayout {
    pub(super) visible: bool,
    pub(super) width: Pixels,
}

/// Owns sidebar layout, scrolling, menus, inline rename, and focus transitions.
pub(super) struct WorkspaceSidebar {
    rows: Vec<WorkspaceRowViewModel>,
    remote_unavailable: Option<String>,
    layout: SidebarLayout,
    scroll_handle: ScrollHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    focus: FocusHandle,
    menu: Option<WorkspaceId>,
    rename: Option<WorkspaceRenameState>,
    resize_origin: Option<SidebarLayout>,
    suppress_pointer_until_release: bool,
}

impl WorkspaceSidebar {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("workspace-scrollbar"));
        cx.subscribe_in(
            &scrollbar,
            window,
            |sidebar, _, event: &OverlayScrollbarEvent<f32>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    sidebar.focus.focus(window);
                    cx.emit(SidebarEvent::FocusChanged);
                }
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    let current = sidebar.scroll_handle.offset();
                    sidebar
                        .scroll_handle
                        .set_offset(point(current.x, px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();
        cx.observe_window_bounds(window, |sidebar, window, cx| {
            sidebar.constrain_to_window(window, cx);
        })
        .detach();
        let focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&focus, window, |_, _, cx| {
            cx.notify();
            cx.emit(SidebarEvent::FocusChanged);
        })
        .detach();
        cx.on_blur(&focus, window, |_, _, cx| {
            cx.notify();
            cx.emit(SidebarEvent::FocusChanged);
        })
        .detach();
        Self {
            rows: Vec::new(),
            remote_unavailable: None,
            layout: SidebarLayout {
                visible: true,
                width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            },
            scroll_handle: ScrollHandle::new(),
            scrollbar,
            focus,
            menu: None,
            rename: None,
            resize_origin: None,
            suppress_pointer_until_release: false,
        }
    }

    pub(super) fn dismiss_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_is_focused(window) {
            self.focus.focus(window);
        }
        self.rename = None;
        self.menu = None;
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    fn begin_resize(&mut self, source: ResizeInputSource) {
        self.resize_origin = Some(self.layout);
        if source == ResizeInputSource::Pointer {
            self.suppress_pointer_until_release = false;
        }
    }

    fn finish_resize(
        &mut self,
        source: ResizeInputSource,
        reason: ResizeFinishReason,
    ) -> (bool, Option<SidebarLayout>) {
        if source == ResizeInputSource::Pointer {
            self.suppress_pointer_until_release = !matches!(
                reason,
                ResizeFinishReason::Completed | ResizeFinishReason::PointerButtonLost
            );
        }
        let origin = self.resize_origin.take();
        (
            origin.is_some(),
            origin.filter(|_| reason == ResizeFinishReason::Escape),
        )
    }

    pub(super) fn rename_is_focused(&self, window: &Window) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.focus_handle.is_focused(window))
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height_px = f32::from(self.scroll_handle.bounds().size.height);
        let maximum_offset_px = f32::from(self.scroll_handle.max_offset().height);
        let offset_px = -f32::from(self.scroll_handle.offset().y);
        ScrollMetrics::for_pixels(0.0, track_height_px, maximum_offset_px, offset_px)
    }

    fn sync_scrollbar(&self, cx: &mut App) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    pub(super) fn reveal_scrollbar(&self, cx: &mut App) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }
}

impl WorkspaceSidebar {
    fn request_menu(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.rows.iter().any(|row| row.workspace_id == workspace_id) {
            return false;
        }
        self.focus.focus(window);
        self.rename = None;
        self.menu = Some(workspace_id);
        cx.emit(SidebarEvent::Activate {
            workspace_id,
            focus_pane: false,
        });
        cx.notify();
        true
    }

    pub(super) fn handle_menu_lifecycle(
        &mut self,
        workspace_id: WorkspaceId,
        event: MenuLifecycleEvent,
        _: &Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => self.menu = Some(workspace_id),
            MenuLifecycleEvent::Closed(_)
                if self.menu.is_some_and(|target| target == workspace_id) =>
            {
                self.menu = None
            }
            MenuLifecycleEvent::Closed(_) => return,
        }
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    fn perform_menu_command(
        &mut self,
        workspace_id: WorkspaceId,
        command: RowMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            RowMenuCommand::Rename => {
                let Some(workspace) = self
                    .rows
                    .iter()
                    .find(|row| row.workspace_id == workspace_id)
                else {
                    cx.emit(SidebarEvent::FocusChanged);
                    return;
                };
                let input = cx.new(|cx| {
                    TextInput::new(
                        "workspace-rename-input",
                        "Workspace name",
                        workspace.name.clone(),
                        window,
                        cx,
                    )
                    .variant(TextInputVariant::Bare)
                    .debug_selector("workspace-rename-input")
                });
                let input_id = input.entity_id();
                cx.subscribe_in(
                    &input,
                    window,
                    move |sidebar, input, event: &TextInputEvent, window, cx| match event {
                        TextInputEvent::Submitted => {
                            let value = input.read(cx).value().to_owned();
                            cx.defer_in(window, move |sidebar, window, cx| {
                                sidebar.finish_rename(input_id, Some(value), true, window, cx);
                            });
                        }
                        TextInputEvent::Cancelled => {
                            cx.defer_in(window, move |sidebar, window, cx| {
                                sidebar.finish_rename(input_id, None, true, window, cx);
                            });
                        }
                        TextInputEvent::FocusLost => {
                            let menu_open = sidebar
                                .rename
                                .as_ref()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| rename.context_menu_open);
                            if !menu_open {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |sidebar, window, cx| {
                                    sidebar.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        TextInputEvent::ContextMenuOpened => {
                            if let Some(rename) = &mut sidebar.rename
                                && rename.input.entity_id() == input_id
                            {
                                rename.context_menu_open = true;
                            }
                        }
                        TextInputEvent::ContextMenuClosed => {
                            let should_finish = sidebar
                                .rename
                                .as_mut()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| {
                                    rename.context_menu_open = false;
                                    !rename.focus_handle.is_focused(window)
                                });
                            if should_finish {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |sidebar, window, cx| {
                                    sidebar.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        _ => {}
                    },
                )
                .detach();
                self.rename = Some(WorkspaceRenameState {
                    workspace_id,
                    initial_value: input.read(cx).value().to_owned(),
                    focus_handle: input.read(cx).focus_handle(),
                    input,
                    context_menu_open: false,
                });
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
                cx.defer_in(window, |sidebar, window, cx| {
                    let Some(rename) = &sidebar.rename else {
                        return;
                    };
                    let input = rename.input.clone();
                    let focus_handle = rename.focus_handle.clone();
                    input.update(cx, |input, cx| input.select_all(cx));
                    focus_handle.focus(window);
                    cx.emit(SidebarEvent::FocusChanged);
                });
            }
            RowMenuCommand::Workspace(command) => cx.emit(SidebarEvent::Command {
                workspace_id,
                command,
            }),
        }
    }
    fn finish_rename(
        &mut self,
        input_id: EntityId,
        value: Option<String>,
        restore_sidebar_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.rename.as_ref() else {
            return;
        };
        if rename.input.entity_id() != input_id {
            return;
        }
        let workspace_id = rename.workspace_id;
        if let Some(value) = value
            && value.trim() != rename.initial_value
        {
            cx.emit(SidebarEvent::Rename {
                workspace_id,
                name: value,
            });
        }
        self.rename = None;
        if restore_sidebar_focus {
            self.focus.focus(window);
        }
        cx.emit(SidebarEvent::FocusChanged);

        cx.notify();
    }
}
pub(super) fn remote_connection_status(phase: RemoteConnectionPhase) -> Option<&'static str> {
    match phase {
        RemoteConnectionPhase::Connected => None,
        RemoteConnectionPhase::Reconnecting => Some("Reconnecting…"),
        RemoteConnectionPhase::Disconnected => Some("Disconnected"),
        RemoteConnectionPhase::Failed => Some("Connection failed"),
        RemoteConnectionPhase::Closing => Some("Closing…"),
    }
}

pub(super) fn remote_connection_color(phase: RemoteConnectionPhase) -> Color {
    match phase {
        RemoteConnectionPhase::Reconnecting => ACTIVE_THEME.info,
        RemoteConnectionPhase::Connected => ACTIVE_THEME.success,
        RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Closing => ACTIVE_THEME.icon,
        RemoteConnectionPhase::Failed => ACTIVE_THEME.error,
    }
}

impl WorkspaceSidebar {
    pub(super) fn set_rows(
        &mut self,
        rows: Vec<WorkspaceRowViewModel>,
        remote_unavailable: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.rows = rows;
        self.remote_unavailable = remote_unavailable;
        cx.notify();
    }
    fn collapsed_top_chrome_width(&self, window: &Window) -> Pixels {
        let active = self.rows.iter().find(|row| row.active);
        collapsed_top_chrome_width(
            active.map_or("", |row| row.name.as_ref()),
            active.is_some_and(|row| row.pinned),
            window,
        )
    }
}
impl Render for WorkspaceSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_scrollbar(cx);
        self.render_body(cx.entity().downgrade(), window, cx)
    }
}
fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

impl WorkspaceSidebar {
    pub(super) fn set_layout(
        &mut self,
        visible: bool,
        width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let maximum = (window.bounds().size.width - px(TERMINAL_CONTENT_MINIMUM_WIDTH))
            .min(px(SIDEBAR_MAXIMUM_WIDTH))
            .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH));
        let width = width.clamp(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH), maximum);
        if self.layout == (SidebarLayout { visible, width }) {
            return;
        }
        self.layout = SidebarLayout { visible, width };
        if !visible {
            self.scrollbar
                .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        }
        cx.emit(SidebarEvent::LayoutChanged);
        cx.notify();
    }
    fn constrain_to_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.layout.visible {
            self.set_layout(true, self.layout.width, window, cx);
        }
    }
    pub(super) fn resize(
        &mut self,
        requested_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let minimum_width = px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH);
        if requested_width < minimum_width {
            let was_sidebar_focused =
                self.focus.is_focused(window) || self.rename_is_focused(window);
            self.rename = None;
            self.set_layout(false, minimum_width, window, cx);
            if was_sidebar_focused {
                cx.emit(SidebarEvent::FocusPane);
            }
            cx.emit(SidebarEvent::FocusChanged);
            return;
        }

        self.set_layout(true, requested_width, window, cx);
    }

    fn handle_resize_event(
        &mut self,
        event: ResizeHandleEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ResizeHandleEvent::InteractionStarted { source, .. } => {
                self.begin_resize(source);
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
            }
            ResizeHandleEvent::ResizeRequested {
                requested_value, ..
            } => {
                let should_resize = self.layout.visible
                    || px(requested_value)
                        >= self
                            .collapsed_top_chrome_width(window)
                            .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH));
                if should_resize {
                    self.resize(px(requested_value), window, cx);
                }
            }
            ResizeHandleEvent::ResetRequested { source } => {
                self.resize(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), window, cx);
                if source == ResizeInputSource::Pointer {
                    cx.emit(SidebarEvent::FocusPane);
                }
            }
            ResizeHandleEvent::InteractionFinished { source, reason, .. } => {
                let (finished, restore) = self.finish_resize(source, reason);
                if let Some(origin) = restore {
                    self.set_layout(origin.visible, origin.width, window, cx);
                }
                if !finished {
                    return;
                }
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
                if source == ResizeInputSource::Pointer {
                    cx.emit(SidebarEvent::FocusPane);
                }
            }
        }
    }
}

impl WorkspaceSidebar {
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let Some(current) = self.rows.iter().position(|row| row.active) else {
            return;
        };
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(self.rows.len() - 1),
            "home" => 0,
            "end" => self.rows.len() - 1,
            "enter" | "escape" => {
                cx.emit(SidebarEvent::FocusPane);
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.scroll_handle.scroll_to_item(next);
        if next != current {
            cx.emit(SidebarEvent::Activate {
                workspace_id: self.rows[next].workspace_id,
                focus_pane: false,
            });
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }
}

// Preserve the muted hierarchy while keeping small text readable on selected and hovered rows.
fn secondary_text_color() -> Color {
    let muted = ACTIVE_THEME.text_muted;
    let text = ACTIVE_THEME.text;
    let channel = |muted: u8, text: u8| ((u16::from(muted) * 7 + u16::from(text)) / 8) as u8;
    Color::from_rgb_components(
        channel(muted.r, text.r),
        channel(muted.g, text.g),
        channel(muted.b, text.b),
    )
}

pub(super) const SIDEBAR_TOGGLE_INSET: f32 = 4.0;
pub(super) const TOP_CHROME_ACTION_SIZE: f32 = 28.0;
pub(super) const WORKSPACE_SWITCHER_PADDING: f32 = 4.0;
pub(super) const COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH: f32 = 220.0;
pub(super) const TRAFFIC_LIGHT_CLEARANCE: f32 = 82.0;
pub(super) const WORKSPACE_CHIP_ICON_SIZE: f32 = 14.0;
pub(super) const WORKSPACE_CHIP_PIN_SIZE: f32 = 12.0;
pub(super) const WORKSPACE_CHIP_GAP: f32 = 5.0;
pub(super) const WORKSPACE_CHIP_TEXT_SIZE: f32 = 12.0;
pub(super) fn top_chrome_trailing_inset() -> Pixels {
    // Leave the trigger border outside the sidebar resize target as well.
    super::resize_handle_theme::spacious_target_half_thickness() + px(1.0)
}

pub(super) fn collapsed_top_chrome_width(name: &str, pinned: bool, window: &Window) -> Pixels {
    let text_style = window.text_style();
    let run = TextRun {
        len: name.len(),
        font: Font {
            family: text_style.font_family,
            features: text_style.font_features,
            fallbacks: text_style.font_fallbacks,
            weight: text_style.font_weight,
            style: text_style.font_style,
        },
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let name_width = window
        .text_system()
        .shape_line(
            name.to_owned().into(),
            px(WORKSPACE_CHIP_TEXT_SIZE),
            &[run],
            None,
        )
        .width;
    let fixed_width = px(TRAFFIC_LIGHT_CLEARANCE
        + TOP_CHROME_ACTION_SIZE
        + WORKSPACE_CHIP_ICON_SIZE * 2.0
        + WORKSPACE_CHIP_GAP * 2.0
        + WORKSPACE_SWITCHER_PADDING * 2.0
        + 2.0)
        + top_chrome_trailing_inset();
    let pin_width = if pinned {
        px(WORKSPACE_CHIP_PIN_SIZE + WORKSPACE_CHIP_GAP)
    } else {
        px(0.0)
    };
    (fixed_width + pin_width + name_width)
        .ceil()
        .min(px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH))
}

impl WorkspaceSidebar {
    pub(super) fn layout(&self) -> SidebarLayout {
        self.layout
    }
    pub(super) fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }
    pub(super) fn is_resizing(&self) -> bool {
        self.resize_origin.is_some()
    }
    pub(super) fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }
    pub(super) fn menu_target(&self) -> Option<WorkspaceId> {
        self.menu
    }
    pub(super) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.emit(SidebarEvent::FocusChanged);
            cx.notify();
        }
    }
    pub(super) fn cancel_rename_for(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.workspace_id == workspace_id)
        {
            self.cancel_rename(cx);
        }
    }
    pub(super) fn reveal_row(&self, index: usize, cx: &mut Context<Self>) {
        self.scroll_handle.scroll_to_item(index);
        cx.notify();
    }
    pub(super) fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore =
            self.is_focused(window) || self.rename_is_focused(window) || self.menu.is_some();
        if self.layout.visible && self.menu.take().is_some() {
            dismiss_active_menu(window, cx);
        }
        self.cancel_rename(cx);
        self.set_layout(!self.layout.visible, self.layout.width, window, cx);
        if !self.layout.visible && restore {
            cx.emit(SidebarEvent::FocusPane);
        }
    }
    pub(super) fn toggle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_focused(window) || self.rename_is_focused(window) {
            self.cancel_rename(cx);
            cx.emit(SidebarEvent::FocusPane);
        } else {
            self.set_layout(true, self.layout.width, window, cx);
            if let Some(index) = self.rows.iter().position(|row| row.active) {
                self.reveal_row(index, cx);
            }
            cx.defer_in(window, |sidebar, window, cx| {
                sidebar.focus.focus(window);
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
            });
        }
    }
}

#[cfg(test)]
impl WorkspaceSidebar {
    pub(super) fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll_handle
    }
    pub(super) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }
    pub(super) fn rename_input(&self) -> Option<&Entity<TextInput>> {
        self.rename.as_ref().map(|rename| &rename.input)
    }
    pub(super) fn set_resizing_for_test(&mut self, resizing: bool) {
        self.resize_origin = resizing.then_some(self.layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn luminance(color: Color) -> f64 {
        let linear = |channel: u8| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        linear(color.r) * 0.2126 + linear(color.g) * 0.7152 + linear(color.b) * 0.0722
    }
    #[test]
    fn secondary_text_should_be_readable_on_every_row_background() {
        let foreground = luminance(secondary_text_color());
        for background in [
            ACTIVE_THEME.panel_background,
            ACTIVE_THEME.element_selected,
            ACTIVE_THEME.ghost_element_hover,
        ] {
            let background = luminance(background);
            assert!(
                (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05) >= 4.5
            );
        }
    }
    #[test]
    fn connection_states_should_use_semantic_colors() {
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Reconnecting),
            ACTIVE_THEME.info
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Failed),
            ACTIVE_THEME.error
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Disconnected),
            ACTIVE_THEME.icon
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Closing),
            ACTIVE_THEME.icon
        );
    }
}
