use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollWheelEvent, SharedString, Task, TextRun, Window, div, font, px, rgba,
};

use super::overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics};
use super::terminal_element::{
    TerminalGridCache, TerminalGridElement, terminal_grid_content_bounds,
};
use super::{
    CopySelection, DecreaseTerminalFontSize, IncreaseTerminalFontSize, PasteClipboard,
    ResetTerminalFontSize, TERMINAL_KEY_CONTEXT,
};
use crate::terminal::geometry::{
    BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize, TerminalGeometry,
};
use crate::terminal::{
    InputModifiers, KeyAction, KeyInput, PhysicalKey, PointerButton, PointerInput, PointerPhase,
    ScreenSnapshot, SessionEvent, SurfacePosition, TerminalSessionHandle, WheelInput,
    WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_LINE_HEIGHT: f32 = 20.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const FONT_SIZE_STEP: f32 = 1.0;
const HORIZONTAL_PADDING: f32 = 4.0;
const VERTICAL_PADDING: f32 = 8.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_PANE_TITLE_CHARACTERS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    FocusRequested,
    TitleChanged(SharedString),
    Exited,
}

pub(crate) struct TerminalPane {
    session_factory: WorkspaceTerminalSessionFactory,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    screen: Arc<ScreenSnapshot>,
    status: Option<String>,
    fallback_title: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    cell_width: Pixels,
    backing_scale: BackingScale,
    last_geometry: Option<TerminalGeometry>,
    grid_bounds: Option<Bounds<Pixels>>,
    pressed_button: Option<PointerButton>,
    wheel_remainder: f32,
    scrollbar: Entity<OverlayScrollbar<u64>>,
    render_cache: TerminalGridCache,
    _event_task: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family, DEFAULT_FONT_SIZE);
        let backing_scale = BackingScale::new(window.scale_factor()).unwrap_or(BackingScale::ONE);
        let fallback_title: SharedString =
            normalized_pane_title("", &session_factory.fallback_title()).into();
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("terminal-scrollbar"));
        cx.subscribe_in(
            &scrollbar,
            window,
            |pane, _, event: &OverlayScrollbarEvent<u64>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    pane.focus_handle.focus(window);
                    cx.emit(TerminalPaneEvent::FocusRequested);
                }
                OverlayScrollbarEvent::OffsetRequested(rows) => {
                    if let Some(session) = &pane.session {
                        session.scroll_to(*rows);
                    }
                }
            },
        )
        .detach();
        cx.observe_window_bounds(window, |pane, window, cx| {
            pane.update_backing_scale(window.scale_factor(), window, cx);
        })
        .detach();

        Self {
            session_factory,
            session: None,
            session_start_attempted: false,
            screen: ScreenSnapshot::empty(),
            status: None,
            title: fallback_title.clone(),
            fallback_title,
            focus_handle,
            font_family,
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            cell_width,
            backing_scale,
            last_geometry: None,
            grid_bounds: None,
            pressed_button: None,
            wheel_remainder: 0.0,
            scrollbar,
            render_cache: TerminalGridCache::new(),
            _event_task: None,
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

    #[cfg(test)]
    pub(crate) const fn font_size(&self) -> f32 {
        self.font_size
    }

    pub(crate) fn close(&mut self) {
        self._event_task.take();
        self.session.take();
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<u64>> {
        let size = self.last_geometry?.grid();
        ScrollMetrics::for_rows(
            VERTICAL_PADDING,
            f32::from(size.rows) * self.line_height,
            self.screen.scrollbar.total_rows,
            self.screen.scrollbar.visible_rows,
            self.screen.scrollbar.offset_rows,
        )
    }

    fn sync_scrollbar(&self, cx: &mut Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn reveal_scrollbar(&self, cx: &mut Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }

    fn update_grid_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let geometry = terminal_geometry(
            bounds,
            self.cell_width,
            self.line_height,
            self.backing_scale,
        );
        self.grid_bounds = Some(terminal_grid_content_bounds(
            bounds,
            usize::from(geometry.grid().cols),
            self.cell_width,
        ));
        if self.last_geometry == Some(geometry) {
            return;
        }
        self.last_geometry = Some(geometry);
        self.sync_scrollbar(cx);

        if let Some(session) = &self.session {
            session.resize(geometry);
            return;
        }

        if self.session_start_attempted {
            return;
        }
        self.session_start_attempted = true;

        match self.session_factory.start(geometry) {
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
                self.screen = screen;
                self.sync_scrollbar(cx);
            }
            SessionEvent::Exited(status) => {
                eprintln!("{status}");
                self.status = Some(status.to_string());
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Failed(failure) => {
                let status = failure.to_string();
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

    fn increase_font_size(
        &mut self,
        _: &IncreaseTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(self.font_size + FONT_SIZE_STEP, window, cx);
    }

    fn decrease_font_size(
        &mut self,
        _: &DecreaseTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(self.font_size - FONT_SIZE_STEP, window, cx);
    }

    fn reset_font_size(
        &mut self,
        _: &ResetTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(DEFAULT_FONT_SIZE, window, cx);
    }

    fn set_font_size(&mut self, font_size: f32, window: &mut Window, cx: &mut Context<Self>) {
        let font_size = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.font_size == font_size {
            return;
        }

        self.font_size = font_size;
        self.line_height = line_height_for_font_size(font_size);
        self.cell_width = measure_cell_width(window, &self.font_family, font_size);
        self.last_geometry = None;
        self.sync_scrollbar(cx);
        cx.notify();
    }

    fn update_backing_scale(&mut self, factor: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backing_scale) = BackingScale::new(factor) else {
            eprintln!("ignored invalid terminal backing scale: {factor}");
            return;
        };
        if self.backing_scale == backing_scale {
            return;
        }

        self.backing_scale = backing_scale;
        self.cell_width = measure_cell_width(window, &self.font_family, self.font_size);
        self.last_geometry = None;
        self.render_cache.invalidate_scale_dependent();
        self.sync_scrollbar(cx);
        cx.notify();
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
        self.reveal_scrollbar(cx);
        let lines = f32::from(event.delta.pixel_delta(px(self.line_height)).y) / self.line_height;
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
            self.last_geometry?,
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
        let background = gpui_color(self.screen.background);
        let status = self.status.clone();
        self.sync_scrollbar(cx);
        let scrollbar = self.scrollbar.clone();
        let terminal_grid = TerminalGridElement::new(
            &self.screen,
            &self.font_family,
            px(self.font_size),
            px(self.line_height),
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
            .px(px(HORIZONTAL_PADDING))
            .py(px(VERTICAL_PADDING))
            .cursor_text()
            .key_context(TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_action(cx.listener(Self::reset_font_size))
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
            .child(scrollbar)
            .when_some(status, |root, status| {
                root.child(
                    div()
                        .debug_selector(|| "terminal-status".to_owned())
                        .absolute()
                        .right(px(HORIZONTAL_PADDING))
                        .bottom(px(VERTICAL_PADDING))
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

fn measure_cell_width(window: &mut Window, family: &SharedString, font_size: f32) -> Pixels {
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
        .shape_line("M".into(), px(font_size), &[run], None)
        .width
}

fn line_height_for_font_size(font_size: f32) -> f32 {
    font_size * DEFAULT_LINE_HEIGHT / DEFAULT_FONT_SIZE
}

fn terminal_geometry(
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: f32,
    backing_scale: BackingScale,
) -> TerminalGeometry {
    let width = f32::from(bounds.size.width).max(f32::from(cell_width));
    let height = f32::from(bounds.size.height).max(line_height);

    TerminalGeometry::from_viewport(
        LogicalSize::new(width, height),
        LogicalCellSize::new(f32::from(cell_width), line_height),
        backing_scale,
        CellGridSize::new(MIN_COLS, MIN_ROWS),
    )
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
        ..InputModifiers::default()
    }
}

fn terminal_surface_position(
    bounds: Bounds<Pixels>,
    position: gpui::Point<Pixels>,
    geometry: TerminalGeometry,
    allow_outside: bool,
) -> Option<SurfacePosition> {
    if !allow_outside && !bounds.contains(&position) {
        return None;
    }

    let local_x = f32::from(position.x - bounds.origin.x);
    let local_y = f32::from(position.y - bounds.origin.y);
    let backing = geometry.to_backing_position(LogicalPosition::new(local_x, local_y));
    Some(SurfacePosition {
        x: backing.x,
        y: backing.y,
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
    let physical_key = physical_key(&keystroke.key);
    let text = keystroke.key_char.clone().filter(|text| {
        !text.is_empty() && !text.chars().any(char::is_control)
    });
    let unshifted_codepoint = single_char(&keystroke.key).map(unshifted_character);

    Some(KeyInput {
        action: if event.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        },
        physical_key,
        native_key_code: None,
        logical_key: keystroke.key.clone(),
        text,
        unshifted_codepoint,
        modifiers: input_modifiers(keystroke.modifiers),
        consumed_modifiers: InputModifiers::default(),
    })
}

fn physical_key(key: &str) -> PhysicalKey {
    match key {
        "enter" => PhysicalKey::Enter,
        "backspace" => PhysicalKey::Backspace,
        "tab" => PhysicalKey::Tab,
        "escape" => PhysicalKey::Escape,
        "up" => PhysicalKey::ArrowUp,
        "down" => PhysicalKey::ArrowDown,
        "right" => PhysicalKey::ArrowRight,
        "left" => PhysicalKey::ArrowLeft,
        "home" => PhysicalKey::Home,
        "end" => PhysicalKey::End,
        "pageup" => PhysicalKey::PageUp,
        "pagedown" => PhysicalKey::PageDown,
        "insert" => PhysicalKey::Insert,
        "delete" => PhysicalKey::Delete,
        "f1" => PhysicalKey::F1,
        "f2" => PhysicalKey::F2,
        "f3" => PhysicalKey::F3,
        "f4" => PhysicalKey::F4,
        "f5" => PhysicalKey::F5,
        "f6" => PhysicalKey::F6,
        "f7" => PhysicalKey::F7,
        "f8" => PhysicalKey::F8,
        "f9" => PhysicalKey::F9,
        "f10" => PhysicalKey::F10,
        "f11" => PhysicalKey::F11,
        "f12" => PhysicalKey::F12,
        "f13" => PhysicalKey::F13,
        "f14" => PhysicalKey::F14,
        "f15" => PhysicalKey::F15,
        "f16" => PhysicalKey::F16,
        "f17" => PhysicalKey::F17,
        "f18" => PhysicalKey::F18,
        "f19" => PhysicalKey::F19,
        "f20" => PhysicalKey::F20,
        "f21" => PhysicalKey::F21,
        "f22" => PhysicalKey::F22,
        "f23" => PhysicalKey::F23,
        "f24" => PhysicalKey::F24,
        "f25" => PhysicalKey::F25,
        "space" | " " => PhysicalKey::Space,
        value => single_char(value)
            .map(physical_character_key)
            .unwrap_or(PhysicalKey::Unidentified),
    }
}

fn physical_character_key(character: char) -> PhysicalKey {
    match unshifted_character(character) {
        '`' => PhysicalKey::Backquote,
        '\\' => PhysicalKey::Backslash,
        '[' => PhysicalKey::BracketLeft,
        ']' => PhysicalKey::BracketRight,
        ',' => PhysicalKey::Comma,
        '0' => PhysicalKey::Digit0,
        '1' => PhysicalKey::Digit1,
        '2' => PhysicalKey::Digit2,
        '3' => PhysicalKey::Digit3,
        '4' => PhysicalKey::Digit4,
        '5' => PhysicalKey::Digit5,
        '6' => PhysicalKey::Digit6,
        '7' => PhysicalKey::Digit7,
        '8' => PhysicalKey::Digit8,
        '9' => PhysicalKey::Digit9,
        '=' => PhysicalKey::Equal,
        'a' => PhysicalKey::A,
        'b' => PhysicalKey::B,
        'c' => PhysicalKey::C,
        'd' => PhysicalKey::D,
        'e' => PhysicalKey::E,
        'f' => PhysicalKey::F,
        'g' => PhysicalKey::G,
        'h' => PhysicalKey::H,
        'i' => PhysicalKey::I,
        'j' => PhysicalKey::J,
        'k' => PhysicalKey::K,
        'l' => PhysicalKey::L,
        'm' => PhysicalKey::M,
        'n' => PhysicalKey::N,
        'o' => PhysicalKey::O,
        'p' => PhysicalKey::P,
        'q' => PhysicalKey::Q,
        'r' => PhysicalKey::R,
        's' => PhysicalKey::S,
        't' => PhysicalKey::T,
        'u' => PhysicalKey::U,
        'v' => PhysicalKey::V,
        'w' => PhysicalKey::W,
        'x' => PhysicalKey::X,
        'y' => PhysicalKey::Y,
        'z' => PhysicalKey::Z,
        '-' => PhysicalKey::Minus,
        '.' => PhysicalKey::Period,
        '\'' => PhysicalKey::Quote,
        ';' => PhysicalKey::Semicolon,
        '/' => PhysicalKey::Slash,
        ' ' => PhysicalKey::Space,
        _ => PhysicalKey::Unidentified,
    }
}

fn unshifted_character(character: char) -> char {
    match character {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        character if character.is_ascii_uppercase() => character.to_ascii_lowercase(),
        character => character,
    }
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use gpui::{Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{SessionFailure, TerminalSessionFactory};

    fn terminal_pane(cx: &mut TestAppContext) -> (Entity<TerminalPane>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                .with_start_failure("terminal session unavailable in UI test"),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx)
    }

    #[gpui::test]
    fn command_equals_should_increase_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-=");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (14.0, 15.0));
    }

    #[gpui::test]
    fn command_minus_should_decrease_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd--");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (14.0, 13.0));
    }

    #[gpui::test]
    fn command_zero_should_reset_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| pane.set_font_size(20.0, window, cx));
        });
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-0");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (20.0, DEFAULT_FONT_SIZE));
    }

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

    fn expected_key(
        physical_key: PhysicalKey,
        logical_key: &str,
        text: Option<&str>,
        modifiers: InputModifiers,
    ) -> KeyInput {
        KeyInput {
            action: KeyAction::Press,
            physical_key,
            native_key_code: None,
            logical_key: logical_key.to_owned(),
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: single_char(logical_key).map(unshifted_character),
            modifiers,
            consumed_modifiers: InputModifiers::default(),
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
            Some(expected_key(
                PhysicalKey::A,
                "a",
                Some("a"),
                InputModifiers::default(),
            ))
        );
        assert_eq!(
            encode_key(&event("enter", None, Modifiers::default())),
            Some(expected_key(
                PhysicalKey::Enter,
                "enter",
                None,
                InputModifiers::default(),
            ))
        );
        assert_eq!(
            encode_key(&event("up", None, Modifiers::default())),
            Some(expected_key(
                PhysicalKey::ArrowUp,
                "up",
                None,
                InputModifiers::default(),
            ))
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
            Some(expected_key(
                PhysicalKey::C,
                "c",
                Some("c"),
                InputModifiers {
                    control: true,
                    alt: true,
                    ..InputModifiers::default()
                },
            ))
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
            Some(expected_key(
                PhysicalKey::D,
                "d",
                Some("d"),
                InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            ))
        );
    }

    #[test]
    fn preserves_unbound_command_keys_for_terminal_protocol_encoding() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_key(&event("q", None, modifiers)),
            Some(expected_key(
                PhysicalKey::Q,
                "q",
                None,
                InputModifiers {
                    platform: true,
                    ..InputModifiers::default()
                },
            ))
        );
    }

    #[test]
    fn maps_rendered_positions_to_reported_terminal_geometry() {
        let bounds = Bounds::new(
            gpui::point(px(10.0), px(20.0)),
            gpui::size(px(75.0), px(40.0)),
        );
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::ONE,
        );

        let position =
            terminal_surface_position(bounds, gpui::point(px(47.5), px(30.0)), geometry, false)
                .unwrap();

        assert_eq!(position, SurfacePosition { x: 37.5, y: 10.0 });
        assert!(
            terminal_surface_position(bounds, gpui::point(px(9.0), px(20.0)), geometry, false,)
                .is_none()
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
                ..InputModifiers::default()
            }
        );
    }

    #[gpui::test]
    fn terminal_scrollbar_should_request_exact_row_offsets(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let records = TestTerminalSessionRecords::default();
        let session = TestTerminalSessionFactory::new(records.clone())
            .start(
                TerminalGeometry::from_grid(
                    CellGridSize::new(80, 24),
                    LogicalCellSize::new(8.0, 16.0),
                    BackingScale::ONE,
                ),
                std::path::Path::new("/tmp/spaceterm-terminal-pane-test"),
            )
            .expect("the test terminal session should start");
        let scrollbar = pane.update(cx, |pane, _| {
            pane.session = Some(session.handle);
            pane.scrollbar.clone()
        });

        scrollbar.update(cx, |_, cx| {
            cx.emit(OverlayScrollbarEvent::OffsetRequested(u64::MAX - 1));
        });
        cx.run_until_parked();

        assert_eq!(
            records.commands().last().map(|input| &input.command),
            Some(&RecordedSessionCommand::ScrollTo(u64::MAX - 1))
        );
    }

    #[gpui::test]
    fn backing_scale_change_should_preserve_the_grid_and_resize_backing_pixels(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-scale-test"),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.run_until_parked();
        let initial = records.starts()[0].geometry;

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.update_backing_scale(1.0, window, cx);
            });
        });
        cx.run_until_parked();
        let resized = records
            .commands()
            .into_iter()
            .find_map(|call| match call.command {
                RecordedSessionCommand::Resize(geometry) => Some(geometry),
                _ => None,
            })
            .expect("a backing-scale change should resize the Terminal Session");

        assert_eq!(
            (
                initial.grid(),
                resized.grid(),
                initial.backing_grid_size().width,
                resized.backing_grid_size().width,
            ),
            (
                initial.grid(),
                initial.grid(),
                resized.backing_grid_size().width * 2,
                resized.backing_grid_size().width,
            )
        );
    }

    #[gpui::test]
    fn terminal_pane_close_should_drop_its_session_once_when_repeated(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.run_until_parked();

        pane.update(cx, |pane, _| {
            pane.close();
            pane.close();
        });

        assert_eq!(records.dropped_session_ids(), vec![1]);
    }

    #[gpui::test]
    fn terminal_failure_should_keep_the_pane_visible_with_a_failure_status(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        let exits = Rc::new(Cell::new(0));
        let exits_for_subscription = Rc::clone(&exits);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::Exited) {
                    exits_for_subscription.update(|exits| exits + 1);
                }
            })
            .detach();
        });
        cx.run_until_parked();
        let sender = records
            .event_sender(1)
            .expect("rendering the Pane must start its Terminal Session");

        sender
            .try_send(SessionEvent::Failed(SessionFailure::PtyRead {
                read_error: "read unavailable".to_owned(),
                exit_status: "exit code 7".to_owned(),
            }))
            .unwrap();
        cx.run_until_parked();

        let state = pane.read_with(cx, |pane, _| (pane.status.clone(), pane.session.is_some()));
        assert_eq!(
            state,
            (
                Some(
                    "Shell output failed: read unavailable; shell exited (exit code 7)".to_owned()
                ),
                true,
            )
        );
        assert_eq!(
            (exits.get(), records.dropped_session_ids()),
            (0, Vec::new())
        );
        assert!(cx.debug_bounds("terminal-status").is_some());
    }
}
