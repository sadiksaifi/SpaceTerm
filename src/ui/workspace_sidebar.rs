//! Workspace sidebar presentation and transient interaction lifecycle.
//! Workspace policy stays with the application; the sidebar emits typed requests.
mod text;
mod view;

use super::workspace_chrome::WorkspaceChromeLayout;
use super::workspace_status::{WorkspaceStatusPaint, resolve as resolve_workspace_status};
use super::{NewRemoteWorkspace, WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH};
use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::domain::{RemoteConnectionPhase, WorkspaceId};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DispatchPhase, Entity, EntityId, EventEmitter, FocusHandle,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle,
    SharedString, WeakEntity, Window, canvas, div, point, px, rgba,
};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, ButtonSize, ButtonVariant,
    ContextMenu, Icon, IconButton, IconName, Menu, MenuEntry, MenuLifecycleEvent, MenuSize,
    OverlayScrollbar, OverlayScrollbarEvent, ResizeAxis, ResizeFinishReason, ResizeHandle,
    ResizeHandleEvent, ResizeHandleTarget, ResizeInputSource, ScrollMetrics, TextInput,
    TextInputEvent, TextInputVariant, Tooltip, TooltipTargetVisibility, dismiss_active_menu,
};

const CHROME_DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;
const SIDEBAR_FOOTER_HORIZONTAL_PADDING: f32 = 4.0;

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

pub(super) const SIDEBAR_ROW_HEIGHT: f32 = 58.0;
/// The air a row's content keeps inside the chip that carries its selection.
///
/// The row's own padding is this padding plus the chip inset on that side, so the content stays
/// balanced inside the chip even though the chip's two insets differ.
pub(super) const SIDEBAR_ROW_CHIP_PADDING: f32 = 6.0;
/// The vertical inset of the chip that carries a row's selection.
///
/// The horizontal insets come from the Workspace frame, which knows what the chip rests against on
/// each side: the window edge on the leading side and the floating content stage on the trailing
/// one. The radius comes from the same frame, so a selected Workspace, an Active Tab, and a
/// floating Pane are one family of shapes.
pub(super) const SIDEBAR_ROW_SELECTION_INSET_Y: f32 = 3.0;
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

#[derive(Clone, Copy)]
enum NewWorkspaceMenuCommand {
    Local,
    Remote,
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
    new_workspace_menu_open: bool,
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
            new_workspace_menu_open: false,
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

    pub(super) fn handle_new_workspace_menu_lifecycle(
        &mut self,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => self.new_workspace_menu_open = true,
            MenuLifecycleEvent::Closed(_) => {
                if !self.new_workspace_menu_open {
                    return;
                }
                self.new_workspace_menu_open = false;
            }
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

pub(super) fn remote_connection_color(
    phase: RemoteConnectionPhase,
    colors: &ChromeColors,
) -> Color {
    match phase {
        RemoteConnectionPhase::Reconnecting => colors.info,
        RemoteConnectionPhase::Connected => colors.success,
        RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Closing => colors.icon_muted,
        RemoteConnectionPhase::Failed => colors.error,
    }
}

fn workspace_row_status_paint(
    proposed: Color,
    selected: bool,
    minimum_contrast: f64,
    colors: &ChromeColors,
) -> WorkspaceStatusPaint {
    let (normal_background, hovered_background) = if selected {
        (
            colors.row_selected_background,
            colors.row_selected_hover_background,
        )
    } else {
        (colors.row_background, colors.row_hover_background)
    };
    resolve_workspace_status(
        proposed,
        normal_background,
        hovered_background,
        minimum_contrast,
    )
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
    fn collapsed_top_chrome_width(&self, window: &Window, cx: &App) -> Pixels {
        let active = self.rows.iter().find(|row| row.active);
        WorkspaceChromeLayout::collapsed_width(
            active.map_or("", |row| row.name.as_ref()),
            active.is_some_and(|row| row.pinned),
            window,
            cx,
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
                            .collapsed_top_chrome_width(window, cx)
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
    pub(super) fn menu_open(&self) -> bool {
        self.menu.is_some() || self.new_workspace_menu_open
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
        let restore = self.is_focused(window) || self.rename_is_focused(window) || self.menu_open();
        if self.layout.visible && self.menu_open() {
            self.menu.take();
            self.new_workspace_menu_open = false;
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

    #[test]
    fn custom_row_background_materializes_against_the_sidebar_host() {
        use crate::appearance::{
            AppearanceGeneration, AppearancePreferences, AvailableFonts, CompositionCapabilities,
            SchemeCatalog, SurfaceRole, SystemAppearance,
        };

        let mut preferences = AppearancePreferences::default();
        preferences.background.transparency = 1.0;
        let resolved = SchemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable()
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .expect("built-in appearance should resolve");
        let mut appearance = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        appearance.colors.panel_background = Color::rgb(0x202020);
        appearance.panel_controls.reference.row_background = Color::rgb(0x303030);
        let host = crate::ui::workspace_frame::base_surface(&appearance.colors);
        let expected = appearance.materials.paint(
            SurfaceRole::Surface,
            host,
            appearance.panel_controls.reference.row_background,
        );

        assert_eq!(view::row_background(&appearance), Some(expected));
        appearance.panel_controls.reference.row_background = host;
        assert_eq!(view::row_background(&appearance), None);
    }

    #[test]
    fn secondary_text_should_be_readable_on_every_row_background() {
        let colors = ChromeColors::default();
        for (foreground, background) in [
            (colors.row_secondary, colors.row_background),
            (colors.row_hover_secondary, colors.row_hover_background),
            (
                colors.row_selected_secondary,
                colors.row_selected_background,
            ),
            (
                colors.row_selected_hover_secondary,
                colors.row_selected_hover_background,
            ),
        ] {
            assert!(foreground.contrast_ratio(background) >= 4.5);
        }
    }
    #[test]
    fn connection_states_should_use_semantic_colors() {
        assert_eq!(
            remote_connection_color(
                RemoteConnectionPhase::Reconnecting,
                &ChromeColors::default()
            ),
            ChromeColors::default().info
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Failed, &ChromeColors::default()),
            ChromeColors::default().error
        );
        assert_eq!(
            remote_connection_color(
                RemoteConnectionPhase::Disconnected,
                &ChromeColors::default()
            ),
            ChromeColors::default().icon_muted
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Closing, &ChromeColors::default()),
            ChromeColors::default().icon_muted
        );
    }

    #[test]
    fn semantic_status_paints_should_remain_readable_in_every_row_state() {
        for colors in [
            crate::appearance::builtin_chrome_base(crate::appearance::Appearance::Dark),
            crate::appearance::builtin_chrome_base(crate::appearance::Appearance::Light),
        ] {
            for selected in [false, true] {
                let (normal_background, hovered_background) = if selected {
                    (
                        colors.row_selected_background,
                        colors.row_selected_hover_background,
                    )
                } else {
                    (colors.row_background, colors.row_hover_background)
                };
                for semantic in [colors.info, colors.success, colors.warning, colors.error] {
                    let paint = workspace_row_status_paint(semantic, selected, 4.5, &colors);
                    assert!(paint.normal.contrast_ratio(normal_background) >= 4.5);
                    assert!(paint.hovered.contrast_ratio(hovered_background) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn remote_status_paints_should_preserve_distinct_semantic_states() {
        let colors = ChromeColors::default();
        for selected in [false, true] {
            let reconnecting = workspace_row_status_paint(colors.info, selected, 4.5, &colors);
            let failed = workspace_row_status_paint(colors.error, selected, 4.5, &colors);
            assert_ne!(reconnecting, failed);
        }
    }
}
