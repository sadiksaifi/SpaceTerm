use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DefiniteLength, DragMoveEvent, Empty, Entity, MouseDownEvent,
    Pixels, Point, Render, Window, div, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};

use super::{ClosePane, SplitDown, SplitRight, TERMINAL_KEY_CONTEXT, TerminalPane, TogglePaneZoom};
use crate::domain::{
    ClosePaneOutcome, PaneId, PaneNodeRef, PaneSize, PaneTreeRef, SplitAxis, SplitId,
    TerminalWindow, WindowId, ZoomState,
};
use crate::terminal::TerminalSessionFactory;
use crate::theme::{ACTIVE_THEME, Color};

const MINIMUM_PANE_WIDTH: f32 = MENU_WIDTH + PANE_CONTROL_INSET * 2.0;
const DIVIDER_SIZE: f32 = 1.0;
const DIVIDER_HIT_SIZE: f32 = 8.0;
const PANE_CONTROL_INSET: f32 = 8.0;
const PANE_CONTROL_SIZE: f32 = 28.0;
const MENU_WIDTH: f32 = 248.0;
const MENU_ROW_HEIGHT: f32 = 28.0;
const MENU_ITEM_COUNT: usize = 4;
const MENU_SEPARATOR_SIZE: f32 = 1.0;
const MENU_BORDER_SIZE: f32 = 1.0;
const MENU_CORNER_RADIUS: f32 = 8.0;
const MENU_INNER_CORNER_RADIUS: f32 = MENU_CORNER_RADIUS - MENU_BORDER_SIZE;
const MENU_SHORTCUT_TEXT_SIZE: f32 = 11.0;
const MENU_TOP: f32 = PANE_CONTROL_INSET + PANE_CONTROL_SIZE + 4.0;
const MENU_HEIGHT: f32 =
    MENU_ROW_HEIGHT * MENU_ITEM_COUNT as f32 + MENU_SEPARATOR_SIZE + MENU_BORDER_SIZE * 2.0;
const MINIMUM_PANE_HEIGHT: f32 = MENU_TOP + MENU_HEIGHT + PANE_CONTROL_INSET;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum PaneMenuCommand {
    SplitRight,
    SplitDown,
    ToggleZoom,
    ClosePane,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaneMenuItemSpec {
    command: PaneMenuCommand,
    icon: &'static str,
    label: &'static str,
    shortcut: &'static str,
    destructive: bool,
    separator_before: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneMenuRowPosition {
    First,
    Middle,
    Last,
}

#[derive(Clone, Copy)]
struct DraggedSplit {
    split_id: SplitId,
}

pub(crate) struct PaneHost {
    terminal_window: TerminalWindow<Entity<TerminalPane>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    next_pane_id: u64,
    next_split_id: u64,
    pane_bounds: BTreeMap<PaneId, Bounds<Pixels>>,
    split_bounds: BTreeMap<SplitId, Bounds<Pixels>>,
    menu_pane_id: Option<PaneId>,
}

impl PaneHost {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let minimum_pane_size = match PaneSize::new(MINIMUM_PANE_WIDTH, MINIMUM_PANE_HEIGHT) {
            Ok(size) => size,
            Err(error) => {
                unreachable!("fixed minimum Pane dimensions must be valid: {error}")
            }
        };
        let initial_pane_id = PaneId::new(1);
        let initial_terminal =
            cx.new(|cx| TerminalPane::new(Rc::clone(&session_factory), window, cx));

        Self {
            terminal_window: TerminalWindow::new(
                WindowId::new(1),
                initial_pane_id,
                initial_terminal,
                minimum_pane_size,
            ),
            session_factory,
            next_pane_id: 2,
            next_split_id: 1,
            pane_bounds: BTreeMap::new(),
            split_bounds: BTreeMap::new(),
            menu_pane_id: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let Some(terminal) = self
            .terminal_window
            .terminal(self.terminal_window.focused_pane_id())
        else {
            return;
        };
        terminal.update(cx, |terminal, _| terminal.focus(window));
    }

    fn focus_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if let Err(error) = self.terminal_window.focus_pane(pane_id) {
            eprintln!("failed to focus Pane: {error}");
            return;
        }
        self.menu_pane_id = None;
        cx.notify();
    }

    fn split_focused(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        let focused_pane_id = self.terminal_window.focused_pane_id();
        self.split_pane(focused_pane_id, axis, window, cx);
    }

    fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target_bounds) = self.pane_bounds.get(&target_pane_id).copied() else {
            eprintln!("cannot split Pane {target_pane_id} before its bounds are measured");
            return;
        };
        let Ok(target_size) = pane_size(target_bounds) else {
            eprintln!("cannot split Pane {target_pane_id} with invalid measured bounds");
            return;
        };
        let Some(new_pane_id) = self.allocate_pane_id() else {
            eprintln!("cannot split Pane because the Pane ID space is exhausted");
            return;
        };
        let Some(split_id) = self.allocate_split_id() else {
            eprintln!("cannot split Pane because the split ID space is exhausted");
            return;
        };
        let session_factory = Rc::clone(&self.session_factory);
        let result = self.terminal_window.split_pane(
            target_pane_id,
            new_pane_id,
            split_id,
            axis,
            target_size,
            DIVIDER_SIZE,
            || cx.new(|cx| TerminalPane::new(session_factory, window, cx)),
        );

        match result {
            Ok(pane_id) => {
                self.menu_pane_id = None;
                self.split_bounds.clear();
                cx.notify();
                if let Some(terminal) = self.terminal_window.terminal(pane_id) {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to split Pane: {error}"),
        }
    }

    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane(self.terminal_window.focused_pane_id(), window, cx);
    }

    fn close_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.terminal_window.terminal(pane_id).cloned();
        match self.terminal_window.close_pane(pane_id) {
            Ok(ClosePaneOutcome::CloseWindow) => {
                if let Some(terminal) = terminal {
                    terminal.update(cx, |terminal, _| terminal.close());
                }
                window.remove_window();
            }
            Ok(ClosePaneOutcome::PaneClosed {
                focused_pane_id, ..
            }) => {
                if let Some(terminal) = terminal {
                    terminal.update(cx, |terminal, _| terminal.close());
                }
                self.pane_bounds.remove(&pane_id);
                self.split_bounds.clear();
                self.menu_pane_id = None;
                cx.notify();
                if let Some(terminal) = self.terminal_window.terminal(focused_pane_id) {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to close Pane: {error}"),
        }
    }

    fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_window.pane_count() <= 1 {
            return;
        }
        self.terminal_window.toggle_zoom();
        self.menu_pane_id = None;
        cx.notify();
        self.focus(window, cx);
    }

    fn resize_split(
        &mut self,
        split_id: SplitId,
        axis: SplitAxis,
        bounds: Bounds<Pixels>,
        pointer: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(requested_ratio) = split_ratio_for_pointer(axis, bounds, pointer) else {
            return;
        };
        let Ok(available_size) = pane_size(bounds) else {
            return;
        };
        match self.terminal_window.resize_split(
            split_id,
            available_size,
            DIVIDER_SIZE,
            requested_ratio,
        ) {
            Ok(_) => cx.notify(),
            Err(error) => eprintln!("failed to resize split: {error}"),
        }
    }

    fn toggle_menu(
        &mut self,
        pane_id: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalPane>> {
        if let Err(error) = self.terminal_window.focus_pane(pane_id) {
            eprintln!("failed to focus Pane: {error}");
            return None;
        }
        self.menu_pane_id = (self.menu_pane_id != Some(pane_id)).then_some(pane_id);
        cx.notify();
        self.terminal_window.terminal(pane_id).cloned()
    }

    fn perform_menu_command(
        &mut self,
        command: PaneMenuCommand,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu_pane_id = None;
        match command {
            PaneMenuCommand::SplitRight => {
                self.split_pane(pane_id, SplitAxis::Horizontal, window, cx)
            }
            PaneMenuCommand::SplitDown => self.split_pane(pane_id, SplitAxis::Vertical, window, cx),
            PaneMenuCommand::ToggleZoom => self.toggle_zoom(window, cx),
            PaneMenuCommand::ClosePane => self.close_pane(pane_id, window, cx),
        }
    }

    fn allocate_pane_id(&mut self) -> Option<PaneId> {
        let id = PaneId::new(self.next_pane_id);
        self.next_pane_id = self.next_pane_id.checked_add(1)?;
        Some(id)
    }

    fn allocate_split_id(&mut self) -> Option<SplitId> {
        let id = SplitId::new(self.next_split_id);
        self.next_split_id = self.next_split_id.checked_add(1)?;
        Some(id)
    }

    fn on_split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitAxis::Horizontal, window, cx);
    }

    fn on_split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitAxis::Vertical, window, cx);
    }

    fn on_toggle_zoom(&mut self, _: &TogglePaneZoom, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_zoom(window, cx);
    }

    fn on_close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.close_focused(window, cx);
    }

    fn render_tree(&self, tree: PaneTreeRef<'_>, host: gpui::WeakEntity<Self>) -> AnyElement {
        match tree.node() {
            PaneNodeRef::Leaf { pane_id } => self.render_leaf(pane_id, host),
            PaneNodeRef::Split {
                split_id,
                axis,
                ratio,
                first,
                second,
            } => self.render_split(split_id, axis, ratio, first, second, host),
        }
    }

    fn render_leaf(&self, pane_id: PaneId, host: gpui::WeakEntity<Self>) -> AnyElement {
        let Some(terminal) = self.terminal_window.terminal(pane_id).cloned() else {
            return div()
                .size_full()
                .bg(gpui_color(ACTIVE_THEME.terminal_background))
                .into_any_element();
        };
        let focused = self.terminal_window.focused_pane_id() == pane_id;
        let has_multiple_panes = self.terminal_window.pane_count() > 1;
        let pane_group = format!("pane-group-{}", pane_id.get());
        let measure_host = host.clone();
        let focus_host = host.clone();
        let focus_terminal = terminal.clone();

        div()
            .on_children_prepainted(move |children, _, cx| {
                let Some(bounds) = children.first().copied() else {
                    return;
                };
                let _ = measure_host.update(cx, |host, _| {
                    host.pane_bounds.insert(pane_id, bounds);
                });
            })
            .id(("pane", pane_id.get()))
            .group(pane_group.clone())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .capture_any_mouse_down(move |_: &MouseDownEvent, window, cx| {
                let _ = focus_host.update(cx, |host, cx| host.focus_pane(pane_id, cx));
                focus_terminal.update(cx, |terminal, _| terminal.focus(window));
            })
            .child(terminal)
            .when(has_multiple_panes && focused, |pane| {
                pane.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border(px(1.0))
                        .border_color(gpui_color(ACTIVE_THEME.panel_focused_border)),
                )
            })
            .when(has_multiple_panes, |pane| {
                pane.child(render_pane_controls(
                    pane_id,
                    focused,
                    self.menu_pane_id == Some(pane_id),
                    matches!(self.terminal_window.zoom_state(), ZoomState::Zoomed(_)),
                    &pane_group,
                    host.clone(),
                ))
            })
            .into_any_element()
    }

    fn render_split(
        &self,
        split_id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: PaneTreeRef<'_>,
        second: PaneTreeRef<'_>,
        host: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let first = self.render_tree(first, host.clone());
        let second = self.render_tree(second, host.clone());
        let drag_host = host.clone();
        let measure_host = host;
        let mut split = div()
            .on_children_prepainted(move |children, _, cx| {
                let (Some(first), Some(last)) = (children.first(), children.last()) else {
                    return;
                };
                let bounds = first.union(last);
                let _ = measure_host.update(cx, |host, _| {
                    host.split_bounds.insert(split_id, bounds);
                });
            })
            .id(("split", split_id.get()))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .on_drag_move::<DraggedSplit>(move |event: &DragMoveEvent<DraggedSplit>, _, cx| {
                if event.drag(cx).split_id != split_id {
                    return;
                }
                let _ = drag_host.update(cx, |host, cx| {
                    let bounds = host
                        .split_bounds
                        .get(&split_id)
                        .copied()
                        .unwrap_or(event.bounds);
                    host.resize_split(split_id, axis, bounds, event.event.position, cx);
                });
            });
        split = match axis {
            SplitAxis::Horizontal => split.flex_row(),
            SplitAxis::Vertical => split.flex_col(),
        };

        split
            .child(split_child(first, axis, ratio))
            .child(render_divider(split_id, axis))
            .child(split_child(second, axis, 1.0 - ratio))
            .into_any_element()
    }
}

impl Render for PaneHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = cx.entity().downgrade();
        let zoom_state = self.terminal_window.zoom_state();
        let pane_count = self.terminal_window.pane_count();
        let minimum_size = match zoom_state {
            ZoomState::Zoomed(_) => self.terminal_window.minimum_pane_size(),
            ZoomState::Restored => match self.terminal_window.minimum_size(DIVIDER_SIZE) {
                Ok(size) => size,
                Err(error) => {
                    eprintln!("failed to calculate minimum Pane layout size: {error}");
                    self.terminal_window.minimum_pane_size()
                }
            },
        };
        let content = match zoom_state {
            ZoomState::Restored => self.render_tree(self.terminal_window.root(), host.clone()),
            ZoomState::Zoomed(pane_id) => self.render_leaf(pane_id, host),
        };

        div()
            .id(("pane-host", self.terminal_window.id().get()))
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w(px(minimum_size.width()))
            .min_h(px(minimum_size.height()))
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_toggle_zoom))
            .on_action(cx.listener(Self::on_close_pane))
            .child(content)
            .when(matches!(zoom_state, ZoomState::Zoomed(_)), |root| {
                root.child(render_zoom_badge(pane_count))
            })
    }
}

fn split_child(child: AnyElement, axis: SplitAxis, ratio: f32) -> impl IntoElement {
    let child = div()
        .flex_basis(DefiniteLength::Fraction(ratio))
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(child);

    match axis {
        SplitAxis::Horizontal => child.h_full(),
        SplitAxis::Vertical => child.w_full(),
    }
}

fn render_divider(split_id: SplitId, axis: SplitAxis) -> AnyElement {
    let mut divider = div()
        .id(("split-divider", split_id.get()))
        .relative()
        .flex_shrink_0()
        .bg(gpui_color(ACTIVE_THEME.border));
    divider = match axis {
        SplitAxis::Horizontal => divider.w(px(DIVIDER_SIZE)).h_full(),
        SplitAxis::Vertical => divider.h(px(DIVIDER_SIZE)).w_full(),
    };

    let mut hit_target = div()
        .id(("split-divider-hit", split_id.get()))
        .absolute()
        .block_mouse_except_scroll()
        .on_drag(DraggedSplit { split_id }, |_, _, _, cx| cx.new(|_| Empty));
    hit_target = match axis {
        SplitAxis::Horizontal => hit_target
            .left(px(-(DIVIDER_HIT_SIZE - DIVIDER_SIZE) / 2.0))
            .w(px(DIVIDER_HIT_SIZE))
            .h_full()
            .cursor_col_resize(),
        SplitAxis::Vertical => hit_target
            .top(px(-(DIVIDER_HIT_SIZE - DIVIDER_SIZE) / 2.0))
            .h(px(DIVIDER_HIT_SIZE))
            .w_full()
            .cursor_row_resize(),
    };

    divider.child(hit_target).into_any_element()
}

fn render_menu_button(
    pane_id: PaneId,
    focused: bool,
    pane_group: &str,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let button_host = host;
    div()
        .id(("pane-menu-button", pane_id.get()))
        .debug_selector(|| format!("pane-menu-button-{}", pane_id.get()))
        .absolute()
        .top_0()
        .right_0()
        .size(px(PANE_CONTROL_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .occlude()
        .when(!focused, |button| {
            button
                .opacity(0.0)
                .group_hover(pane_group.to_owned(), |button| button.opacity(1.0))
        })
        .hover(|button| {
            button
                .opacity(1.0)
                .bg(gpui_color(ACTIVE_THEME.ghost_element_hover))
        })
        .on_click(move |_, window, cx| {
            let terminal = button_host
                .update(cx, |host, cx| host.toggle_menu(pane_id, cx))
                .ok()
                .flatten();
            if let Some(terminal) = terminal {
                terminal.update(cx, |terminal, _| terminal.focus(window));
            }
            cx.stop_propagation();
        })
        .child(
            Icon::new("ellipsis")
                .size(px(16.0))
                .color(gpui_color(ACTIVE_THEME.icon)),
        )
        .into_any_element()
}

fn render_pane_controls(
    pane_id: PaneId,
    focused: bool,
    show_menu: bool,
    zoomed: bool,
    pane_group: &str,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let dismiss_host = host.clone();

    div()
        .id(("pane-controls", pane_id.get()))
        .absolute()
        .top(px(PANE_CONTROL_INSET))
        .right(px(PANE_CONTROL_INSET))
        .w(px(if show_menu {
            MENU_WIDTH
        } else {
            PANE_CONTROL_SIZE
        }))
        .h(px(if show_menu {
            PANE_CONTROL_SIZE + 4.0 + MENU_HEIGHT
        } else {
            PANE_CONTROL_SIZE
        }))
        .when(show_menu, |controls| {
            controls.on_mouse_down_out(move |_, _, cx| {
                let _ = dismiss_host.update(cx, |host, cx| {
                    host.menu_pane_id = None;
                    cx.notify();
                });
            })
        })
        .child(render_menu_button(
            pane_id,
            focused,
            pane_group,
            host.clone(),
        ))
        .when(show_menu, |controls| {
            controls.child(render_pane_menu(pane_id, zoomed, host))
        })
        .into_any_element()
}

fn render_pane_menu(pane_id: PaneId, zoomed: bool, host: gpui::WeakEntity<PaneHost>) -> AnyElement {
    let mut menu = div()
        .id(("pane-menu", pane_id.get()))
        .debug_selector(|| format!("pane-menu-{}", pane_id.get()))
        .absolute()
        .top(px(PANE_CONTROL_SIZE + 4.0))
        .right_0()
        .w(px(MENU_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(MENU_CORNER_RADIUS))
        .border(px(MENU_BORDER_SIZE))
        .border_color(gpui_color(ACTIVE_THEME.border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .occlude();

    for (index, spec) in pane_menu_items(zoomed).into_iter().enumerate() {
        if spec.separator_before {
            menu = menu.child(
                div()
                    .h(px(MENU_SEPARATOR_SIZE))
                    .mx(px(8.0))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            );
        }
        let position = if index == 0 {
            PaneMenuRowPosition::First
        } else if index == MENU_ITEM_COUNT - 1 {
            PaneMenuRowPosition::Last
        } else {
            PaneMenuRowPosition::Middle
        };
        menu = menu.child(render_menu_row(spec, position, pane_id, host.clone()));
    }

    menu.into_any_element()
}

fn render_menu_row(
    spec: PaneMenuItemSpec,
    position: PaneMenuRowPosition,
    pane_id: PaneId,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let foreground = if spec.destructive {
        ACTIVE_THEME.error
    } else {
        ACTIVE_THEME.text
    };
    let icon_color = if spec.destructive {
        ACTIVE_THEME.error
    } else {
        ACTIVE_THEME.icon
    };

    div()
        .id(spec.command as usize)
        .debug_selector(|| format!("pane-menu-row-{}", spec.command.debug_name()))
        .h(px(MENU_ROW_HEIGHT))
        .px(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .cursor_pointer()
        .text_size(px(13.0))
        .text_color(gpui_color(foreground))
        .when(position == PaneMenuRowPosition::First, |row| {
            row.rounded_t(px(MENU_INNER_CORNER_RADIUS))
        })
        .when(position == PaneMenuRowPosition::Last, |row| {
            row.rounded_b(px(MENU_INNER_CORNER_RADIUS))
        })
        .hover(|row| row.bg(gpui_color(ACTIVE_THEME.element_hover)))
        .on_click(move |_, window, cx| {
            let _ = host.update(cx, |host, cx| {
                host.perform_menu_command(spec.command, pane_id, window, cx);
            });
            cx.stop_propagation();
        })
        .child(
            div()
                .w(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(spec.icon)
                        .size(px(15.0))
                        .color(gpui_color(icon_color)),
                ),
        )
        .child(spec.label)
        .child(div().flex_grow())
        .child(
            div()
                .text_size(px(MENU_SHORTCUT_TEXT_SIZE))
                .text_color(gpui_color(ACTIVE_THEME.icon))
                .child(spec.shortcut),
        )
        .into_any_element()
}

impl PaneMenuCommand {
    fn debug_name(self) -> &'static str {
        match self {
            Self::SplitRight => "split-right",
            Self::SplitDown => "split-down",
            Self::ToggleZoom => "toggle-zoom",
            Self::ClosePane => "close-pane",
        }
    }
}

fn render_zoom_badge(pane_count: usize) -> AnyElement {
    div()
        .absolute()
        .top(px(PANE_CONTROL_INSET))
        .left(px(PANE_CONTROL_INSET))
        .h(px(PANE_CONTROL_SIZE))
        .px(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .rounded(px(6.0))
        .border(px(1.0))
        .border_color(gpui_color(ACTIVE_THEME.border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .text_size(px(12.0))
        .text_color(gpui_color(ACTIVE_THEME.text_muted))
        .child(
            Icon::new("arrow.down.right.and.arrow.up.left")
                .weight(SymbolWeight::Medium)
                .size(px(14.0))
                .color(gpui_color(ACTIVE_THEME.icon)),
        )
        .child(format!("{pane_count} Panes · Zoomed"))
        .into_any_element()
}

fn pane_menu_items(zoomed: bool) -> [PaneMenuItemSpec; MENU_ITEM_COUNT] {
    [
        PaneMenuItemSpec {
            command: PaneMenuCommand::SplitRight,
            icon: "rectangle.split.2x1",
            label: "Split Right",
            shortcut: "⌘D",
            destructive: false,
            separator_before: false,
        },
        PaneMenuItemSpec {
            command: PaneMenuCommand::SplitDown,
            icon: "rectangle.split.1x2",
            label: "Split Down",
            shortcut: "⇧⌘D",
            destructive: false,
            separator_before: false,
        },
        PaneMenuItemSpec {
            command: PaneMenuCommand::ToggleZoom,
            icon: if zoomed {
                "arrow.down.right.and.arrow.up.left"
            } else {
                "arrow.up.left.and.arrow.down.right"
            },
            label: if zoomed { "Restore Panes" } else { "Zoom Pane" },
            shortcut: "⇧⌘↩",
            destructive: false,
            separator_before: false,
        },
        PaneMenuItemSpec {
            command: PaneMenuCommand::ClosePane,
            icon: "xmark",
            label: "Close Pane",
            shortcut: "⌘W",
            destructive: true,
            separator_before: true,
        },
    ]
}

fn pane_size(bounds: Bounds<Pixels>) -> Result<PaneSize, crate::domain::PaneSizeError> {
    PaneSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height))
}

fn split_ratio_for_pointer(
    axis: SplitAxis,
    bounds: Bounds<Pixels>,
    pointer: Point<Pixels>,
) -> Option<f32> {
    let (position, origin, extent) = match axis {
        SplitAxis::Horizontal => (
            f32::from(pointer.x),
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
        ),
        SplitAxis::Vertical => (
            f32::from(pointer.y),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.height),
        ),
    };
    let content_extent = extent - DIVIDER_SIZE;
    (content_extent > 0.0).then_some((position - origin) / content_extent)
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{Modifiers, TestAppContext, bounds, point, px, size};

    use super::*;
    use crate::terminal::{
        GridSize, KeyInput, PointerInput, SessionError, StartedTerminalSession,
        TerminalSessionHandle, WheelInput,
    };

    struct RecordingSessionFactory {
        pointer_count: Rc<Cell<usize>>,
    }

    impl TerminalSessionFactory for RecordingSessionFactory {
        fn start(&self, _size: GridSize) -> Result<StartedTerminalSession, SessionError> {
            let (_, events) = async_channel::unbounded();
            Ok(StartedTerminalSession {
                handle: Box::new(RecordingSessionHandle {
                    pointer_count: Rc::clone(&self.pointer_count),
                }),
                events,
            })
        }
    }

    struct RecordingSessionHandle {
        pointer_count: Rc<Cell<usize>>,
    }

    impl TerminalSessionHandle for RecordingSessionHandle {
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
            let (reply, receiver) = async_channel::bounded(1);
            let _ = reply.try_send(Ok(None));
            receiver
        }
    }

    #[gpui::test]
    fn menu_click_should_execute_command_without_sending_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        let pointer_count = Rc::new(Cell::new(0));
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(RecordingSessionFactory {
            pointer_count: Rc::clone(&pointer_count),
        });
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(session_factory, window, cx));

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let split_down = cx
            .debug_bounds("pane-menu-row-split-down")
            .map(|bounds| bounds.center());
        assert!(split_down.is_some(), "Split Down menu row was not rendered");
        if let Some(split_down) = split_down {
            cx.simulate_mouse_move(split_down, None, Modifiers::none());
            cx.simulate_click(split_down, Modifiers::none());
        }
        cx.run_until_parked();

        let state = host.read_with(cx, |host, _| {
            (
                host.terminal_window.pane_count(),
                host.menu_pane_id,
                pointer_count.get(),
            )
        });
        assert_eq!(state, (3, None, 0));
    }

    #[gpui::test]
    fn ellipsis_click_should_toggle_menu_without_sending_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        let pointer_count = Rc::new(Cell::new(0));
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(RecordingSessionFactory {
            pointer_count: Rc::clone(&pointer_count),
        });
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(session_factory, window, cx));

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
            cx.run_until_parked();
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
            cx.run_until_parked();
        }

        let state = host.read_with(cx, |host, _| (host.menu_pane_id, pointer_count.get()));
        assert_eq!(state, (None, 0));
    }

    #[gpui::test]
    fn opening_nonfocused_pane_menu_should_focus_and_zoom_the_target_pane(cx: &mut TestAppContext) {
        let pointer_count = Rc::new(Cell::new(0));
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(RecordingSessionFactory {
            pointer_count: Rc::clone(&pointer_count),
        });
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(session_factory, window, cx));

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let first_pane_id = PaneId::new(1);
        let menu_button = cx
            .debug_bounds("pane-menu-button-1")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "nonfocused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let zoom_row = cx
            .debug_bounds("pane-menu-row-toggle-zoom")
            .map(|bounds| bounds.center());
        assert!(zoom_row.is_some(), "Zoom Pane menu row was not rendered");
        if let Some(zoom_row) = zoom_row {
            cx.simulate_mouse_move(zoom_row, None, Modifiers::none());
            cx.simulate_click(zoom_row, Modifiers::none());
        }
        cx.run_until_parked();

        let terminal = host.read_with(cx, |host, _| {
            host.terminal_window.terminal(first_pane_id).cloned()
        });
        let terminal_is_focused = cx.update(|window, cx| {
            terminal
                .as_ref()
                .is_some_and(|terminal| terminal.read(cx).is_focused(window))
        });
        let state = host.read_with(cx, |host, _| {
            (
                host.terminal_window.focused_pane_id(),
                host.terminal_window.zoom_state(),
                terminal_is_focused,
                pointer_count.get(),
            )
        });

        assert_eq!(
            state,
            (first_pane_id, ZoomState::Zoomed(first_pane_id), true, 0)
        );
    }

    #[gpui::test]
    fn pane_menu_should_render_compact_row_and_menu_heights(cx: &mut TestAppContext) {
        let pointer_count = Rc::new(Cell::new(0));
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(RecordingSessionFactory {
            pointer_count: Rc::clone(&pointer_count),
        });
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(session_factory, window, cx));

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let first_row_height = cx
            .debug_bounds("pane-menu-row-split-right")
            .map(|bounds| bounds.size.height);
        let last_row_height = cx
            .debug_bounds("pane-menu-row-close-pane")
            .map(|bounds| bounds.size.height);
        let menu_height = cx
            .debug_bounds("pane-menu-2")
            .map(|bounds| bounds.size.height);

        assert_eq!(
            (first_row_height, last_row_height, menu_height),
            (Some(px(28.0)), Some(px(28.0)), Some(px(115.0)))
        );
    }

    #[test]
    fn menu_spec_should_use_restore_label_and_icon_when_zoomed() {
        let zoom_item = pane_menu_items(true)[2];

        assert_eq!(
            (zoom_item.label, zoom_item.icon),
            ("Restore Panes", "arrow.down.right.and.arrow.up.left")
        );
    }

    #[test]
    fn menu_spec_should_show_command_w_for_close_pane() {
        let close_item = pane_menu_items(false)[3];

        assert_eq!(close_item.shortcut, "⌘W");
    }

    #[test]
    fn split_ratio_should_follow_horizontal_pointer_position() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(401.0), px(200.0)));
        let pointer = point(px(110.0), px(80.0));

        assert_eq!(
            split_ratio_for_pointer(SplitAxis::Horizontal, split_bounds, pointer),
            Some(0.25)
        );
    }

    #[test]
    fn split_ratio_should_follow_vertical_pointer_position() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(400.0), px(201.0)));
        let pointer = point(px(110.0), px(70.0));

        assert_eq!(
            split_ratio_for_pointer(SplitAxis::Vertical, split_bounds, pointer),
            Some(0.25)
        );
    }

    #[test]
    fn minimum_pane_size_should_contain_the_complete_menu() {
        let menu_right = PANE_CONTROL_INSET + MENU_WIDTH;
        let menu_bottom = MENU_TOP + MENU_HEIGHT;

        assert!(
            MINIMUM_PANE_WIDTH >= menu_right + PANE_CONTROL_INSET
                && MINIMUM_PANE_HEIGHT >= menu_bottom + PANE_CONTROL_INSET
        );
    }
}
