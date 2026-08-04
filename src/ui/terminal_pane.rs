use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, DispatchPhase, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollWheelEvent, SharedString, Task, TextRun, Window, canvas, div, font, px, rgba,
};

use super::terminal_element::{TerminalGridCache, TerminalGridElement};
use super::{CopySelection, PasteClipboard, TERMINAL_KEY_CONTEXT};
use crate::terminal::{
    GridSize, InputModifiers, KeyCode, KeyInput, PointerButton, PointerInput, PointerPhase,
    ScreenSnapshot, ScrollbarSnapshot, SessionEvent, SurfacePosition, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput,
};
use crate::theme::{ACTIVE_THEME, Color};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
const PADDING: f32 = 12.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const SCROLLBAR_WIDTH: f32 = 5.0;
const SCROLLBAR_HORIZONTAL_HITBOX_PADDING: f32 = 4.0;
const SCROLLBAR_HITBOX_WIDTH: f32 = SCROLLBAR_WIDTH + SCROLLBAR_HORIZONTAL_HITBOX_PADDING * 2.0;
const SCROLLBAR_RIGHT_INSET: f32 = 4.0;
const MIN_SCROLLBAR_THUMB_HEIGHT: f32 = 24.0;
const SCROLLBAR_HIDE_DELAY: Duration = Duration::from_secs(2);
const MAX_PANE_TITLE_CHARACTERS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    TitleChanged(SharedString),
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarGeometry {
    top_px: f32,
    height_px: f32,
    track_height_px: f32,
}

#[derive(Clone, Copy, Debug)]
struct ScrollbarDrag {
    grab_ratio: f32,
    track_top_px: f32,
    track_height_px: f32,
    target_offset_rows: u64,
}

pub(crate) struct TerminalPane {
    session_factory: Rc<dyn TerminalSessionFactory>,
    working_directory: PathBuf,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    screen: Arc<ScreenSnapshot>,
    status: Option<String>,
    fallback_title: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    font_family: SharedString,
    cell_width: Pixels,
    last_grid_size: Option<GridSize>,
    grid_bounds: Option<Bounds<Pixels>>,
    pressed_button: Option<PointerButton>,
    wheel_remainder: f32,
    scrollbar_visible: bool,
    scrollbar_hovered: bool,
    scrollbar_drag: Option<ScrollbarDrag>,
    scrollbar_visibility_generation: u64,
    render_cache: TerminalGridCache,
    _event_task: Option<Task<()>>,
    _scrollbar_hide_task: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        working_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family);
        let fallback_title: SharedString =
            normalized_pane_title("", &session_factory.fallback_title()).into();

        Self {
            session_factory,
            working_directory,
            session: None,
            session_start_attempted: false,
            screen: ScreenSnapshot::empty(),
            status: None,
            title: fallback_title.clone(),
            fallback_title,
            focus_handle,
            font_family,
            cell_width,
            last_grid_size: None,
            grid_bounds: None,
            pressed_button: None,
            wheel_remainder: 0.0,
            scrollbar_visible: false,
            scrollbar_hovered: false,
            scrollbar_drag: None,
            scrollbar_visibility_generation: 0,
            render_cache: TerminalGridCache::new(),
            _event_task: None,
            _scrollbar_hide_task: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    #[cfg(test)]
    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    pub(crate) fn close(&mut self) {
        self._scrollbar_hide_task.take();
        self._event_task.take();
        close_session(&mut self.session);
    }

    fn update_grid_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.grid_bounds = Some(bounds);
        let size = grid_size(bounds, self.cell_width);
        if self.last_grid_size == Some(size) {
            return;
        }
        self.last_grid_size = Some(size);

        if let Some(session) = &self.session {
            session.resize(size);
            return;
        }

        if self.session_start_attempted {
            return;
        }
        self.session_start_attempted = true;

        match self.session_factory.start(size, &self.working_directory) {
            Ok(started) => {
                self.session = Some(started.handle);
                let receiver = started.events;
                self._event_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(event) = receiver.recv().await {
                        let mut events = vec![event];
                        while let Ok(event) = receiver.try_recv() {
                            events.push(event);
                        }
                        if this
                            .update(cx, |this, cx| {
                                for event in events {
                                    this.handle_event(event, cx);
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(error) => {
                eprintln!("failed to start terminal session: {error:#}");
                self.status = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn handle_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Screen(screen) => {
                let title = normalized_pane_title(&screen.title, &self.fallback_title);
                if self.title.as_ref() != title {
                    self.title = title.into();
                    cx.emit(TerminalPaneEvent::TitleChanged(self.title.clone()));
                }
                if screen.scrollbar.total_rows <= screen.scrollbar.visible_rows {
                    self.scrollbar_visible = false;
                    self.scrollbar_hovered = false;
                    self.scrollbar_drag = None;
                }
                self.screen = screen;
            }
            SessionEvent::Exited(status) => {
                eprintln!("{status}");
                self.status = Some(status);
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Error(status) => {
                eprintln!("{status}");
                self.status = Some(status);
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = encode_key(event) {
            if let Some(session) = &self.session {
                session.key(input);
            }
            cx.stop_propagation();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);
        let Some(button) = pointer_button(event.button) else {
            return;
        };
        if self.pressed_button.is_some() {
            cx.stop_propagation();
            return;
        }
        let Some(position) = self.surface_position(event.position, false) else {
            return;
        };

        self.pressed_button = Some(button);
        if let Some(session) = &self.session {
            session.pointer(PointerInput {
                phase: PointerPhase::Press,
                button: Some(button),
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrollbar_drag.is_some() {
            self.move_scrollbar_drag(event.position.y, cx);
            cx.stop_propagation();
            return;
        }
        let dragging = self.pressed_button.is_some();
        let Some(position) = self.surface_position(event.position, dragging) else {
            return;
        };
        if let Some(session) = &self.session {
            session.pointer(PointerInput {
                phase: PointerPhase::Motion,
                button: self.pressed_button,
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.scrollbar_drag.is_some() {
            self.finish_scrollbar_drag(cx);
            cx.stop_propagation();
            return;
        }
        let Some(button) = pointer_button(event.button) else {
            return;
        };
        if self.pressed_button != Some(button) {
            return;
        }
        self.pressed_button = None;
        let Some(position) = self.surface_position(event.position, true) else {
            return;
        };

        if let Some(session) = &self.session {
            session.pointer(PointerInput {
                phase: PointerPhase::Release,
                button: Some(button),
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.surface_position(event.position, false) else {
            return;
        };
        if self.screen.scrollbar.total_rows > self.screen.scrollbar.visible_rows {
            self.reveal_scrollbar(cx);
        }
        let lines = f32::from(event.delta.pixel_delta(px(LINE_HEIGHT)).y) / LINE_HEIGHT;
        let steps = accumulate_wheel_steps(&mut self.wheel_remainder, lines);

        if steps != 0
            && let Some(session) = &self.session
        {
            session.wheel(WheelInput {
                steps,
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn reveal_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_visible = true;
        self.scrollbar_visibility_generation = self.scrollbar_visibility_generation.wrapping_add(1);
        let generation = self.scrollbar_visibility_generation;
        self._scrollbar_hide_task.take();

        if self.scrollbar_drag.is_none() && !self.scrollbar_hovered {
            self._scrollbar_hide_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SCROLLBAR_HIDE_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    if this.scrollbar_visibility_generation == generation
                        && this.scrollbar_drag.is_none()
                        && !this.scrollbar_hovered
                    {
                        this.scrollbar_visible = false;
                        cx.notify();
                    }
                });
            }));
        }
        cx.notify();
    }

    fn set_scrollbar_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.scrollbar_hovered == hovered {
            return;
        }

        self.scrollbar_hovered = hovered;
        self.scrollbar_visibility_generation = self.scrollbar_visibility_generation.wrapping_add(1);
        self._scrollbar_hide_task.take();
        if hovered {
            self.scrollbar_visible = true;
            cx.notify();
        } else {
            self.reveal_scrollbar(cx);
        }
    }

    fn begin_scrollbar_drag(
        &mut self,
        pointer_y: Pixels,
        thumb_bounds: Bounds<Pixels>,
        geometry: ScrollbarGeometry,
        cx: &mut Context<Self>,
    ) {
        self.scrollbar_visibility_generation = self.scrollbar_visibility_generation.wrapping_add(1);
        self._scrollbar_hide_task.take();
        self.scrollbar_visible = true;
        self.scrollbar_drag = Some(ScrollbarDrag {
            grab_ratio: ((f32::from(pointer_y - thumb_bounds.origin.y) / geometry.height_px)
                .clamp(0.0, 1.0)),
            track_top_px: f32::from(thumb_bounds.origin.y) - geometry.top_px,
            track_height_px: geometry.track_height_px,
            target_offset_rows: self.screen.scrollbar.offset_rows,
        });
        cx.notify();
    }

    fn move_scrollbar_drag(&mut self, pointer_y: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.scrollbar_drag else {
            return;
        };
        let pointer_in_track_px = f32::from(pointer_y) - drag.track_top_px;
        let Some(offset_rows) = scrollbar_offset_for_pointer(
            self.screen.scrollbar,
            drag.track_height_px,
            pointer_in_track_px,
            drag.grab_ratio,
        ) else {
            return;
        };
        if offset_rows == drag.target_offset_rows {
            return;
        }

        if let Some(drag) = &mut self.scrollbar_drag {
            drag.target_offset_rows = offset_rows;
        }
        if let Some(session) = &self.session {
            session.scroll_to(offset_rows);
        }
        cx.notify();
    }

    fn finish_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            self.reveal_scrollbar(cx);
        }
    }

    fn copy_selection(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let receiver = session.request_selection_text();
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(Some(text))) if !text.is_empty() => {
                let _ = this.update(cx, |_this, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                });
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| {
                    eprintln!("failed to copy terminal selection: {error}");
                    this.status = Some(error);
                    cx.notify();
                });
            }
            Err(error) => {
                let _ = this.update(cx, |this, cx| {
                    let message = format!("terminal selection reply was lost: {error}");
                    eprintln!("{message}");
                    this.status = Some(message);
                    cx.notify();
                });
            }
            Ok(Ok(None | Some(_))) => {}
        })
        .detach();
    }

    fn paste_clipboard(
        &mut self,
        _: &PasteClipboard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if !text.is_empty()
            && let Some(session) = &self.session
        {
            session.paste(text);
        }
    }

    fn surface_position(
        &self,
        position: gpui::Point<Pixels>,
        allow_outside: bool,
    ) -> Option<SurfacePosition> {
        terminal_surface_position(
            self.grid_bounds?,
            position,
            self.cell_width,
            self.last_grid_size?,
            allow_outside,
        )
    }
}

impl EventEmitter<TerminalPaneEvent> for TerminalPane {}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.close();
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = cx.entity().downgrade();
        let scrollbar_pane = cx.entity();
        let background = gpui_color(self.screen.background);
        let status = self.status.clone();
        let scrollbar_dragging = self.scrollbar_drag.is_some();
        let mut scrollbar_state = self.screen.scrollbar;
        if let Some(drag) = self.scrollbar_drag {
            scrollbar_state.offset_rows = drag.target_offset_rows;
        }
        let scrollbar = self.scrollbar_visible.then(|| {
            self.last_grid_size.and_then(|size| {
                scrollbar_geometry(scrollbar_state, f32::from(size.rows) * LINE_HEIGHT)
            })
        });
        let scrollbar = scrollbar.flatten();
        let terminal_grid = TerminalGridElement::new(
            &self.screen,
            &self.font_family,
            px(FONT_SIZE),
            px(LINE_HEIGHT),
            self.cell_width,
            &mut self.render_cache,
        );

        div()
            .on_children_prepainted(move |children, _window, cx| {
                let Some(bounds) = children.first().copied() else {
                    return;
                };
                let _ = pane.update(cx, |pane, cx| pane.update_grid_bounds(bounds, cx));
            })
            .id("terminal-pane")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .p(px(PADDING))
            .cursor_text()
            .key_context(TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .child(terminal_grid)
            .when_some(scrollbar, move |root, scrollbar| {
                let hover_pane = scrollbar_pane.clone();
                let down_pane = scrollbar_pane.clone();
                let move_pane = scrollbar_pane.clone();
                let up_pane = scrollbar_pane.clone();
                let thumb_color = if scrollbar_dragging {
                    ACTIVE_THEME.icon
                } else {
                    ACTIVE_THEME.scrollbar_thumb_background
                };
                let thumb_hover_color = if scrollbar_dragging {
                    ACTIVE_THEME.icon
                } else {
                    ACTIVE_THEME.icon_accent
                };
                root.child(
                    div()
                        .group("terminal-scrollbar-thumb")
                        .id("terminal-scrollbar-thumb-hitbox")
                        .absolute()
                        .top(px(PADDING + scrollbar.top_px))
                        .right_0()
                        .w(px(SCROLLBAR_HITBOX_WIDTH))
                        .h(px(scrollbar.height_px))
                        .cursor_default()
                        .on_hover(move |hovered, _, cx| {
                            hover_pane.update(cx, |pane, cx| {
                                pane.set_scrollbar_hovered(*hovered, cx);
                            });
                        })
                        .child(
                            div()
                                .absolute()
                                .right(px(SCROLLBAR_RIGHT_INSET))
                                .w(px(SCROLLBAR_WIDTH))
                                .h_full()
                                .rounded(px(SCROLLBAR_WIDTH / 2.0))
                                .bg(gpui_color(thumb_color))
                                .group_hover("terminal-scrollbar-thumb", move |thumb| {
                                    thumb.bg(gpui_color(thumb_hover_color))
                                }),
                        )
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |thumb_bounds, _, window, _| {
                                    window.on_mouse_event(
                                        move |event: &MouseDownEvent, phase, window, cx| {
                                            if phase != DispatchPhase::Bubble
                                                || event.button != MouseButton::Left
                                                || !thumb_bounds.contains(&event.position)
                                            {
                                                return;
                                            }
                                            down_pane.read(cx).focus_handle.focus(window);
                                            down_pane.update(cx, |pane, cx| {
                                                pane.begin_scrollbar_drag(
                                                    event.position.y,
                                                    thumb_bounds,
                                                    scrollbar,
                                                    cx,
                                                );
                                            });
                                            cx.stop_propagation();
                                        },
                                    );

                                    window.on_mouse_event(
                                        move |event: &MouseMoveEvent, phase, _, cx| {
                                            if phase != DispatchPhase::Bubble
                                                || !event.dragging()
                                                || move_pane.read(cx).scrollbar_drag.is_none()
                                            {
                                                return;
                                            }
                                            move_pane.update(cx, |pane, cx| {
                                                pane.move_scrollbar_drag(event.position.y, cx);
                                            });
                                            cx.stop_propagation();
                                        },
                                    );

                                    window.on_mouse_event(
                                        move |event: &MouseUpEvent, phase, _, cx| {
                                            if phase != DispatchPhase::Bubble
                                                || event.button != MouseButton::Left
                                                || up_pane.read(cx).scrollbar_drag.is_none()
                                            {
                                                return;
                                            }
                                            up_pane.update(cx, |pane, cx| {
                                                pane.finish_scrollbar_drag(cx);
                                            });
                                            cx.stop_propagation();
                                        },
                                    );
                                },
                            )
                            .size_full(),
                        ),
                )
            })
            .when_some(status, |root, status| {
                root.child(
                    div()
                        .absolute()
                        .right(px(PADDING))
                        .bottom(px(PADDING))
                        .max_w(px(520.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(gpui_color(ACTIVE_THEME.element_active))
                        .text_color(gpui_color(ACTIVE_THEME.text))
                        .text_sm()
                        .child(status),
                )
            })
    }
}

fn terminal_font(cx: &App) -> SharedString {
    let font_names = cx.text_system().all_font_names();
    [
        "JetBrainsMono Nerd Font",
        "JetBrainsMono Nerd Font Mono",
        "JetBrains Mono",
        "Menlo",
    ]
    .into_iter()
    .find(|candidate| {
        font_names
            .iter()
            .any(|available| available.eq_ignore_ascii_case(candidate))
    })
    .unwrap_or("Menlo")
    .into()
}

fn measure_cell_width(window: &mut Window, family: &SharedString) -> Pixels {
    let run = TextRun {
        len: 1,
        font: font(family.clone()),
        color: gpui_color(ACTIVE_THEME.terminal_foreground).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line("M".into(), px(FONT_SIZE), &[run], None)
        .width
}

fn grid_size(bounds: Bounds<Pixels>, cell_width: Pixels) -> GridSize {
    let width = f32::from(bounds.size.width).max(f32::from(cell_width));
    let height = f32::from(bounds.size.height).max(LINE_HEIGHT);

    GridSize {
        cols: ((width / f32::from(cell_width)).floor() as u16).max(MIN_COLS),
        rows: ((height / LINE_HEIGHT).floor() as u16).max(MIN_ROWS),
        cell_width_px: f32::from(cell_width).round().clamp(1.0, u16::MAX as f32) as u16,
        cell_height_px: LINE_HEIGHT as u16,
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn normalized_pane_title(reported_title: &str, fallback_title: &str) -> String {
    let reported = reported_title
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PANE_TITLE_CHARACTERS)
        .collect::<String>();
    if !reported.is_empty() {
        return reported;
    }

    let fallback = fallback_title
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PANE_TITLE_CHARACTERS)
        .collect::<String>();
    if fallback.is_empty() {
        "Terminal".to_owned()
    } else {
        fallback
    }
}

fn close_session(session: &mut Option<Box<dyn TerminalSessionHandle>>) {
    session.take();
}

fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

fn input_modifiers(modifiers: gpui::Modifiers) -> InputModifiers {
    InputModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
        platform: modifiers.platform,
    }
}

fn scrollbar_geometry(
    scrollbar: ScrollbarSnapshot,
    track_height_px: f32,
) -> Option<ScrollbarGeometry> {
    if scrollbar.total_rows <= scrollbar.visible_rows || track_height_px <= 0.0 {
        return None;
    }

    let total_rows = scrollbar.total_rows as f64;
    let visible_rows = scrollbar.visible_rows as f64;
    let maximum_offset = scrollbar.total_rows.saturating_sub(scrollbar.visible_rows);
    if total_rows <= 0.0 || visible_rows <= 0.0 || maximum_offset == 0 {
        return None;
    }

    let track_height = f64::from(track_height_px);
    let minimum_height = f64::from(MIN_SCROLLBAR_THUMB_HEIGHT.min(track_height_px));
    let thumb_height =
        (visible_rows / total_rows * track_height).clamp(minimum_height, track_height);
    let progress = scrollbar.offset_rows.min(maximum_offset) as f64 / maximum_offset as f64;

    Some(ScrollbarGeometry {
        top_px: ((track_height - thumb_height) * progress) as f32,
        height_px: thumb_height as f32,
        track_height_px,
    })
}

fn scrollbar_offset_for_pointer(
    scrollbar: ScrollbarSnapshot,
    track_height_px: f32,
    pointer_in_track_px: f32,
    grab_ratio: f32,
) -> Option<u64> {
    let geometry = scrollbar_geometry(scrollbar, track_height_px)?;
    let movable_height = (track_height_px - geometry.height_px).max(0.0);
    if movable_height == 0.0 {
        return Some(0);
    }

    let thumb_top = (pointer_in_track_px - grab_ratio.clamp(0.0, 1.0) * geometry.height_px)
        .clamp(0.0, movable_height);
    let progress = thumb_top / movable_height;
    let maximum_offset = scrollbar.total_rows.saturating_sub(scrollbar.visible_rows);
    Some((f64::from(progress) * maximum_offset as f64).round() as u64)
}

fn terminal_surface_position(
    bounds: Bounds<Pixels>,
    position: gpui::Point<Pixels>,
    rendered_cell_width: Pixels,
    grid_size: GridSize,
    allow_outside: bool,
) -> Option<SurfacePosition> {
    if !allow_outside && !bounds.contains(&position) {
        return None;
    }

    let local_x = f32::from(position.x - bounds.origin.x);
    let local_y = f32::from(position.y - bounds.origin.y);
    Some(SurfacePosition {
        x: local_x * f32::from(grid_size.cell_width_px) / f32::from(rendered_cell_width),
        y: local_y * f32::from(grid_size.cell_height_px) / LINE_HEIGHT,
    })
}

fn accumulate_wheel_steps(remainder: &mut f32, delta: f32) -> i32 {
    *remainder += delta;
    let steps = remainder.trunc() as i32;
    *remainder -= steps as f32;
    steps
}

fn encode_key(event: &KeyDownEvent) -> Option<KeyInput> {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.platform || keystroke.modifiers.function {
        return None;
    }

    let code = match keystroke.key.as_str() {
        "enter" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "escape" => KeyCode::Escape,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "right" => KeyCode::ArrowRight,
        "left" => KeyCode::ArrowLeft,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        key => {
            let character = single_char(key).or_else(|| {
                keystroke
                    .key_char
                    .as_deref()
                    .and_then(|text| text.chars().next())
            })?;
            KeyCode::Character(character)
        }
    };

    Some(KeyInput {
        code,
        text: matches!(code, KeyCode::Character(_))
            .then(|| keystroke.key_char.clone())
            .flatten(),
        modifiers: input_modifiers(keystroke.modifiers),
    })
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Keystroke, Modifiers};

    use super::*;

    fn event(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                key: key.to_owned(),
                key_char: key_char.map(ToOwned::to_owned),
                modifiers,
            },
            is_held: false,
        }
    }

    #[test]
    fn reported_terminal_title_should_replace_the_shell_fallback() {
        assert_eq!(
            normalized_pane_title("  Claude Code  ", "zsh"),
            "Claude Code"
        );
    }

    #[test]
    fn empty_terminal_title_should_restore_the_shell_fallback() {
        assert_eq!(normalized_pane_title("\n\t", "zsh"), "zsh");
    }

    #[test]
    fn pane_title_should_remove_control_characters() {
        assert_eq!(
            normalized_pane_title("cargo\u{7} test", "zsh"),
            "cargo test"
        );
    }

    #[test]
    fn maps_printable_text_and_terminal_keys() {
        assert_eq!(
            encode_key(&event("a", Some("a"), Modifiers::default())),
            Some(KeyInput {
                code: KeyCode::Character('a'),
                text: Some("a".to_owned()),
                modifiers: InputModifiers::default(),
            })
        );
        assert_eq!(
            encode_key(&event("enter", None, Modifiers::default())),
            Some(KeyInput {
                code: KeyCode::Enter,
                text: None,
                modifiers: InputModifiers::default(),
            })
        );
        assert_eq!(
            encode_key(&event("up", None, Modifiers::default())),
            Some(KeyInput {
                code: KeyCode::ArrowUp,
                text: None,
                modifiers: InputModifiers::default(),
            })
        );
    }

    #[test]
    fn preserves_control_and_alt_for_worker_side_encoding() {
        let modifiers = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_key(&event("c", Some("c"), modifiers)),
            Some(KeyInput {
                code: KeyCode::Character('c'),
                text: Some("c".to_owned()),
                modifiers: InputModifiers {
                    control: true,
                    alt: true,
                    ..InputModifiers::default()
                },
            })
        );
    }

    #[test]
    fn control_d_should_be_sent_to_the_shell_as_eof_input() {
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::default()
        };

        assert_eq!(
            encode_key(&event("d", Some("d"), modifiers)),
            Some(KeyInput {
                code: KeyCode::Character('d'),
                text: Some("d".to_owned()),
                modifiers: InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            })
        );
    }

    #[test]
    fn leaves_command_shortcuts_for_the_application() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert!(encode_key(&event("q", None, modifiers)).is_none());
    }

    #[test]
    fn maps_rendered_positions_to_reported_terminal_geometry() {
        let bounds = Bounds::new(
            gpui::point(px(10.0), px(20.0)),
            gpui::size(px(75.0), px(40.0)),
        );
        let grid = GridSize {
            cols: 10,
            rows: 2,
            cell_width_px: 8,
            cell_height_px: 20,
        };

        let position = terminal_surface_position(
            bounds,
            gpui::point(px(47.5), px(30.0)),
            px(7.5),
            grid,
            false,
        )
        .unwrap();

        assert_eq!(position, SurfacePosition { x: 40.0, y: 10.0 });
        assert!(terminal_surface_position(
            bounds,
            gpui::point(px(9.0), px(20.0)),
            px(7.5),
            grid,
            false,
        )
        .is_none());
    }

    #[test]
    fn calculates_proportional_scrollbar_geometry_across_the_track() {
        let top = scrollbar_geometry(
            ScrollbarSnapshot {
                total_rows: 100,
                offset_rows: 0,
                visible_rows: 20,
            },
            200.0,
        )
        .unwrap();
        assert_eq!(
            top,
            ScrollbarGeometry {
                top_px: 0.0,
                height_px: 40.0,
                track_height_px: 200.0,
            }
        );

        let middle = scrollbar_geometry(
            ScrollbarSnapshot {
                total_rows: 100,
                offset_rows: 40,
                visible_rows: 20,
            },
            200.0,
        )
        .unwrap();
        assert_eq!(
            middle,
            ScrollbarGeometry {
                top_px: 80.0,
                height_px: 40.0,
                track_height_px: 200.0,
            }
        );

        assert_eq!(
            scrollbar_geometry(
                ScrollbarSnapshot {
                    total_rows: 100,
                    offset_rows: 80,
                    visible_rows: 20,
                },
                200.0,
            )
            .unwrap()
            .top_px,
            160.0
        );
        assert!(
            scrollbar_geometry(
                ScrollbarSnapshot {
                    total_rows: 20,
                    offset_rows: 0,
                    visible_rows: 20,
                },
                200.0,
            )
            .is_none()
        );
    }

    #[test]
    fn enforces_a_minimum_scrollbar_thumb_height() {
        let geometry = scrollbar_geometry(
            ScrollbarSnapshot {
                total_rows: 10_000,
                offset_rows: 4_000,
                visible_rows: 20,
            },
            200.0,
        )
        .unwrap();

        assert_eq!(geometry.height_px, MIN_SCROLLBAR_THUMB_HEIGHT);
        assert!(geometry.top_px > 0.0);
    }

    #[test]
    fn maps_thumb_drag_positions_to_absolute_scrollback_offsets() {
        let scrollbar = ScrollbarSnapshot {
            total_rows: 100,
            offset_rows: 0,
            visible_rows: 20,
        };

        assert_eq!(
            scrollbar_offset_for_pointer(scrollbar, 200.0, 20.0, 0.5),
            Some(0)
        );
        assert_eq!(
            scrollbar_offset_for_pointer(scrollbar, 200.0, 100.0, 0.5),
            Some(40)
        );
        assert_eq!(
            scrollbar_offset_for_pointer(scrollbar, 200.0, 180.0, 0.5),
            Some(80)
        );
        assert_eq!(
            scrollbar_offset_for_pointer(scrollbar, 200.0, -100.0, 0.5),
            Some(0)
        );
        assert_eq!(
            scrollbar_offset_for_pointer(scrollbar, 200.0, 500.0, 0.5),
            Some(80)
        );
    }

    #[test]
    fn accumulates_fractional_trackpad_scroll_into_terminal_steps() {
        let mut remainder = 0.0;

        assert_eq!(accumulate_wheel_steps(&mut remainder, 0.4), 0);
        assert_eq!(accumulate_wheel_steps(&mut remainder, 0.7), 1);
        assert!((remainder - 0.1).abs() < f32::EPSILON * 4.0);
        assert_eq!(accumulate_wheel_steps(&mut remainder, -1.3), -1);
        assert!((remainder + 0.2).abs() < f32::EPSILON * 4.0);
    }

    #[test]
    fn maps_gpui_buttons_and_modifiers_to_terminal_input() {
        assert_eq!(pointer_button(MouseButton::Left), Some(PointerButton::Left));
        assert_eq!(
            pointer_button(MouseButton::Navigate(gpui::NavigationDirection::Back)),
            None
        );
        assert_eq!(
            input_modifiers(Modifiers {
                control: true,
                alt: true,
                shift: true,
                platform: true,
                function: true,
            }),
            InputModifiers {
                shift: true,
                alt: true,
                control: true,
                platform: true,
            }
        );
    }

    struct DropCountingSession {
        drop_count: Rc<Cell<usize>>,
    }

    impl Drop for DropCountingSession {
        fn drop(&mut self) {
            self.drop_count.set(self.drop_count.get() + 1);
        }
    }

    impl TerminalSessionHandle for DropCountingSession {
        fn key(&self, _: KeyInput) {}

        fn resize(&self, _: GridSize) {}

        fn pointer(&self, _: PointerInput) {}

        fn wheel(&self, _: WheelInput) {}

        fn scroll_to(&self, _: u64) {}

        fn paste(&self, _: String) {}

        fn request_selection_text(
            &self,
        ) -> async_channel::Receiver<Result<Option<String>, String>> {
            let (_, receiver) = async_channel::bounded(1);
            receiver
        }
    }

    #[test]
    fn close_session_should_drop_a_handle_exactly_once_when_repeated() {
        let drop_count = Rc::new(Cell::new(0));
        let mut session: Option<Box<dyn TerminalSessionHandle>> =
            Some(Box::new(DropCountingSession {
                drop_count: Rc::clone(&drop_count),
            }));

        close_session(&mut session);
        close_session(&mut session);

        assert_eq!(drop_count.get(), 1);
    }
}
