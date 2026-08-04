use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, MouseButton, MouseDownEvent, Pixels, Point,
    Render, ScrollHandle, SharedString, Window, div, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};

use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, CloseTarget, CloseWindow,
    CreateWindow, PANE_ACTION_MENU_HEIGHT, PANE_ACTION_MENU_WIDTH, PaneActionMenuCommand, PaneHost,
    PaneHostEvent, TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_WIDTH,
    handle_top_chrome_mouse_down, render_pane_action_menu,
};
use crate::domain::{
    CloseWindowOutcome, SplitAxis, WindowCollection, WindowError, WindowId, ZoomState,
};
use crate::terminal::TerminalSessionFactory;
use crate::theme::{ACTIVE_THEME, Color};

const WINDOW_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const WINDOW_BAR_DIVIDER_SIZE: f32 = 1.0;
const WINDOW_ITEM_WIDTH: f32 = 132.0;
const WINDOW_ITEM_MINIMUM_WIDTH: f32 = 84.0;
const WINDOW_ITEM_MAXIMUM_WIDTH: f32 = 160.0;
const WINDOW_ITEM_RIGHT_PADDING: f32 = 6.0;
const WINDOW_CLOSE_CONTROL_SIZE: f32 = 20.0;
const WINDOW_CLOSE_ICON_SIZE: f32 = 12.0;
const WINDOW_CONTROL_SIZE: f32 = 28.0;
const WINDOW_CONTROL_INSET: f32 = 4.0;
const WINDOW_MENU_BAR_OVERLAP: f32 = 8.0;
const WINDOW_MENU_TOP: f32 = WINDOW_BAR_HEIGHT - WINDOW_MENU_BAR_OVERLAP;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowMenuState {
    window_id: WindowId,
    left: Option<Pixels>,
    zoomed: bool,
    zoom_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowManagerEvent {
    CloseWorkspaceRequested,
    PresentationChanged,
}

pub(crate) struct WindowManager {
    windows: WindowCollection<Entity<PaneHost>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    workspace_root: PathBuf,
    active: bool,
    sidebar_visible: bool,
    next_window_id: u64,
    window_menu: Option<WindowMenuState>,
    window_bar_scroll_handle: ScrollHandle,
}

impl WindowManager {
    fn report_window_error(operation: &str, error: WindowError) {
        eprintln!("failed to {operation} Window: {error}");
    }

    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_window_id = WindowId::new(1);
        let initial_window = Self::create_pane_host(
            initial_window_id,
            Rc::clone(&session_factory),
            workspace_root.clone(),
            window,
            cx,
        );

        Self {
            windows: WindowCollection::new(initial_window_id, initial_window),
            session_factory,
            workspace_root,
            active: true,
            sidebar_visible: true,
            next_window_id: 2,
            window_menu: None,
            window_bar_scroll_handle: ScrollHandle::new(),
        }
    }

    fn create_pane_host(
        window_id: WindowId,
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PaneHost> {
        let pane_host =
            cx.new(|cx| PaneHost::new(window_id, session_factory, workspace_root, window, cx));
        debug_assert_eq!(pane_host.read(cx).window_id(), window_id);
        cx.subscribe_in(
            &pane_host,
            window,
            |manager, _, event: &PaneHostEvent, window, cx| match event {
                PaneHostEvent::CloseWindowRequested { window_id } => {
                    manager.close_window(*window_id, window, cx);
                }
                PaneHostEvent::PresentationChanged { .. } => {
                    cx.emit(WindowManagerEvent::PresentationChanged);
                    cx.notify();
                }
            },
        )
        .detach();
        pane_host
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.active {
            self.windows.active_window().read(cx).focus(window, cx);
        }
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        self.windows
            .active_window()
            .update(cx, |pane_host, cx| pane_host.activate_without_focus(cx));
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.windows
            .active_window()
            .update(cx, |pane_host, cx| pane_host.deactivate(cx));
    }

    pub(crate) fn close_all(&self, cx: &mut App) {
        for (_, pane_host) in self.windows.iter() {
            pane_host.update(cx, |pane_host, cx| pane_host.close_all(cx));
        }
    }

    pub(crate) fn set_sidebar_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.sidebar_visible != visible {
            self.sidebar_visible = visible;
            cx.notify();
        }
    }

    pub(crate) fn sidebar_detail(&self, cx: &App) -> SharedString {
        let title = self.windows.active_window().read(cx).window_title();
        if self.windows.len() == 1 {
            return title;
        }
        format!("{title} · {} Windows", self.windows.len()).into()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.windows
            .active_window()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }

    fn allocate_window_id(&mut self) -> Option<WindowId> {
        let window_id = WindowId::new(self.next_window_id);
        self.next_window_id = self.next_window_id.checked_add(1)?;
        Some(window_id)
    }

    fn scroll_active_window_into_view(&self) {
        let active_window_id = self.windows.active_window_id();
        if let Some(index) = self
            .windows
            .iter()
            .position(|(window_id, _)| window_id == active_window_id)
        {
            self.window_bar_scroll_handle.scroll_to_item(index);
        }
    }

    pub(crate) fn create_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(window_id) = self.allocate_window_id() else {
            eprintln!("cannot create Window because the Window ID space is exhausted");
            return;
        };
        let previous_window = self.windows.active_window().clone();
        let pane_host = Self::create_pane_host(
            window_id,
            Rc::clone(&self.session_factory),
            self.workspace_root.clone(),
            window,
            cx,
        );
        if let Err(error) = self.windows.create_window(window_id, || pane_host.clone()) {
            pane_host.update(cx, |pane_host, cx| pane_host.close_all(cx));
            Self::report_window_error("create", error);
            return;
        }

        previous_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        if self.active {
            pane_host.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            pane_host.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.window_menu = None;
        self.scroll_active_window_into_view();
        cx.emit(WindowManagerEvent::PresentationChanged);
        cx.notify();
    }

    fn activate_window(
        &mut self,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(next_window) = self.windows.window(window_id).cloned() else {
            eprintln!("cannot activate unknown Window {window_id}");
            return false;
        };
        let previous_window_id = self.windows.active_window_id();
        let previous_window = self.windows.active_window().clone();
        if let Err(error) = self.windows.activate_window(window_id) {
            Self::report_window_error("activate", error);
            return false;
        }

        if previous_window_id != window_id {
            previous_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        if self.active {
            next_window.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            next_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.window_menu = None;
        self.scroll_active_window_into_view();
        cx.emit(WindowManagerEvent::PresentationChanged);
        cx.notify();
        true
    }

    fn activate_window_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let window_id = self
            .windows
            .iter()
            .nth(index)
            .map(|(window_id, _)| window_id);
        if let Some(window_id) = window_id {
            self.activate_window(window_id, window, cx);
        }
    }

    fn close_window(&mut self, window_id: WindowId, window: &mut Window, cx: &mut Context<Self>) {
        let was_active = self.windows.active_window_id() == window_id;
        self.window_menu = None;
        match self.windows.close_window(window_id) {
            Ok(CloseWindowOutcome::WindowClosed {
                closed_window_id,
                payload,
                active_window_id,
            }) => {
                debug_assert_eq!(closed_window_id, window_id);
                payload.update(cx, |pane_host, cx| pane_host.close_all(cx));
                if was_active {
                    let active_window = self.windows.active_window().clone();
                    if self.active {
                        active_window.update(cx, |pane_host, cx| pane_host.activate(window, cx));
                    } else {
                        active_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
                    }
                }
                debug_assert_eq!(active_window_id, self.windows.active_window_id());
                self.scroll_active_window_into_view();
                cx.emit(WindowManagerEvent::PresentationChanged);
                cx.notify();
            }
            Ok(CloseWindowOutcome::CloseOperatingSystemWindow) => {
                cx.emit(WindowManagerEvent::CloseWorkspaceRequested);
            }
            Err(error) => Self::report_window_error("close", error),
        }
    }

    fn open_window_menu(
        &mut self,
        window_id: WindowId,
        left: Option<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.activate_window(window_id, window, cx) {
            return;
        }
        let Some(pane_host) = self.windows.window(window_id) else {
            return;
        };
        let pane_host = pane_host.read(cx);
        self.window_menu = Some(WindowMenuState {
            window_id,
            left,
            zoomed: matches!(pane_host.zoom_state(), ZoomState::Zoomed(_)),
            zoom_enabled: pane_host.pane_count() > 1,
        });
        cx.notify();
    }

    fn toggle_active_window_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_id = self.windows.active_window_id();
        if self
            .window_menu
            .is_some_and(|menu| menu.window_id == window_id && menu.left.is_none())
        {
            self.window_menu = None;
            cx.notify();
            return;
        }
        self.open_window_menu(window_id, None, window, cx);
    }

    fn open_context_menu(
        &mut self,
        window_id: WindowId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let content_width = window.bounds().size.width;
        let maximum_left = (content_width - px(PANE_ACTION_MENU_WIDTH + WINDOW_CONTROL_INSET))
            .max(px(WINDOW_CONTROL_INSET));
        let left = event
            .position
            .x
            .clamp(px(WINDOW_CONTROL_INSET), maximum_left);
        self.open_window_menu(window_id, Some(left), window, cx);
    }

    fn perform_menu_command(
        &mut self,
        command: PaneActionMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.window_menu.take() else {
            return;
        };
        let Some(pane_host) = self.windows.window(menu.window_id).cloned() else {
            return;
        };

        match command {
            PaneActionMenuCommand::SplitRight => pane_host.update(cx, |pane_host, cx| {
                pane_host.split_focused(SplitAxis::Horizontal, window, cx);
            }),
            PaneActionMenuCommand::SplitDown => pane_host.update(cx, |pane_host, cx| {
                pane_host.split_focused(SplitAxis::Vertical, window, cx);
            }),
            PaneActionMenuCommand::ToggleZoom => {
                pane_host.update(cx, |pane_host, cx| pane_host.toggle_zoom(window, cx));
            }
            PaneActionMenuCommand::Close => self.close_window(menu.window_id, window, cx),
        }
        cx.notify();
    }

    fn dismiss_window_menu(&mut self, cx: &mut Context<Self>) {
        if self.window_menu.take().is_some() {
            cx.notify();
        }
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.close_window(self.windows.active_window_id(), window, cx);
    }

    fn on_create_window(&mut self, _: &CreateWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.create_window(window, cx);
    }

    fn on_activate_window_1(
        &mut self,
        _: &ActivateWindow1,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(0, window, cx);
    }

    fn on_activate_window_2(
        &mut self,
        _: &ActivateWindow2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(1, window, cx);
    }

    fn on_activate_window_3(
        &mut self,
        _: &ActivateWindow3,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(2, window, cx);
    }

    fn on_activate_window_4(
        &mut self,
        _: &ActivateWindow4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(3, window, cx);
    }

    fn on_activate_window_5(
        &mut self,
        _: &ActivateWindow5,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(4, window, cx);
    }

    fn on_activate_window_6(
        &mut self,
        _: &ActivateWindow6,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(5, window, cx);
    }

    fn on_activate_window_7(
        &mut self,
        _: &ActivateWindow7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(6, window, cx);
    }

    fn on_activate_window_8(
        &mut self,
        _: &ActivateWindow8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(7, window, cx);
    }

    fn on_activate_window_9(
        &mut self,
        _: &ActivateWindow9,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(8, window, cx);
    }

    fn render_window_item(
        &self,
        window_id: WindowId,
        title: SharedString,
        active: bool,
        manager: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let click_manager = manager.clone();
        let context_manager = manager.clone();
        let close_manager = manager;
        let window_group = format!("window-item-{}", window_id.get());
        div()
            .id(("window-item", window_id.get()))
            .debug_selector(move || {
                format!(
                    "window-item-{}-{}",
                    window_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .group(window_group.clone())
            .h_full()
            .flex_none()
            .w(px(WINDOW_ITEM_WIDTH))
            .min_w(px(WINDOW_ITEM_MINIMUM_WIDTH))
            .max_w(px(WINDOW_ITEM_MAXIMUM_WIDTH))
            .pl(px(12.0))
            .pr(px(WINDOW_ITEM_RIGHT_PADDING))
            .flex()
            .items_center()
            .cursor_pointer()
            .occlude()
            .bg(gpui_color(if active {
                ACTIVE_THEME.tab_active_background
            } else {
                ACTIVE_THEME.tab_inactive_background
            }))
            .text_size(px(12.0))
            .text_color(gpui_color(if active {
                ACTIVE_THEME.tab_active_foreground
            } else {
                ACTIVE_THEME.text_muted
            }))
            .hover(|item| item.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    manager.activate_window(window_id, window, cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                let _ = context_manager.update(cx, |manager, cx| {
                    manager.open_context_menu(window_id, event, window, cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .id(("window-title", window_id.get()))
                    .debug_selector(move || format!("window-title-{}", window_id.get()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .id(("window-close-button", window_id.get()))
                    .debug_selector(move || format!("window-close-button-{}", window_id.get()))
                    .size(px(WINDOW_CLOSE_CONTROL_SIZE))
                    .ml(px(4.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .occlude()
                    .when(!active, |button| {
                        button
                            .opacity(0.0)
                            .group_hover(window_group, |button| button.opacity(1.0))
                    })
                    .hover(|button| {
                        button
                            .opacity(1.0)
                            .bg(gpui_color(ACTIVE_THEME.ghost_element_hover))
                    })
                    .on_click(move |_, window, cx| {
                        let _ = close_manager.update(cx, |manager, cx| {
                            manager.close_window(window_id, window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("xmark")
                            .weight(SymbolWeight::Medium)
                            .size(px(WINDOW_CLOSE_ICON_SIZE))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .child(
                div()
                    .id(("window-item-divider", window_id.get()))
                    .debug_selector(move || format!("window-item-{}-divider", window_id.get()))
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(
                div()
                    .id(("window-item-bottom-divider", window_id.get()))
                    .debug_selector(move || {
                        format!("window-item-{}-bottom-divider", window_id.get())
                    })
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .when(active, |item| {
                item.child(
                    div()
                        .id(("window-item-underline", window_id.get()))
                        .debug_selector(move || {
                            format!("window-item-{}-underline", window_id.get())
                        })
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(WINDOW_BAR_DIVIDER_SIZE))
                        .bg(gpui_color(ACTIVE_THEME.panel_focused_border)),
                )
            })
            .into_any_element()
    }

    fn render_window_bar(&self, manager: gpui::WeakEntity<Self>, cx: &App) -> AnyElement {
        let active_window_id = self.windows.active_window_id();
        let mut items = div()
            .id("window-items")
            .h_full()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&self.window_bar_scroll_handle);
        for (window_id, pane_host) in self.windows.iter() {
            items = items.child(self.render_window_item(
                window_id,
                pane_host.read(cx).window_title(),
                window_id == active_window_id,
                manager.clone(),
            ));
        }

        let create_manager = manager.clone();
        let menu_manager = manager;
        div()
            .id("window-bar")
            .debug_selector(|| "window-bar".to_owned())
            .relative()
            .h(px(WINDOW_BAR_HEIGHT))
            .min_w_0()
            .flex_1()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .pr(px(WINDOW_CONTROL_SIZE + WINDOW_CONTROL_INSET * 2.0))
            .bg(gpui_color(ACTIVE_THEME.tab_bar_background))
            .on_mouse_down(MouseButton::Left, handle_top_chrome_mouse_down)
            .child(
                div()
                    .id("window-bar-divider")
                    .debug_selector(|| "window-bar-divider".to_owned())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(items)
            .child(
                div()
                    .id("create-window-button")
                    .debug_selector(|| "create-window-button".to_owned())
                    .size(px(WINDOW_CONTROL_SIZE))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .occlude()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let _ = create_manager.update(cx, |manager, cx| {
                            manager.create_window(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("plus")
                            .size(px(14.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .child(
                div()
                    .id("window-menu-button")
                    .debug_selector(|| "window-menu-button".to_owned())
                    .absolute()
                    .top(px(WINDOW_CONTROL_INSET))
                    .right(px(WINDOW_CONTROL_INSET))
                    .size(px(WINDOW_CONTROL_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .occlude()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let _ = menu_manager.update(cx, |manager, cx| {
                            manager.toggle_active_window_menu(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("ellipsis")
                            .size(px(16.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .into_any_element()
    }

    fn render_window_menu(
        &self,
        menu: WindowMenuState,
        manager: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let dismiss_manager = manager.clone();
        let menu_element = render_pane_action_menu(
            ("window-menu", menu.window_id.get()),
            menu.zoomed,
            menu.zoom_enabled,
            CloseTarget::Window,
            manager,
            |manager, command, window, cx| {
                manager.perform_menu_command(command, window, cx);
            },
        );
        let controls = div()
            .id(("window-menu-controls", menu.window_id.get()))
            .debug_selector(move || format!("window-menu-controls-{}", menu.window_id.get()))
            .absolute()
            .top(px(WINDOW_MENU_TOP))
            .w(px(PANE_ACTION_MENU_WIDTH))
            .h(px(PANE_ACTION_MENU_HEIGHT))
            .on_mouse_down_out(move |event, window, cx| {
                if window_menu_button_contains(event.position, window.bounds().size.width) {
                    return;
                }
                let _ = dismiss_manager.update(cx, |manager, cx| {
                    manager.dismiss_window_menu(cx);
                });
            })
            .child(menu_element);
        match menu.left {
            Some(left) => controls.left(left),
            None => controls.right(px(WINDOW_CONTROL_INSET)),
        }
        .into_any_element()
    }
}

impl Render for WindowManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.windows.len() > 0);
        let manager = cx.entity().downgrade();
        let active_window = self.windows.active_window().clone();
        let window_bar = self.render_window_bar(manager.clone(), cx);

        div()
            .id("window-manager")
            .debug_selector(|| "window-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .on_action(cx.listener(Self::on_create_window))
            .on_action(cx.listener(Self::on_activate_window_1))
            .on_action(cx.listener(Self::on_activate_window_2))
            .on_action(cx.listener(Self::on_activate_window_3))
            .on_action(cx.listener(Self::on_activate_window_4))
            .on_action(cx.listener(Self::on_activate_window_5))
            .on_action(cx.listener(Self::on_activate_window_6))
            .on_action(cx.listener(Self::on_activate_window_7))
            .on_action(cx.listener(Self::on_activate_window_8))
            .on_action(cx.listener(Self::on_activate_window_9))
            .on_action(cx.listener(Self::on_close_window))
            .child(
                div()
                    .h(px(TOP_CHROME_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id("window-manager-top-spacer")
                            .debug_selector(|| "window-manager-top-spacer".to_owned())
                            .w(px(WORKSPACE_SIDEBAR_WIDTH))
                            .h_full()
                            .flex_shrink_0()
                            .bg(gpui_color(ACTIVE_THEME.tab_bar_background)),
                    )
                    .child(window_bar),
            )
            .child(
                div()
                    .id("window-manager-content")
                    .debug_selector(|| "window-manager-content".to_owned())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .when(self.sidebar_visible, |body| {
                        body.ml(px(WORKSPACE_SIDEBAR_WIDTH))
                    })
                    .child(active_window),
            )
            .when_some(self.window_menu, |root, menu| {
                root.child(self.render_window_menu(menu, manager))
            })
    }
}

impl EventEmitter<WindowManagerEvent> for WindowManager {}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn window_menu_button_contains(position: Point<Pixels>, content_width: Pixels) -> bool {
    let left = content_width - px(WINDOW_CONTROL_INSET + WINDOW_CONTROL_SIZE);
    let right = content_width - px(WINDOW_CONTROL_INSET);
    let top = px(WINDOW_CONTROL_INSET);
    let bottom = top + px(WINDOW_CONTROL_SIZE);

    position.x >= left && position.x <= right && position.y >= top && position.y <= bottom
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::sync::Arc;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::domain::PaneId;
    use crate::terminal::{
        GridSize, KeyInput, PointerInput, ScreenSnapshot, SessionError, SessionEvent,
        StartedTerminalSession, TerminalSessionHandle, WheelInput,
    };

    #[derive(Clone)]
    struct SessionRecords {
        event_senders: Rc<RefCell<Vec<async_channel::Sender<SessionEvent>>>>,
        dropped_session_ids: Rc<RefCell<Vec<usize>>>,
        pointer_count: Rc<Cell<usize>>,
    }

    struct WindowSessionFactory {
        records: SessionRecords,
        next_session_id: Cell<usize>,
    }

    struct WindowSessionHandle {
        session_id: usize,
        dropped_session_ids: Rc<RefCell<Vec<usize>>>,
        pointer_count: Rc<Cell<usize>>,
    }

    impl TerminalSessionFactory for WindowSessionFactory {
        fn start(
            &self,
            _size: GridSize,
            _working_directory: &Path,
        ) -> Result<StartedTerminalSession, SessionError> {
            let session_id = self.next_session_id.get();
            self.next_session_id.set(session_id + 1);
            let (sender, events) = async_channel::unbounded();
            self.records.event_senders.borrow_mut().push(sender);
            Ok(StartedTerminalSession {
                handle: Box::new(WindowSessionHandle {
                    session_id,
                    dropped_session_ids: Rc::clone(&self.records.dropped_session_ids),
                    pointer_count: Rc::clone(&self.records.pointer_count),
                }),
                events,
            })
        }
    }

    impl Drop for WindowSessionHandle {
        fn drop(&mut self) {
            self.dropped_session_ids.borrow_mut().push(self.session_id);
        }
    }

    impl TerminalSessionHandle for WindowSessionHandle {
        fn key(&self, _input: KeyInput) {}

        fn resize(&self, _size: GridSize) {}

        fn pointer(&self, _input: PointerInput) {
            self.pointer_count.update(|count| count + 1);
        }

        fn wheel(&self, _input: WheelInput) {}

        fn scroll_to(&self, _offset_rows: u64) {}

        fn paste(&self, _text: String) {}

        fn request_selection_text(
            &self,
        ) -> async_channel::Receiver<Result<Option<String>, String>> {
            let (sender, receiver) = async_channel::bounded(1);
            let _ = sender.try_send(Ok(None));
            receiver
        }
    }

    fn window_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WindowManager>,
        SessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = SessionRecords {
            event_senders: Rc::new(RefCell::new(Vec::new())),
            dropped_session_ids: Rc::new(RefCell::new(Vec::new())),
            pointer_count: Rc::new(Cell::new(0)),
        };
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(WindowSessionFactory {
            records: records.clone(),
            next_session_id: Cell::new(1),
        });
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WindowManager::new(
                session_factory,
                PathBuf::from("/tmp/termspace-window-manager-test"),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
    }

    fn right_click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn window_bar_should_keep_dim_dividers_beneath_every_item_and_accent_the_active_window(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        let divider = cx
            .debug_bounds("window-bar-divider")
            .expect("the Window bar divider was not rendered");
        let underline = cx
            .debug_bounds("window-item-2-underline")
            .expect("the Active Window underline was not rendered");
        let inactive_item = cx
            .debug_bounds("window-item-1-inactive")
            .expect("the inactive Window item was not rendered");
        let active_item = cx
            .debug_bounds("window-item-2-active")
            .expect("the Active Window item was not rendered");
        let item_divider = cx
            .debug_bounds("window-item-1-divider")
            .expect("the Window item divider was not rendered");
        let inactive_bottom_divider = cx
            .debug_bounds("window-item-1-bottom-divider")
            .expect("the inactive Window bottom divider was not rendered");
        let active_bottom_divider = cx
            .debug_bounds("window-item-2-bottom-divider")
            .expect("the Active Window bottom divider was not rendered");

        assert_eq!(
            (
                bar.size.height,
                divider.size.height,
                underline.size.height,
                divider.origin.y + divider.size.height,
                item_divider.size.width,
                item_divider.size.height,
                item_divider.origin.x + item_divider.size.width,
                inactive_bottom_divider.origin.y,
                inactive_bottom_divider.size,
                active_bottom_divider.origin.y,
                active_bottom_divider.size,
            ),
            (
                px(WINDOW_BAR_HEIGHT),
                px(WINDOW_BAR_DIVIDER_SIZE),
                px(WINDOW_BAR_DIVIDER_SIZE),
                underline.origin.y + underline.size.height,
                px(WINDOW_BAR_DIVIDER_SIZE),
                inactive_item.size.height,
                inactive_item.origin.x + inactive_item.size.width,
                divider.origin.y,
                gpui::size(inactive_item.size.width, px(WINDOW_BAR_DIVIDER_SIZE)),
                divider.origin.y,
                gpui::size(active_item.size.width, px(WINDOW_BAR_DIVIDER_SIZE)),
            )
        );
    }

    #[gpui::test]
    fn window_bar_should_start_after_the_persistent_sidebar_chrome(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = window_manager(cx);
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let spacer = cx
            .debug_bounds("window-manager-top-spacer")
            .expect("the persistent top-left spacer was not rendered");
        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");

        assert_eq!(
            (spacer.origin, spacer.size, bar.origin.x),
            (
                root.origin,
                gpui::size(px(WORKSPACE_SIDEBAR_WIDTH), px(TOP_CHROME_HEIGHT)),
                root.origin.x + px(WORKSPACE_SIDEBAR_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn hiding_sidebar_should_expand_content_without_moving_the_window_bar(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let visible_content = cx
            .debug_bounds("window-manager-content")
            .expect("the Window content was not rendered");
        let visible_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");

        manager.update(cx, |manager, cx| manager.set_sidebar_visible(false, cx));
        cx.run_until_parked();

        let hidden_content = cx
            .debug_bounds("window-manager-content")
            .expect("the expanded Window content was not rendered");
        let hidden_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        assert_eq!(
            (
                visible_content.origin.x,
                hidden_content.origin.x,
                visible_bar.origin.x,
                hidden_bar.origin.x,
            ),
            (
                root.origin.x + px(WORKSPACE_SIDEBAR_WIDTH),
                root.origin.x,
                root.origin.x + px(WORKSPACE_SIDEBAR_WIDTH),
                root.origin.x + px(WORKSPACE_SIDEBAR_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn command_t_should_create_and_activate_a_new_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (2, WindowId::new(2), Vec::new()));
    }

    #[gpui::test]
    fn command_number_shortcuts_should_activate_windows_by_position(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        for _ in 1..9 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }

        let mut active_window_ids = Vec::new();
        for shortcut in [
            "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
        ] {
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            active_window_ids
                .push(manager.read_with(cx, |manager, _| manager.windows.active_window_id()));
        }

        assert_eq!(
            active_window_ids,
            (1..=9).map(WindowId::new).collect::<Vec<_>>()
        );
    }

    #[gpui::test]
    fn unavailable_command_number_shortcut_should_preserve_the_active_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);

        cx.simulate_keystrokes("cmd-9");
        cx.run_until_parked();

        let active_window_id =
            manager.read_with(cx, |manager, _| manager.windows.active_window_id());
        assert_eq!(active_window_id, WindowId::new(1));
    }

    #[gpui::test]
    fn create_button_should_create_and_activate_without_dropping_the_inactive_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let first_entity_id =
            manager.read_with(cx, |manager, _| manager.windows.active_window().entity_id());

        click("create-window-button", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager
                    .windows
                    .window(WindowId::new(1))
                    .map(Entity::entity_id),
                manager
                    .windows
                    .window(WindowId::new(1))
                    .is_some_and(|window| !window.read(cx).is_active()),
                manager.windows.active_window().read(cx).is_active(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WindowId::new(2),
                Some(first_entity_id),
                true,
                true,
                Vec::new(),
            )
        );
    }

    #[gpui::test]
    fn single_pane_window_title_should_follow_the_terminal_title(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        let sender = records
            .event_senders
            .borrow()
            .first()
            .cloned()
            .expect("the initial Window session must have started");

        sender
            .try_send(SessionEvent::Screen(Arc::new(ScreenSnapshot {
                rows: Arc::from([]),
                background: ACTIVE_THEME.terminal_background,
                scrollbar: Default::default(),
                title: Arc::from("Claude Code"),
            })))
            .unwrap();
        cx.run_until_parked();

        let title = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).window_title()
        });
        assert_eq!(title.as_ref(), "Claude Code");
    }

    #[gpui::test]
    fn split_window_title_should_show_the_count_and_restore_the_terminal_title_after_close(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let sender = records
            .event_senders
            .borrow()
            .first()
            .cloned()
            .expect("the initial Window session must have started");
        sender
            .try_send(SessionEvent::Screen(Arc::new(ScreenSnapshot {
                rows: Arc::from([]),
                background: ACTIVE_THEME.terminal_background,
                scrollbar: Default::default(),
                title: Arc::from("Claude Code"),
            })))
            .unwrap();
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let split_title = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).window_title()
        });
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let restored_title = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).window_title()
        });

        assert_eq!(
            (split_title.as_ref(), restored_title.as_ref()),
            ("2 Panes", "Claude Code")
        );
    }

    #[gpui::test]
    fn hover_close_button_should_close_its_window_without_activating_it(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-close-button-1", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WindowId::new(2), vec![1]));
    }

    #[gpui::test]
    fn active_window_close_button_should_close_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-close-button-2", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn window_close_button_should_use_a_compact_right_inset(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = window_manager(cx);
        let item = cx
            .debug_bounds("window-item-1-active")
            .expect("the Active Window item was not rendered");
        let close_button = cx
            .debug_bounds("window-close-button-1")
            .expect("the Active Window close button was not rendered");

        assert_eq!(
            (
                item.origin.x + item.size.width - (close_button.origin.x + close_button.size.width),
                close_button.size,
            ),
            (
                px(WINDOW_ITEM_RIGHT_PADDING),
                gpui::size(px(WINDOW_CLOSE_CONTROL_SIZE), px(WINDOW_CLOSE_CONTROL_SIZE)),
            )
        );
    }

    #[gpui::test]
    fn creating_windows_should_scroll_the_active_window_into_view(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);

        for _ in 0..20 {
            click("create-window-button", cx);
        }

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager.window_bar_scroll_handle.offset().x,
            )
        });
        assert_eq!((state.0, state.1), (21, WindowId::new(21)));
        assert!(
            state.2 < px(0.0),
            "the Window bar did not scroll; offset was {:?}",
            state.2
        );
    }

    #[gpui::test]
    fn activating_an_inactive_window_should_restore_its_focused_pane(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);

        click("window-item-1-inactive", cx);

        let first_window = manager.read_with(cx, |manager, _| {
            manager
                .windows
                .window(WindowId::new(1))
                .cloned()
                .expect("Window 1 must remain owned")
        });
        let state = cx.update(|window, cx| {
            let pane_host = first_window.read(cx);
            (
                manager.read(cx).windows.active_window_id(),
                pane_host.focused_pane_id(),
                pane_host.focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (WindowId::new(1), PaneId::new(2), true));
    }

    #[gpui::test]
    fn right_click_should_activate_and_target_the_clicked_window(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        right_click("window-item-1-inactive", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.active_window_id(),
                manager
                    .window_menu
                    .map(|menu| (menu.window_id, menu.left.is_some())),
            )
        });
        assert_eq!(state, (WindowId::new(1), Some((WindowId::new(1), true))));
    }

    #[gpui::test]
    fn top_ellipsis_should_target_the_active_window(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-menu-button", cx);

        let menu = manager.read_with(cx, |manager, _| manager.window_menu);
        assert_eq!(
            menu.map(|menu| (menu.window_id, menu.left)),
            Some((WindowId::new(2), None))
        );
    }

    #[gpui::test]
    fn top_ellipsis_should_toggle_its_open_menu_closed(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        click("window-menu-button", cx);

        let menu = manager.read_with(cx, |manager, _| manager.window_menu);
        assert_eq!(menu, None);
    }

    #[gpui::test]
    fn window_menu_split_should_target_the_selected_window_without_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        right_click("window-item-1-inactive", cx);

        click("window-menu-row-split-right", cx);

        let pane_counts = manager.read_with(cx, |manager, cx| {
            (
                manager
                    .windows
                    .window(WindowId::new(1))
                    .expect("Window 1 must remain owned")
                    .read(cx)
                    .pane_count(),
                manager
                    .windows
                    .window(WindowId::new(2))
                    .expect("Window 2 must remain owned")
                    .read(cx)
                    .pane_count(),
                records.pointer_count.get(),
            )
        });
        assert_eq!(pane_counts, (2, 1, 0));
    }

    #[gpui::test]
    fn single_pane_window_menu_should_disable_zoom_without_dismissing_the_menu(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        click("window-menu-row-toggle-zoom", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.window_menu.is_some(),
                manager.windows.active_window().read(cx).zoom_state(),
            )
        });
        assert_eq!(state, (true, ZoomState::Restored));
    }

    #[gpui::test]
    fn close_window_menu_should_focus_the_neighbor_and_drop_the_closed_session_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        click("window-menu-button", cx);

        click("window-menu-row-close-window", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn closing_an_inactive_window_should_preserve_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_window(WindowId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (WindowId::new(2), vec![1]));
    }

    #[gpui::test]
    fn inactive_shell_exit_should_close_its_window_without_stealing_focus(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let first_sender = records
            .event_senders
            .borrow()
            .first()
            .cloned()
            .expect("Window 1 session must have started");

        first_sender
            .try_send(SessionEvent::Exited("Shell exited".to_owned()))
            .unwrap();
        cx.run_until_parked();

        let active_window =
            manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).windows.active_window_id(),
                active_window
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (WindowId::new(2), true, vec![1]));
    }

    #[gpui::test]
    fn inactive_workspace_active_window_exit_should_leave_its_fallback_deactivated_and_unfocused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let active_sender = records
            .event_senders
            .borrow()
            .get(1)
            .cloned()
            .expect("Window 2 session must have started");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
            window.blur();
        });

        active_sender
            .try_send(SessionEvent::Exited("Shell exited".to_owned()))
            .unwrap();
        cx.run_until_parked();

        let fallback = manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).active,
                manager.read(cx).windows.len(),
                manager.read(cx).windows.active_window_id(),
                fallback.read(cx).is_active(),
                fallback.read(cx).focused_terminal_is_focused(window, cx),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (false, 1, WindowId::new(1), false, false, vec![2]));
    }

    #[gpui::test]
    fn active_shell_exit_should_close_its_window_and_focus_the_neighbor(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let active_sender = records
            .event_senders
            .borrow()
            .get(1)
            .cloned()
            .expect("Window 2 session must have started");

        active_sender
            .try_send(SessionEvent::Exited("Shell exited".to_owned()))
            .unwrap();
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn closing_a_multi_pane_window_should_close_every_owned_session_exactly_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_window(WindowId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids.borrow().clone();
        dropped.sort_unstable();
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_w_should_close_only_the_focused_pane_when_the_window_is_split(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window().read(cx).pane_count(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, 1, vec![2]));
    }

    #[gpui::test]
    fn command_w_should_close_the_active_window_when_its_last_pane_closes(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn command_shift_w_should_close_every_pane_in_only_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);
        click("window-item-1-inactive", cx);

        cx.simulate_keystrokes("cmd-shift-w");
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids.borrow().clone();
        dropped.sort_unstable();
        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager.windows.active_window().read(cx).pane_count(),
            )
        });
        assert_eq!(state, (1, WindowId::new(2), 1));
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_shift_w_should_request_owning_workspace_close_for_the_final_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let close_requests = Rc::new(Cell::new(0));
        let close_requests_for_subscription = Rc::clone(&close_requests);
        manager.update(cx, |_, cx| {
            cx.subscribe(&manager, move |_, _, event: &WindowManagerEvent, _| {
                if matches!(event, WindowManagerEvent::CloseWorkspaceRequested) {
                    close_requests_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.simulate_keystrokes("cmd-shift-w");

        assert_eq!(
            (
                close_requests.get(),
                records.dropped_session_ids.borrow().clone()
            ),
            (1, Vec::new())
        );
    }

    #[gpui::test]
    fn window_menu_should_overlap_the_bar_and_clamp_inside_the_content_width(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = window_manager(cx);
        right_click("window-item-1-active", cx);

        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        let menu = cx
            .debug_bounds("window-menu-1")
            .expect("the Window menu was not rendered");
        let item = cx
            .debug_bounds("window-item-1-active")
            .expect("the Window item was not rendered");
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let local_pointer_x = item.center().x - root.origin.x;
        let maximum_left = (root.size.width - px(PANE_ACTION_MENU_WIDTH + WINDOW_CONTROL_INSET))
            .max(px(WINDOW_CONTROL_INSET));
        let expected_menu_x =
            root.origin.x + local_pointer_x.clamp(px(WINDOW_CONTROL_INSET), maximum_left);

        assert_eq!(
            (
                bar.origin.y + bar.size.height - menu.origin.y,
                menu.origin.x,
                menu.origin.x >= root.origin.x,
                menu.origin.x + menu.size.width <= root.origin.x + root.size.width,
            ),
            (px(WINDOW_MENU_BAR_OVERLAP), expected_menu_x, true, true)
        );
    }
}
